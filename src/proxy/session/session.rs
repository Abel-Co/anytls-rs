use crate::proxy::padding::PaddingFactory;
use crate::proxy::session::frame::{Frame, CMD_FIN, CMD_PSH, CMD_SETTINGS, CMD_SYN, CMD_SYNACK, CMD_ALERT, CMD_UPDATE_PADDING_SCHEME, CMD_HEART_REQUEST, CMD_HEART_RESPONSE, CMD_SERVER_SETTINGS, CMD_WASTE, HEADER_OVERHEAD_SIZE};
use crate::proxy::session::stream::Stream;
use crate::util::string_map::{StringMap, StringMapExt};
use bytes::{BufMut, Bytes, BytesMut};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// 使用 util 中定义的 trait
use crate::util::r#type::AsyncReadWrite;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};

/// Session 管理多个 Stream 的连接复用
pub struct Session {
    conn: Arc<Mutex<Box<dyn AsyncReadWrite>>>,
    
    // Stream 管理
    streams: Arc<RwLock<HashMap<u32, mpsc::Sender<Bytes>>>>,
    next_stream_id: AtomicU32,
    
    // 状态管理
    closed: AtomicBool,
    is_client: bool,
    
    // 设置相关
    settings_sent: AtomicBool,
    peer_version: AtomicU32,
    
    // 填充相关
    padding: Arc<PaddingFactory>,
    pkt_counter: AtomicU32,
    send_padding: AtomicBool,
    
    // 缓冲相关
    buffering: AtomicBool,
    buffer: Arc<Mutex<BytesMut>>,
    
    // 数据通道
    data_tx: mpsc::UnboundedSender<(u32, Bytes)>,
    
    // Session ID
    session_id: AtomicU64,
}

impl Session {
    /// 创建客户端 Session
    pub fn new_client(
        conn: Box<dyn AsyncReadWrite>,
        padding: Arc<PaddingFactory>,
    ) -> Self {
        let (data_tx, _data_rx) = mpsc::unbounded_channel();
        
        Self {
            conn: Arc::new(Mutex::new(conn)),
            streams: Arc::new(RwLock::new(HashMap::new())),
            next_stream_id: AtomicU32::new(1),
            closed: AtomicBool::new(false),
            is_client: true,
            settings_sent: AtomicBool::new(false),
            peer_version: AtomicU32::new(0),
            padding,
            pkt_counter: AtomicU32::new(0),
            send_padding: AtomicBool::new(true),
            buffering: AtomicBool::new(false),
            buffer: Arc::new(Mutex::new(BytesMut::new())),
            data_tx,
            session_id: AtomicU64::new(0),
        }
    }

    /// 创建服务端 Session
    pub fn new_server(
        conn: Box<dyn AsyncReadWrite>,
        padding: Arc<PaddingFactory>,
    ) -> Self {
        let (data_tx, _data_rx) = mpsc::unbounded_channel();
        
        Self {
            conn: Arc::new(Mutex::new(conn)),
            streams: Arc::new(RwLock::new(HashMap::new())),
            next_stream_id: AtomicU32::new(1),
            closed: AtomicBool::new(false),
            is_client: false,
            settings_sent: AtomicBool::new(false),
            peer_version: AtomicU32::new(0),
            padding,
            pkt_counter: AtomicU32::new(0),
            send_padding: AtomicBool::new(false),
            buffering: AtomicBool::new(false),
            buffer: Arc::new(Mutex::new(BytesMut::new())),
            data_tx,
            session_id: AtomicU64::new(0),
        }
    }

    /// 启动 Session
    pub async fn run(&self) -> io::Result<()> {
        log::info!("[Session] Starting session (client: {})", self.is_client);
        
        if self.is_client {
            // 客户端发送设置
            log::debug!("[Session] Sending client settings");
            self.send_client_settings().await?;
            log::info!("[Session] Client settings sent");
        }

        // 直接运行接收循环
        log::debug!("[Session] Starting receive loop");
        self.recv_loop().await
    }

