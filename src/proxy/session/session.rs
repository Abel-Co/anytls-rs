use crate::proxy::padding::PaddingFactory;
use crate::proxy::session::frame::{Frame, CMD_FIN, CMD_PSH, CMD_SETTINGS, CMD_SYN, CMD_SYNACK, CMD_ALERT, CMD_UPDATE_PADDING_SCHEME, CMD_HEART_REQUEST, CMD_HEART_RESPONSE, CMD_SERVER_SETTINGS, CMD_WASTE, HEADER_OVERHEAD_SIZE};
use crate::proxy::session::stream::Stream;
use crate::util::string_map::{StringMap, StringMapExt};
use bytes::{BufMut, Bytes, BytesMut};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::io::{ReadHalf, WriteHalf};

// 使用 util 中定义的 trait
use crate::util::r#type::AsyncReadWrite;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};

/// Session 管理多个 Stream 的连接复用
pub struct Session {
    conn_r: Arc<Mutex<ReadHalf<Box<dyn AsyncReadWrite>>>>,
    conn_w: Arc<Mutex<WriteHalf<Box<dyn AsyncReadWrite>>>>,
    
    // Stream 管理
    streams: Arc<RwLock<HashMap<u32, mpsc::Sender<Bytes>>>>,
    next_stream_id: AtomicU32,
    
    // 状态管理
    closed: AtomicBool,
    is_client: bool,
    
    // 设置相关
    peer_version: AtomicU32,
    
    // 填充相关
    padding: Arc<PaddingFactory>,
    pkt_counter: AtomicU32,
    send_padding: AtomicBool,
}

impl Session {
    /// 创建客户端 Session
    pub fn new_client(
        conn: Box<dyn AsyncReadWrite>,
        padding: Arc<PaddingFactory>,
        enable_padding: bool,
    ) -> Self {
        let (conn_r, conn_w) = tokio::io::split(conn);
        
        Self {
            conn_r: Arc::new(Mutex::new(conn_r)),
            conn_w: Arc::new(Mutex::new(conn_w)),
            streams: Arc::new(RwLock::new(HashMap::new())),
            next_stream_id: AtomicU32::new(1),
            closed: AtomicBool::new(false),
            is_client: true,
            peer_version: AtomicU32::new(0),
            padding,
            pkt_counter: AtomicU32::new(0),
            send_padding: AtomicBool::new(enable_padding),
        }
    }

