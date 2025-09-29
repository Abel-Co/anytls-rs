use crate::proxy::padding::PaddingFactory;
use crate::proxy::session::frame::{
    Frame, RawHeader, CMD_ALERT, CMD_FIN, CMD_HEART_REQUEST, CMD_HEART_RESPONSE, CMD_PSH, CMD_SERVER_SETTINGS,
    CMD_SETTINGS, CMD_SYN, CMD_SYNACK, CMD_UPDATE_PADDING_SCHEME, CMD_WASTE, HEADER_OVERHEAD_SIZE,
};
use crate::proxy::session::Stream;
use crate::util::{string_map::{StringMap, StringMapExt}, PROGRAM_VERSION_NAME};
use crate::util::r#type::AsyncReadWrite;
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use bytes::BytesMut;
use tokio::sync::{Mutex, RwLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct Session {
    conn: Arc<Mutex<Box<dyn AsyncReadWrite>>>,
    streams: Arc<Mutex<HashMap<u32, Arc<Stream>>>>,
    stream_id: Arc<Mutex<u32>>,
    closed: Arc<Mutex<bool>>,
    is_client: bool,
    peer_version: Arc<Mutex<u8>>,
    padding: Arc<RwLock<PaddingFactory>>,
    send_padding: Arc<Mutex<bool>>,
    buffering: Arc<Mutex<bool>>,
    buffer: Arc<Mutex<Vec<u8>>>,
    pkt_counter: Arc<Mutex<u32>>,
    on_new_stream: Option<Arc<dyn Fn(Arc<Stream>) + Send + Sync>>,
}

impl Session {
    pub fn new_client(
        conn: Box<dyn crate::util::r#type::AsyncReadWrite>,
        padding: Arc<RwLock<PaddingFactory>>,
    ) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            streams: Arc::new(Mutex::new(HashMap::new())),
            stream_id: Arc::new(Mutex::new(0)),
            closed: Arc::new(Mutex::new(false)),
            is_client: true,
            peer_version: Arc::new(Mutex::new(0)),
            padding,
            send_padding: Arc::new(Mutex::new(true)),
            buffering: Arc::new(Mutex::new(false)),
            buffer: Arc::new(Mutex::new(Vec::new())),
            pkt_counter: Arc::new(Mutex::new(0)),
            on_new_stream: None,
        }
    }
    
    pub fn new_server(
        conn: Box<dyn crate::util::r#type::AsyncReadWrite>,
        on_new_stream: Box<dyn Fn(Arc<Stream>) + Send + Sync>,
        padding: Arc<RwLock<PaddingFactory>>,
    ) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            streams: Arc::new(Mutex::new(HashMap::new())),
            stream_id: Arc::new(Mutex::new(0)),
            closed: Arc::new(Mutex::new(false)),
            is_client: false,
            peer_version: Arc::new(Mutex::new(0)),
            padding,
            send_padding: Arc::new(Mutex::new(false)),
            buffering: Arc::new(Mutex::new(false)),
            buffer: Arc::new(Mutex::new(Vec::new())),
            pkt_counter: Arc::new(Mutex::new(0)),
            on_new_stream: Some(Arc::new(on_new_stream)),
        }
    }
    
    pub async fn run(&self) -> io::Result<()> {
        if self.is_client {
            // Client settings are sent in create_session, just run recv_loop
            self.recv_loop().await
        } else {
            // Server runs recv_loop directly
            self.recv_loop().await
        }
    }
    
    pub async fn send_settings(&self) -> io::Result<()> {
        let padding = self.padding.read().await;
        let mut settings = StringMap::new();
        settings.insert("v".to_string(), "2".to_string());
        settings.insert("client".to_string(), PROGRAM_VERSION_NAME.to_string());
        settings.insert("padding-md5".to_string(), padding.md5().to_string());
        drop(padding);
        
        let frame = Frame::with_data(CMD_SETTINGS, 0, settings.to_bytes().into());
        self.write_frame(frame).await?;
        
        let mut buffering = self.buffering.lock().await;
        *buffering = true;
        
        Ok(())
    }
    
    pub async fn recv_loop(&self) -> io::Result<()> {
        let mut received_settings_from_client = false;
        
        loop {
            if *self.closed.lock().await {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Session closed"));
            }
            
            // 读取头部信息 (7字节)
            let mut header_bytes = BytesMut::with_capacity(HEADER_OVERHEAD_SIZE);
            {
                let mut conn = self.conn.lock().await;
                conn.read_exact(&mut header_bytes).await?;
            }
            
            let Some(header) = RawHeader::from_bytes(header_bytes) else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid header"));
            };
            
            let sid = header.sid;
            let cmd = header.cmd;
            let length = header.length as usize;
            
            // 根据命令类型处理
            match cmd {
                CMD_PSH => {
                    if length > 0 {
                        let mut data = vec![0u8; length];
                        let mut payload = BytesMut::new();
                        {
                            let mut conn = self.conn.lock().await;
                            conn.read_exact(&mut payload).await?;
                        }
                        
                        let streams = self.streams.lock().await;
                        if let Some(stream) = streams.get(&sid) {
                            // 将数据写入流
                            if let Err(e) = stream.write(&data).await {
                                // todo let Ok(frame) = Frame::from_bytes(payload) else {}
                                log::warn!("Failed to write to stream {}: {}", sid, e);
                            }
                        }
                    }
                }
                CMD_SYN => { // should be server only
                    log::debug!("Received CMD_SYN from client, received_settings_from_client: {}", received_settings_from_client);
                    if !self.is_client && !received_settings_from_client {
                        // 客户端没有发送设置，发送警告
                        log::warn!("Client did not send its settings, sending alert");
                        let frame = Frame::with_data(CMD_ALERT, 0, "client did not send its settings".as_bytes().to_vec().into());
                        self.write_frame(frame).await?;
                        return Ok(());
                    }
                    
                    let mut streams = self.streams.lock().await;
                    if !streams.contains_key(&sid) {
                        let stream = Arc::new(Stream::new(sid));
                        streams.insert(sid, stream.clone());
                        drop(streams);
                        
                        // 调用新流回调
                        if let Some(callback) = self.on_new_stream.clone() {
                            let stream_clone = stream.clone();
                            tokio::spawn(async move {
                                callback(stream_clone);
                            });
                        }
                    }
                }
                CMD_SYNACK => { // should be client only
                    if length > 0 {
                        let mut data = vec![0u8; length];
                        {
                            let mut conn = self.conn.lock().await;
                            conn.read_exact(&mut data).await?;
                        }
                        
                        // 报告错误
                        let streams = self.streams.lock().await;
                        if let Some(stream) = streams.get(&sid) {
                            let error_msg = format!("remote: {}", String::from_utf8_lossy(&data));
                            let _ = stream.close_with_error(Some(io::Error::new(io::ErrorKind::Other, error_msg))).await;
                        }
                    }
                }
                CMD_FIN => {
                    let streams = self.streams.lock().await;
                    if let Some(stream) = streams.get(&sid) {
                        let _ = stream.close().await;
                    }
                    let mut streams = self.streams.lock().await;
                    streams.remove(&sid);
                }
                CMD_WASTE => {
                    if length > 0 {
                        let mut data = vec![0u8; length];
                        {
                            let mut conn = self.conn.lock().await;
                            conn.read_exact(&mut data).await?;
                        }
                        // 丢弃填充数据
                    }
                }
                CMD_SETTINGS => {
                    log::debug!("Received CMD_SETTINGS, length: {}", length);
                    if length > 0 {
                        let mut data = vec![0u8; length];
                        {
                            let mut conn = self.conn.lock().await;
                            conn.read_exact(&mut data).await?;
                        }
                        
                        if !self.is_client {
                            log::debug!("Server received settings from client");
                            received_settings_from_client = true;
                            let settings = StringMap::from_bytes(&data);
                            
                            // 检查填充方案
                            let padding = self.padding.read().await;
                            if settings.get("padding-md5") != Some(&padding.md5().to_string()) {
                                let frame = Frame::with_data(CMD_UPDATE_PADDING_SCHEME, 0, padding.raw_scheme().to_vec().into());
                                self.write_frame(frame).await?;
                            }
                            
                            // 检查客户端版本
                            if let Some(version_str) = settings.get("v") {
                                if let Ok(version) = version_str.parse::<u8>() {
                                    if version >= 2 {
                                        let mut peer_version = self.peer_version.lock().await;
                                        *peer_version = version;
                                        drop(peer_version);
                                        
                                        // 发送服务器设置
                                        let mut server_settings = StringMap::new();
                                        server_settings.insert("v".to_string(), "2".to_string());
                                        let frame = Frame::with_data(CMD_SERVER_SETTINGS, 0, server_settings.to_bytes().into());
                                        self.write_frame(frame).await?;
                                    }
                                }
                            }
                        }
                    }
                }
                CMD_ALERT => {
                    if length > 0 {
                        let mut data = vec![0u8; length];
                        {
                            let mut conn = self.conn.lock().await;
                            conn.read_exact(&mut data).await?;
                        }
                        
                        if self.is_client {
                            log::error!("[Alert from server] {}", String::from_utf8_lossy(&data));
                        }
                        return Ok(());
                    }
                }
                CMD_UPDATE_PADDING_SCHEME => {
                    if length > 0 {
                        // `rawScheme` Do not use buffer to prevent subsequent misuse
                        let mut raw_scheme = vec![0u8; length];
                        {
                            let mut conn = self.conn.lock().await;
                            conn.read_exact(&mut raw_scheme).await?;
                        }
                        
                        if self.is_client {
                            // 更新填充方案
                            use crate::proxy::padding::DefaultPaddingFactory;
                            if DefaultPaddingFactory::update(&raw_scheme).await {
                                log::info!("[Update padding succeed] {:x}", md5::compute(&raw_scheme));
                            } else {
                                log::warn!("[Update padding failed] {:x}", md5::compute(&raw_scheme));
                            }
                        }
                    }
                }
                CMD_HEART_REQUEST => {
                    let frame = Frame::new(CMD_HEART_RESPONSE, sid);
                    self.write_frame(frame).await?;
                }
                CMD_HEART_RESPONSE => {
                    // 心跳响应处理 - 目前没有实现主动心跳检查
                }
                CMD_SERVER_SETTINGS => {
                    if length > 0 {
                        let mut data = vec![0u8; length];
                        {
                            let mut conn = self.conn.lock().await;
                            conn.read_exact(&mut data).await?;
                        }
                        
                        if self.is_client {
                            // Check server's version
                            let settings = StringMap::from_bytes(&data);
                            if let Some(version_str) = settings.get("v") {
                                if let Ok(version) = version_str.parse::<u8>() {
                                    let mut peer_version = self.peer_version.lock().await;
                                    *peer_version = version;
                                }
                            }
                        }
                    }
                }
                _ => {
                    // 未知命令，忽略
                }
            }
        }
    }
    
    pub async fn write_frame(&self, frame: Frame) -> io::Result<usize> {
        let Ok(data) = frame.to_bytes() else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Frame payload too large"));
        };
        self.write_conn(&data).await
    }
    
    async fn write_conn(&self, data: &[u8]) -> io::Result<usize> {
        let mut conn = self.conn.lock().await;
        
        // 检查是否正在缓冲
        if *self.buffering.lock().await {
            let mut buffer = self.buffer.lock().await;
            buffer.extend_from_slice(data);
            return Ok(data.len());
        }
        
        // 如果有缓冲数据，先发送缓冲数据
        let mut buffer = self.buffer.lock().await;
        if !buffer.is_empty() {
            let buffered_data = buffer.clone();
            buffer.clear();
            drop(buffer);
            
            let mut combined_data = buffered_data;
            combined_data.extend_from_slice(data);
            conn.write_all(&combined_data).await?;
            return Ok(combined_data.len());
        }
        drop(buffer);
        
        // 处理填充
        if *self.send_padding.lock().await {
            let mut pkt_counter = self.pkt_counter.lock().await;
            *pkt_counter += 1;
            let pkt = *pkt_counter;
            drop(pkt_counter);
            
            let padding = self.padding.read().await;
            if pkt < padding.stop() {
                let pkt_sizes = padding.generate_record_payload_sizes(pkt);
                let mut total_written = 0;
                let mut remaining_data = data;
                
                for &size in &pkt_sizes {
                    if size == crate::proxy::padding::CHECK_MARK {
                        if remaining_data.is_empty() {
                            break;
                        } else {
                            continue;
                        }
                    }
                    
                    let size = size as usize;
                    if remaining_data.len() > size {
                        // 这个包全是有效载荷
                        conn.write_all(&remaining_data[..size]).await?;
                        total_written += size;
                        remaining_data = &remaining_data[size..];
                    } else if !remaining_data.is_empty() {
                        // 这个包包含填充和有效载荷的最后部分
                        let padding_len = size.saturating_sub(remaining_data.len()).saturating_sub(HEADER_OVERHEAD_SIZE);
                        if padding_len > 0 {
                            let mut padding_data = vec![0u8; HEADER_OVERHEAD_SIZE + padding_len];
                            padding_data[0] = CMD_WASTE;
                            padding_data[1..5].copy_from_slice(&0u32.to_be_bytes());
                            padding_data[5..7].copy_from_slice(&(padding_len as u16).to_be_bytes());
                            
                            let mut combined = remaining_data.to_vec();
                            combined.extend_from_slice(&padding_data);
                            conn.write_all(&combined).await?;
                            total_written += remaining_data.len();
                        } else {
                            conn.write_all(remaining_data).await?;
                            total_written += remaining_data.len();
                        }
                        remaining_data = &[];
                    } else {
                        // 这个包全是填充
                        let mut padding_data = vec![0u8; HEADER_OVERHEAD_SIZE + size];
                        padding_data[0] = CMD_WASTE;
                        padding_data[1..5].copy_from_slice(&0u32.to_be_bytes());
                        padding_data[5..7].copy_from_slice(&(size as u16).to_be_bytes());
                        conn.write_all(&padding_data).await?;
                    }
                }
                
                // 可能还有剩余的有效载荷要写入
                if !remaining_data.is_empty() {
                    conn.write_all(remaining_data).await?;
                    total_written += remaining_data.len();
                }
                
                return Ok(total_written);
            } else {
                // 停止发送填充
                let mut send_padding = self.send_padding.lock().await;
                *send_padding = false;
            }
        }
        
        conn.write_all(data).await?;
        Ok(data.len())
    }
    
    pub async fn open_stream(&self) -> io::Result<Arc<Stream>> {
        let mut stream_id = self.stream_id.lock().await;
        *stream_id += 1;
        let id = *stream_id;
        drop(stream_id);
        
        let stream = Arc::new(Stream::new(id));
        
        let frame = Frame::new(CMD_SYN, id);
        self.write_frame(frame).await?;
        
        let mut streams = self.streams.lock().await;
        streams.insert(id, stream.clone());
        
        Ok(stream)
    }
    
    pub async fn stream_closed(&self, sid: u32) -> io::Result<()> {
        let frame = Frame::new(CMD_FIN, sid);
        self.write_frame(frame).await?;
        
        let mut streams = self.streams.lock().await;
        streams.remove(&sid);
        
        Ok(())
    }
    
    pub async fn close(&self) -> io::Result<()> {
        let mut closed = self.closed.lock().await;
        if *closed {
            return Ok(());
        }
        *closed = true;
        drop(closed);
        
        let streams = self.streams.lock().await;
        for stream in streams.values() {
            let _ = stream.close().await;
        }
        
        Ok(())
    }
    
    pub async fn is_closed(&self) -> bool {
        *self.closed.lock().await
    }
    
    pub async fn peer_version(&self) -> u8 {
        *self.peer_version.lock().await
    }
}

impl Clone for Session {
    fn clone(&self) -> Self {
        Self {
            conn: self.conn.clone(),
            streams: self.streams.clone(),
            stream_id: self.stream_id.clone(),
            closed: self.closed.clone(),
            is_client: self.is_client,
            peer_version: self.peer_version.clone(),
            padding: self.padding.clone(),
            send_padding: self.send_padding.clone(),
            buffering: self.buffering.clone(),
            buffer: self.buffer.clone(),
            pkt_counter: self.pkt_counter.clone(),
            on_new_stream: self.on_new_stream.clone(),
        }
    }
}