    /// 发送客户端设置
    async fn send_client_settings(&self) -> io::Result<()> {
        if self.settings_sent.swap(true, Ordering::AcqRel) {
            log::debug!("[Session] Settings already sent, skipping");
            return Ok(());
        }

        let settings = StringMap::from([
            ("v".to_string(), "2".to_string()),
            ("client".to_string(), crate::PROGRAM_VERSION_NAME.to_string()),
            ("padding-md5".to_string(), self.padding.md5().to_string()),
        ]);

        log::debug!("[Session] Client settings: {:?}", settings);
        let frame = Frame::with_data(CMD_SETTINGS, 0, Bytes::from(settings.to_bytes()));
        self.write_control_frame(frame).await?;
        log::debug!("[Session] Client settings frame sent");
        
        self.buffering.store(true, Ordering::Release);
        log::debug!("[Session] Buffering enabled");
        Ok(())
    }

    /// 打开新的 Stream
    pub async fn open_stream(&self) -> io::Result<Stream> {
        if self.is_closed() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Session closed"));
        }

        let stream_id = self.next_stream_id.fetch_add(1, Ordering::AcqRel);
        
        // 创建数据通道
        let (data_tx, data_rx) = mpsc::channel(100);
        let (frame_tx, mut frame_rx) = mpsc::channel(100);
        let (close_tx, _close_rx) = oneshot::channel();
        
        // 创建 Stream
        let stream = Stream::new(stream_id, data_rx, frame_tx, close_tx);

        // 注册 Stream 到 Session
        {
            let mut streams = self.streams.write().await;
            streams.insert(stream_id, data_tx);
        }

        // 发送 SYN 帧
        let frame = Frame::new(CMD_SYN, stream_id);
        self.write_control_frame(frame).await?;
        log::debug!("[Session] SYN frame sent for stream {}", stream_id);

        // 启动帧处理任务
        let session_clone = Arc::new(self.clone());
        tokio::spawn(async move {
            while let Some(frame) = frame_rx.recv().await {
                if let Err(e) = session_clone.write_frame(frame).await {
                    log::error!("Failed to write frame: {}", e);
                    break;
                }
            }
        });

        // 停止缓冲
        self.buffering.store(false, Ordering::Release);