    /// 创建服务端 Session
    pub fn new_server(
        conn: Box<dyn AsyncReadWrite>,
        padding: Arc<PaddingFactory>,
    ) -> Self {
        let (conn_r, conn_w) = tokio::io::split(conn);
        
        Self {
            conn_r: Arc::new(Mutex::new(conn_r)),
            conn_w: Arc::new(Mutex::new(conn_w)),
            streams: Arc::new(RwLock::new(HashMap::new())),
            next_stream_id: AtomicU32::new(1),
            closed: AtomicBool::new(false),
            is_client: false,
            peer_version: AtomicU32::new(0),
            padding,
            pkt_counter: AtomicU32::new(0),
            send_padding: AtomicBool::new(false),
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

        // 在独立异步任务中运行接收循环
        log::debug!("[Session] Spawning receive loop task");
        let session_clone = self.clone();
        tokio::spawn(async move {
            if let Err(e) = session_clone.recv_loop().await {
                log::error!("Session receive loop error: {}", e);
            }
        });

        Ok(())
    }

    /// 发送客户端设置
    async fn send_client_settings(&self) -> io::Result<()> {
        let settings = StringMap::from([
            ("v".to_string(), "2".to_string()),
            ("client".to_string(), crate::PROGRAM_VERSION_NAME.to_string()),
            ("padding-md5".to_string(), self.padding.md5().to_string()),
        ]);

        log::debug!("[Session] Client settings: {:?}", settings);
        let frame = Frame::with_data(CMD_SETTINGS, 0, Bytes::from(settings.to_bytes()));
        let data = frame.to_bytes();
        // 协议要求：新会话必须“立即发送” cmdSettings。
        // 即使开启 padding，这里也直写，避免被分包/延后。
        let mut conn = self.conn_w.lock().await;
        conn.write_all(&data).await?;
        log::debug!("[Session] Client settings frame sent");
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
        let data = frame.to_bytes().to_vec();
        log::debug!("[Session] Writing frame: {:?}, bytes: {:?}", frame, &data);
        self.write_conn(&data).await
    }

    /// 写入连接
    async fn write_conn(&self, data: &[u8]) -> io::Result<usize> {
        let mut conn = self.conn_w.lock().await;
        
        log::debug!("[Session] Writing {} bytes to connection", data.len());

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
    async fn write_with_padding<W>(&self, conn: &mut W, data: &[u8]) -> io::Result<usize>
    where
        W: AsyncWrite + Unpin,
    {
        let pkt = self.pkt_counter.fetch_add(1, Ordering::AcqRel);
        if pkt >= self.padding.stop() {
            self.send_padding.store(false, Ordering::Release);
            conn.write_all(data).await?;
            return Ok(data.len());
        }

        // 兼容优先：业务帧必须保持完整，不对 AnyTLS 帧做字节级切分。
        // 先直发业务帧，再根据策略追加 CMD_WASTE 填充帧。
        conn.write_all(data).await?;

        let pkt_sizes = self.padding.generate_record_payload_sizes(pkt);
        let target_size = pkt_sizes
            .into_iter()
            .find(|s| *s != crate::proxy::padding::CHECK_MARK)
            .map(|s| s as usize)
            .unwrap_or(0);

        if target_size > data.len() + HEADER_OVERHEAD_SIZE {
            let waste_payload_len = target_size - data.len() - HEADER_OVERHEAD_SIZE;
            let mut waste = BytesMut::with_capacity(HEADER_OVERHEAD_SIZE + waste_payload_len);
            waste.put_u8(CMD_WASTE);
            waste.put_u32(0);
            waste.put_u16(waste_payload_len as u16);
            waste.extend_from_slice(&self.padding.rng_vec(waste_payload_len));
            conn.write_all(&waste).await?;
        }

        Ok(data.len())
    }

    /// 接收循环
    async fn recv_loop(&self) -> io::Result<()> {
        let mut header_buf = [0u8; HEADER_OVERHEAD_SIZE];

        loop {
            if self.is_closed() {
                log::debug!("[Session] Session closed, exiting receive loop");
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Session closed"));
            }

            // 读取头部
            log::debug!("[Session] Reading frame header");
            let (cmd, sid, data) = {
                // 读路径只在“读取一帧”期间持有连接锁，避免阻塞并发写
                let mut conn = self.conn_r.lock().await;
                conn.read_exact(&mut header_buf).await?;
                let header = crate::proxy::session::frame::RawHeader::from_bytes(&header_buf)?;
                log::debug!(
                    "[Session] Received frame header: cmd={}, sid={}, length={}",
                    header.cmd,
                    header.sid,
                    header.length
                );

                let mut data = BytesMut::with_capacity(header.length as usize);
                if header.length > 0 {
                    data.resize(header.length as usize, 0);
                    conn.read_exact(&mut data).await?;
                    log::debug!("[Session] Read {} bytes of frame data", data.len());
                }

                (header.cmd, header.sid, data.freeze())
            };

            // 处理帧
            log::debug!("[Session] Processing frame: cmd={}, sid={}", cmd, sid);
            self.handle_frame(cmd, sid, data).await?;
        }
    }

    /// 处理接收到的帧
    async fn handle_frame(&self, cmd: u8, sid: u32, data: Bytes) -> io::Result<()> {
        log::debug!("[Session] Handling frame: cmd={}, sid={}, data_len={}", cmd, sid, data.len());
        
        match cmd {
            CMD_WASTE => {
                // 填充数据 - 完全忽略
                // 这是填充包，不需要任何处理
                log::debug!("[Session] Received WASTE frame for stream {}", sid);
            }
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
                let mut streams = self.streams.write().await;
                if let Some(stream_tx) = streams.remove(&sid) {
                    drop(stream_tx);
                }
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
        log::debug!("[Session] handle_client_settings: received {} bytes of settings data: {:?}", data.len(), &data);

        let settings = StringMap::from_bytes(&data);
        log::debug!("[Session] handle_client_settings: parsed settings: {:?}", settings);

        // 检查填充方案
        if let Some(padding_md5) = settings.get("padding-md5") {
            log::debug!("[Session] handle_client_settings: padding MD5 - received: {}, current: {}", padding_md5, self.padding.md5());

            if padding_md5 != self.padding.md5() {
                let raw_scheme = self.padding.raw_scheme.clone();
                log::debug!("[Session] handle_client_settings: sending padding scheme update ({} bytes)", raw_scheme.len());

                let frame = Frame::with_data(CMD_UPDATE_PADDING_SCHEME, 0, Bytes::from(raw_scheme));
                self.write_control_frame(frame).await?;
                log::debug!("[Session] handle_client_settings: padding scheme update frame sent");
            } else {
                log::debug!("[Session] handle_client_settings: padding schemes match, no update needed");
            }
        } else {
            log::debug!("[Session] handle_client_settings: no padding-md5 found in settings");
        }

        // 检查客户端版本
        if let Some(version) = settings.get("v") {
            if let Ok(v) = version.parse::<u32>() {
                let old_version = self.peer_version.load(Ordering::Acquire);
                self.peer_version.store(v, Ordering::Release);
                log::debug!("[Session] handle_client_settings: version updated from {} to {}", old_version, v);

                if v >= 2 {
                    // 发送服务端设置
                    let server_settings = StringMap::from([("v".to_string(), "2".to_string())]);
                    log::debug!("[Session] handle_client_settings: sending server settings: {:?}", server_settings);

                    let frame = Frame::with_data(CMD_SERVER_SETTINGS, 0, Bytes::from(server_settings.to_bytes()));
                    self.write_control_frame(frame).await?;
                    log::debug!("[Session] handle_client_settings: server settings frame sent");
                } else {
                    log::debug!("[Session] handle_client_settings: version {} < 2, not sending server settings", v);
                }
            } else {
                log::warn!("[Session] handle_client_settings: failed to parse version: '{}'", version);
            }
        } else {
            log::debug!("[Session] handle_client_settings: no version found in settings");
        }

        log::debug!("[Session] handle_client_settings: processing completed successfully");
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
            conn_r: self.conn_r.clone(),
            conn_w: self.conn_w.clone(),
            streams: self.streams.clone(),
            next_stream_id: AtomicU32::new(self.next_stream_id.load(Ordering::Acquire)),
            closed: AtomicBool::new(self.closed.load(Ordering::Acquire)),
            is_client: self.is_client,
            peer_version: AtomicU32::new(self.peer_version.load(Ordering::Acquire)),
            padding: self.padding.clone(),
            pkt_counter: AtomicU32::new(self.pkt_counter.load(Ordering::Acquire)),
            send_padding: AtomicBool::new(self.send_padding.load(Ordering::Acquire)),
        }
    }
}