        Ok(stream)
    }

    /// 关闭指定的 Stream
    pub async fn close_stream(&self, stream_id: u32) -> io::Result<()> {
        // 从 streams 中移除
        {
            let mut streams = self.streams.write().await;
            streams.remove(&stream_id);
        }

        // 发送 FIN 帧
        let frame = Frame::new(CMD_FIN, stream_id);
        self.write_control_frame(frame).await?;

        Ok(())
    }

    /// 写入数据帧
    pub async fn write_data_frame(&self, stream_id: u32, data: &[u8]) -> io::Result<usize> {
        let frame = Frame::with_data(CMD_PSH, stream_id, Bytes::copy_from_slice(data));
        self.write_frame(frame).await
    }

    /// 写入控制帧
    async fn write_control_frame(&self, frame: Frame) -> io::Result<usize> {
        self.write_frame(frame).await
    }

    /// 写入帧
    async fn write_frame(&self, frame: Frame) -> io::Result<usize> {
        let data = frame.to_bytes();
        self.write_conn(&data).await
    }

    /// 写入连接
    async fn write_conn(&self, data: &[u8]) -> io::Result<usize> {
        let mut conn = self.conn.lock().await;
        
        log::debug!("[Session] Writing {} bytes to connection", data.len());
        
        // 如果正在缓冲，添加到缓冲区
        if self.buffering.load(Ordering::Acquire) {
            let mut buffer = self.buffer.lock().await;
            buffer.extend_from_slice(data);
            log::debug!("[Session] Buffered {} bytes (total buffer size: {})", data.len(), buffer.len());
            return Ok(data.len());
        }

        // 处理填充
        if self.send_padding.load(Ordering::Acquire) {
            log::debug!("[Session] Writing with padding");
            self.write_with_padding(&mut *conn, data).await
        } else {
            log::debug!("[Session] Writing without padding");
            conn.write_all(data).await?;
            Ok(data.len())
        }
    }

    /// 带填充的写入
    async fn write_with_padding(&self, conn: &mut dyn AsyncReadWrite, data: &[u8]) -> io::Result<usize> {
        let pkt = self.pkt_counter.fetch_add(1, Ordering::AcqRel);
        
        if pkt < self.padding.stop() {
            let pkt_sizes = self.padding.generate_record_payload_sizes(pkt);
            let mut remaining = data;
            let mut total_written = 0;

            for size in pkt_sizes {
                if size == crate::proxy::padding::CHECK_MARK {
                    if remaining.is_empty() {
                        break;
                    } else {
                        continue;
                    }
                }

                let size = size as usize;
                if remaining.len() > size {
                    // 这个包全是有效载荷
                    conn.write_all(&remaining[..size]).await?;
                    total_written += size;
                    remaining = &remaining[size..];
                } else if !remaining.is_empty() {
                    // 这个包包含填充和有效载荷的最后部分
                    let padding_len = size.saturating_sub(remaining.len() + HEADER_OVERHEAD_SIZE);
                    if padding_len > 0 {
                        let mut packet = BytesMut::with_capacity(size);
                        packet.put_u8(CMD_WASTE);
                        packet.put_u32(0);
                        packet.put_u16(padding_len as u16);
                        packet.extend_from_slice(remaining);
                        packet.extend_from_slice(&self.padding.rng_vec(padding_len));
                        conn.write_all(&packet).await?;
                    } else {
                        conn.write_all(remaining).await?;
                    }
                    total_written += remaining.len();
                    remaining = &[];
                } else {
                    // 这个包全是填充
                    let mut packet = BytesMut::with_capacity(size);
                    packet.put_u8(CMD_WASTE);
                    packet.put_u32(0);
                    packet.put_u16((size - HEADER_OVERHEAD_SIZE) as u16);
                    packet.extend_from_slice(&self.padding.rng_vec(size - HEADER_OVERHEAD_SIZE));
                    conn.write_all(&packet).await?;
                }
            }

            // 可能还有剩余的有效载荷要写入
            if !remaining.is_empty() {
                conn.write_all(remaining).await?;
                total_written += remaining.len();
            }

            Ok(total_written)
        } else {
            self.send_padding.store(false, Ordering::Release);
            conn.write_all(data).await?;
            Ok(data.len())
        }
    }

    /// 接收循环
    async fn recv_loop(&self) -> io::Result<()> {
        let mut conn = self.conn.lock().await;
        let mut header_buf = [0u8; HEADER_OVERHEAD_SIZE];

        loop {
            if self.is_closed() {
                log::debug!("[Session] Session closed, exiting receive loop");
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Session closed"));
            }

            // 读取头部
            log::debug!("[Session] Reading frame header");
            conn.read_exact(&mut header_buf).await?;
            let header = crate::proxy::session::frame::RawHeader::from_bytes(&header_buf)?;
            log::debug!("[Session] Received frame header: cmd={}, sid={}, length={}", 
                       header.cmd, header.sid, header.length);

            // 读取数据
            let mut data = BytesMut::with_capacity(header.length as usize);
            if header.length > 0 {
                data.resize(header.length as usize, 0);
                conn.read_exact(&mut data).await?;
                log::debug!("[Session] Read {} bytes of frame data", data.len());
            }

            // 处理帧
            log::debug!("[Session] Processing frame: cmd={}, sid={}", header.cmd, header.sid);
            self.handle_frame(header.cmd, header.sid, data.freeze()).await?;
        }
    }

    /// 处理接收到的帧
    async fn handle_frame(&self, cmd: u8, sid: u32, data: Bytes) -> io::Result<()> {
        log::debug!("[Session] Handling frame: cmd={}, sid={}, data_len={}", cmd, sid, data.len());
        
        match cmd {
            CMD_PSH => {
                // 数据推送 - 将数据发送到对应的 Stream
                log::debug!("[Session] Processing PSH frame for stream {}", sid);
                if !data.is_empty() {
                    let data_len = data.len();
                    let streams = self.streams.read().await;
                    if let Some(stream_tx) = streams.get(&sid) {
                        if let Err(_) = stream_tx.try_send(data) {
                            log::warn!("[Session] Failed to send data to stream {}: channel full", sid);
                        } else {
                            log::debug!("[Session] Successfully sent {} bytes to stream {}", data_len, sid);
                        }
                    } else {
                        log::warn!("[Session] Received data for unknown stream: {}", sid);
                    }
                } else {
                    log::debug!("[Session] Received empty PSH frame for stream {}", sid);
                }
            }
            CMD_SYN => {
                // 流打开请求
                if !self.is_client {
                    // 服务端处理新的 Stream 请求
                    if self.streams.read().await.contains_key(&sid) {
                        // Stream 已存在，发送错误响应
                        let error_msg = format!("Stream {} already exists", sid);
                        let frame = Frame::with_data(CMD_SYNACK, sid, Bytes::from(error_msg));
                        if let Err(e) = self.write_control_frame(frame).await {
                            log::error!("Failed to send SYNACK error for stream {}: {}", sid, e);
                        }
                    } else {
                        // 创建新的 Stream
                        let (data_tx, data_rx) = mpsc::channel(100);
                        let (frame_tx, _frame_rx) = mpsc::channel(100);
                        let (close_tx, _close_rx) = oneshot::channel();
                        let _stream = Stream::new(sid, data_rx, frame_tx, close_tx);
                        
                        // 注册 Stream 到 Session
                        {
                            let mut streams = self.streams.write().await;
                            streams.insert(sid, data_tx);
                        }
                        
                        // 发送 SYNACK 确认
                        let frame = Frame::new(CMD_SYNACK, sid);
                        if let Err(e) = self.write_control_frame(frame).await {
                            log::error!("Failed to send SYNACK for stream {}: {}", sid, e);
                            // 如果发送失败，移除刚创建的 Stream
                            let mut streams = self.streams.write().await;
                            streams.remove(&sid);
                        } else {
                            log::info!("Stream {} opened successfully", sid);
                        }
                    }
                } else {
                    // 客户端不应该收到 SYN，记录警告
                    log::warn!("Client received unexpected SYN for stream: {}", sid);
                }
            }
            CMD_SYNACK => {
                // 流打开确认
                if self.is_client {
                    if let Some(_stream_tx) = self.streams.read().await.get(&sid) {
                        if !data.is_empty() {
                            // 包含错误信息
                            let error_msg = String::from_utf8_lossy(&data);
                            log::error!("Stream {} open failed: {}", sid, error_msg);
                        } else {
                            // 成功打开
                            log::info!("Stream {} opened successfully", sid);
                        }
                    } else {
                        log::warn!("Received SYNACK for unknown stream: {}", sid);
                    }
                } else {
                    // 服务端不应该收到 SYNACK，记录警告
                    log::warn!("Server received unexpected SYNACK for stream: {}", sid);
                }
            }
            CMD_FIN => {
                // 流关闭
                if self.streams.read().await.contains_key(&sid) {
                    log::info!("Stream {} closing", sid);
                    // 由于 Arc<Stream> 不能调用需要 &mut self 的方法
                    // 这里需要重新设计关闭机制
                }
                
                // 从 streams 中移除
                {
                    let mut streams = self.streams.write().await;
                    streams.remove(&sid);
                }
            }
            CMD_WASTE => {
                // 填充数据 - 完全忽略
                // 这是填充包，不需要任何处理
            }
            CMD_SETTINGS => {
                // 客户端设置
                if !self.is_client && !data.is_empty() {
                    self.handle_client_settings(data).await?;
                } else if self.is_client {
                    log::warn!("Client received unexpected SETTINGS");
                }
            }
            CMD_ALERT => {
                // 警告消息
                if !data.is_empty() {
                    let alert_msg = String::from_utf8_lossy(&data);
                    if self.is_client {
                        log::error!("Alert from server: {}", alert_msg);
                    } else {
                        log::error!("Alert from client: {}", alert_msg);
                    }
                    return Err(io::Error::new(io::ErrorKind::Other, format!("Alert received: {}", alert_msg)));
                } else {
                    log::warn!("Received empty alert");
                }
            }
            CMD_UPDATE_PADDING_SCHEME => {
                // 更新填充方案
                if self.is_client && !data.is_empty() {
                    self.handle_update_padding_scheme(data).await?;
                } else if !self.is_client {
                    log::warn!("Server received unexpected UPDATE_PADDING_SCHEME");
                }
            }
            CMD_HEART_REQUEST => {
                // 心跳请求 - 立即回复
                let frame = Frame::new(CMD_HEART_RESPONSE, sid);
                if let Err(e) = self.write_control_frame(frame).await {
                    log::error!("Failed to send heartbeat response: {}", e);
                } else {
                    log::debug!("Sent heartbeat response for stream: {}", sid);
                }
            }
            CMD_HEART_RESPONSE => {
                // 心跳响应 - 记录接收
                log::debug!("Received heartbeat response for stream: {}", sid);
            }
            CMD_SERVER_SETTINGS => {
                // 服务端设置
                if self.is_client && !data.is_empty() {
                    log::debug!("[Session] Received server settings, processing...");
                    self.handle_server_settings(data).await?;
                    log::debug!("[Session] Server settings processed, ready for streams");
                } else if !self.is_client {
                    log::warn!("[Session] Server received unexpected SERVER_SETTINGS");
                }
            }
            _ => {
                // 未知命令
                log::warn!("Unknown command received: {} for stream: {}", cmd, sid);
                // 对于未知命令，我们选择忽略而不是返回错误
                // 这样可以保持协议的向前兼容性
            }
        }

        Ok(())
    }

    /// 处理客户端设置
    async fn handle_client_settings(&self, data: Bytes) -> io::Result<()> {
        let settings = StringMap::from_bytes(&data);
        
        // 检查填充方案
        if let Some(padding_md5) = settings.get("padding-md5") {
            if padding_md5 != self.padding.md5() {
                let raw_scheme = self.padding.raw_scheme().to_vec();
                let frame = Frame::with_data(CMD_UPDATE_PADDING_SCHEME, 0, Bytes::from(raw_scheme));
                self.write_control_frame(frame).await?;
            }
        }

        // 检查客户端版本
        if let Some(version) = settings.get("v") {
            if let Ok(v) = version.parse::<u32>() {
                self.peer_version.store(v, Ordering::Release);
                if v >= 2 {
                    // 发送服务端设置
                    let server_settings = StringMap::from([("v".to_string(), "2".to_string())]);
                    let frame = Frame::with_data(CMD_SERVER_SETTINGS, 0, Bytes::from(server_settings.to_bytes()));
                    self.write_control_frame(frame).await?;
                }
            }
        }

        Ok(())
    }

    /// 处理服务端设置
    async fn handle_server_settings(&self, data: Bytes) -> io::Result<()> {
        let settings = StringMap::from_bytes(&data);
        if let Some(version) = settings.get("v") {
            if let Ok(v) = version.parse::<u32>() {
                self.peer_version.store(v, Ordering::Release);
            }
        }
        Ok(())
    }

    /// 处理填充方案更新
    async fn handle_update_padding_scheme(&self, data: Bytes) -> io::Result<()> {
        // 尝试创建新的填充方案
        if let Some(new_padding) = crate::proxy::padding::PaddingFactory::new(&data) {
            // 验证填充方案的 MD5
            let new_md5 = new_padding.md5();
            let current_md5 = self.padding.md5();
            
            if new_md5 != current_md5 {
                log::info!("Updating padding scheme from {} to {}", current_md5, new_md5);
                
                // 这里应该更新全局填充方案
                // 由于当前架构限制，我们只能记录日志
                // 在实际应用中，可能需要通过回调或事件通知机制来更新全局填充方案
                log::info!("New padding scheme MD5: {}", new_md5);
                
                // 发送确认（可选）
                let ack_data = format!("Padding scheme updated to: {}", new_md5);
                let frame = Frame::with_data(CMD_UPDATE_PADDING_SCHEME, 0, Bytes::from(ack_data));
                if let Err(e) = self.write_control_frame(frame).await {
                    log::error!("Failed to send padding scheme update ack: {}", e);
                }
            } else {
                log::debug!("Received same padding scheme, no update needed");
            }
        } else {
            log::error!("Invalid padding scheme data received");
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid padding scheme"));
        }
        
        Ok(())
    }


    /// 检查是否已关闭
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// 关闭 Session
    pub async fn close(&self) -> io::Result<()> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }

        // 关闭所有 Stream
        {
            let streams = self.streams.read().await;
            for _stream in streams.values() {
                // 由于 Arc<Stream> 不能调用需要 &mut self 的方法
                // 我们通过发送关闭信号来处理
                // 这里需要重新设计关闭机制
            }
        }

        Ok(())
    }
}

impl Clone for Session {
    fn clone(&self) -> Self {
        Self {
            conn: self.conn.clone(),
            streams: self.streams.clone(),
            next_stream_id: AtomicU32::new(self.next_stream_id.load(Ordering::Acquire)),
            closed: AtomicBool::new(self.closed.load(Ordering::Acquire)),
            is_client: self.is_client,
            settings_sent: AtomicBool::new(self.settings_sent.load(Ordering::Acquire)),
            peer_version: AtomicU32::new(self.peer_version.load(Ordering::Acquire)),
            padding: self.padding.clone(),
            pkt_counter: AtomicU32::new(self.pkt_counter.load(Ordering::Acquire)),
            send_padding: AtomicBool::new(self.send_padding.load(Ordering::Acquire)),
            buffering: AtomicBool::new(self.buffering.load(Ordering::Acquire)),
            buffer: self.buffer.clone(),
            data_tx: self.data_tx.clone(),
            session_id: AtomicU64::new(self.session_id.load(Ordering::Acquire)),
        }
    }
}