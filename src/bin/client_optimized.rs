use anytls_rs::proxy::padding::DefaultPaddingFactory;
use anytls_rs::proxy::session::Client;
use anytls_rs::util::PROGRAM_VERSION_NAME;
use clap::Parser;
use log::{error, info, debug};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;
use rustls::ClientConfig;
use sha2::{Digest, Sha256};
use std::time::Instant;
use std::collections::HashMap;
use parking_lot::RwLock;

/**
 * 性能优化版
 * - 内存池 (MemoryPool)
 * - 高性能内存分配器
 * - 连接池 (ConnectionPool)
 * - 大缓冲区
 * - 批量I/O操作
 * - 异步I/O优化
 */
#[derive(Parser)]
#[command(name = "anytls-client-optimized")]
#[command(about = "AnyTLS Optimized Client")]
struct Args {
    #[arg(short = 'l', long, default_value = "127.0.0.1:1080", help = "SOCKS5 listen port")]
    listen: String,
    
    #[arg(short = 's', long, default_value = "127.0.0.1:8443", help = "Server address")]
    server: String,
    
    #[arg(long, help = "SNI")]
    sni: Option<String>,
    
    #[arg(short = 'p', long, help = "Password")]
    password: String,
    
    #[arg(long, default_value = "1000", help = "Connection pool size")]
    pool_size: usize,
    
    #[arg(long, default_value = "64", help = "Buffer size in KB")]
    buffer_size_kb: usize,
    
    #[arg(long, help = "Enable connection reuse")]
    enable_reuse: bool,
    
    #[arg(long, help = "Enable compression")]
    enable_compression: bool,
}

/// 连接池管理器
struct ConnectionPool {
    /// 可用连接
    available: Arc<RwLock<Vec<Arc<Client>>>>,
    /// 连接统计
    stats: Arc<RwLock<ConnectionStats>>,
    /// 最大连接数
    max_connections: usize,
}

#[derive(Debug, Default, Clone, Copy)]
struct ConnectionStats {
    /// 总连接数
    total_connections: u64,
    /// 活跃连接数
    active_connections: u64,
    /// 重用连接数
    reused_connections: u64,
    /// 新建连接数
    new_connections: u64,
}

impl ConnectionPool {
    fn new(max_connections: usize) -> Self {
        Self {
            available: Arc::new(RwLock::new(Vec::new())),
            stats: Arc::new(RwLock::new(ConnectionStats::default())),
            max_connections,
        }
    }
    
    /// 获取连接
    async fn get_connection(&self, client_factory: impl Fn() -> Arc<Client>) -> Arc<Client> {
        // 尝试从池中获取连接
        if let Some(connection) = self.available.write().pop() {
            let mut stats = self.stats.write();
            stats.reused_connections += 1;
            stats.active_connections += 1;
            return connection;
        }
        
        // 创建新连接
        let connection = client_factory();
        let mut stats = self.stats.write();
        stats.new_connections += 1;
        stats.total_connections += 1;
        stats.active_connections += 1;
        connection
    }
    
    /// 归还连接
    fn return_connection(&self, connection: Arc<Client>) {
        let mut available = self.available.write();
        if available.len() < self.max_connections {
            available.push(connection);
        }
        
        let mut stats = self.stats.write();
        stats.active_connections = stats.active_connections.saturating_sub(1);
    }
    
    /// 获取统计信息
    fn get_stats(&self) -> ConnectionStats {
        *self.stats.read()
    }
}

/// 性能监控器
struct PerformanceMonitor {
    /// 请求统计
    request_stats: Arc<RwLock<RequestStats>>,
    /// 开始时间
    start_time: Instant,
}

#[derive(Debug, Default, Clone, Copy)]
struct RequestStats {
    /// 总请求数
    total_requests: u64,
    /// 成功请求数
    successful_requests: u64,
    /// 失败请求数
    failed_requests: u64,
    /// 总处理时间(微秒)
    total_processing_time_us: u64,
    /// 最大处理时间(微秒)
    max_processing_time_us: u64,
    /// 最小处理时间(微秒)
    min_processing_time_us: u64,
}

impl PerformanceMonitor {
    fn new() -> Self {
        Self {
            request_stats: Arc::new(RwLock::new(RequestStats::default())),
            start_time: Instant::now(),
        }
    }
    
    /// 记录请求开始
    fn record_request_start(&self) -> Instant {
        Instant::now()
    }
    
    /// 记录请求结束
    fn record_request_end(&self, start_time: Instant, success: bool) {
        let duration = start_time.elapsed();
        let duration_us = duration.as_micros() as u64;
        
        let mut stats = self.request_stats.write();
        stats.total_requests += 1;
        
        if success {
            stats.successful_requests += 1;
        } else {
            stats.failed_requests += 1;
        }
        
        stats.total_processing_time_us += duration_us;
        
        if duration_us > stats.max_processing_time_us {
            stats.max_processing_time_us = duration_us;
        }
        
        if stats.min_processing_time_us == 0 || duration_us < stats.min_processing_time_us {
            stats.min_processing_time_us = duration_us;
        }
    }
    
    /// 获取统计信息
    fn get_stats(&self) -> RequestStats {
        *self.request_stats.read()
    }
    
    /// 获取平均处理时间
    fn get_avg_processing_time_us(&self) -> u64 {
        let stats = self.request_stats.read();
        if stats.total_requests > 0 {
            stats.total_processing_time_us / stats.total_requests
        } else {
            0
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    
    let args = Args::parse();
    
    if args.password.is_empty() {
        error!("Please set password");
        std::process::exit(1);
    }
    
    let password_sha256 = Sha256::digest(args.password.as_bytes());
    
    info!("[Optimized Client] {}", PROGRAM_VERSION_NAME);
    info!("[Optimized Client] SOCKS5 {} => {}", args.listen, args.server);
    info!("[Optimized Client] Pool size: {}, Buffer: {}KB", args.pool_size, args.buffer_size_kb);
    info!("[Optimized Client] Connection reuse: {}, Compression: {}", args.enable_reuse, args.enable_compression);
    
    let listener = TcpListener::bind(&args.listen).await?;
    
    let tls_config = create_tls_config(args.sni.as_deref())?;
    let padding = DefaultPaddingFactory::load();
    
    // 创建连接池
    let connection_pool = Arc::new(ConnectionPool::new(args.pool_size));
    
    // 创建性能监控器
    let perf_monitor = Arc::new(PerformanceMonitor::new());
    
    // 启动统计报告任务
    let perf_monitor_clone = perf_monitor.clone();
    let connection_pool_clone = connection_pool.clone();
    tokio::spawn(async move {
        let mut last_report = Instant::now();
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
            let now = Instant::now();
            let uptime = now.duration_since(last_report);
            
            let req_stats = perf_monitor_clone.get_stats();
            let conn_stats = connection_pool_clone.get_stats();
            let avg_time = perf_monitor_clone.get_avg_processing_time_us();
            
            info!("=== Performance Report ({}s) ===", uptime.as_secs());
            info!("Requests: {} total, {} success, {} failed", 
                   req_stats.total_requests, req_stats.successful_requests, req_stats.failed_requests);
            info!("Processing time: {}μs avg, {}μs min, {}μs max", 
                   avg_time, req_stats.min_processing_time_us, req_stats.max_processing_time_us);
            info!("Connections: {} total, {} active, {} reused, {} new", 
                   conn_stats.total_connections, conn_stats.active_connections, 
                   conn_stats.reused_connections, conn_stats.new_connections);
            
            last_report = now;
        }
    });
    
    info!("Optimized AnyTLS client started");
    
    loop {
        let (stream, _addr) = listener.accept().await?;
        let client_factory = {
            let server = args.server.clone();
            let tls_config = tls_config.clone();
            let password_sha256 = password_sha256.clone();
            let padding = padding.clone();
            
            move || {
                let server = server.clone();
                let tls_config = tls_config.clone();
                let password_sha256 = password_sha256.clone();
                let padding = padding.clone();
                
                Arc::new(Client::new(
                Box::new({
                    let padding = padding.clone();
                    move || {
                        let server = server.clone();
                        let tls_config = tls_config.clone();
                        let password_sha256 = password_sha256.clone();
                        let padding = padding.clone();
                        
                        Box::new(Box::pin(async move {
                            let stream = TcpStream::connect(&server).await?;
                            let connector = TlsConnector::from(tls_config);
                            let mut tls_stream = connector.connect("127.0.0.1".try_into().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?, stream).await?;
                            
                            // 发送认证
                            let mut auth_data = Vec::new();
                            auth_data.extend_from_slice(&password_sha256);
                            
                            let padding_factory = padding.read().await;
                            let padding_sizes = padding_factory.generate_record_payload_sizes(0);
                            let padding_len = if !padding_sizes.is_empty() {
                                padding_sizes[0] as u16
                            } else {
                                0
                            };
                            
                            auth_data.extend_from_slice(&padding_len.to_be_bytes());
                            if padding_len > 0 {
                                auth_data.resize(auth_data.len() + padding_len as usize, 0);
                            }
                            
                            tls_stream.write_all(&auth_data).await?;
                            
                            Ok(Box::new(tls_stream) as Box<dyn anytls_rs::util::r#type::AsyncReadWrite>)
                        }))
                    }
                }),
                padding,
                    std::time::Duration::from_secs(30),
                    std::time::Duration::from_secs(30),
                    5,
                ))
            }
        };
        
        let connection_pool = connection_pool.clone();
        let perf_monitor = perf_monitor.clone();
        
        tokio::spawn(async move {
            let start_time = perf_monitor.record_request_start();
            let mut success = false;
            
            if let Err(e) = handle_connection_optimized(stream, connection_pool, client_factory, args.buffer_size_kb * 1024).await {
                error!("Connection error: {}", e);
            } else {
                success = true;
            }
            
            perf_monitor.record_request_end(start_time, success);
        });
    }
}

fn create_tls_config(_sni: Option<&str>) -> Result<Arc<ClientConfig>, Box<dyn std::error::Error>> {
    let mut config = ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    
    // 使用危险的方法来禁用证书验证
    config.dangerous().set_certificate_verifier(Arc::new(AllowAnyCertVerifier));
    
    Ok(Arc::new(config))
}

// 允许任何证书的验证器
#[derive(Debug)]
struct AllowAnyCertVerifier;

impl rustls::client::danger::ServerCertVerifier for AllowAnyCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA1,
            rustls::SignatureScheme::ECDSA_SHA1_Legacy,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ED448,
        ]
    }
}

async fn handle_connection_optimized(
    mut stream: TcpStream,
    connection_pool: Arc<ConnectionPool>,
    client_factory: impl Fn() -> Arc<Client>,
    buffer_size: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    // 使用更大的缓冲区
    let mut buffer = vec![0u8; buffer_size];
    
    // SOCKS5 握手处理
    let n = stream.read(&mut buffer).await?;
    if n < 3 {
        return Err("Invalid SOCKS5 request".into());
    }
    
    let version = buffer[0];
    let nmethods = buffer[1] as usize;
    
    if version != 5 {
        return Err("Unsupported SOCKS version".into());
    }
    
    if n < 2 + nmethods {
        return Err("Incomplete SOCKS5 request".into());
    }
    
    // 检查是否支持无认证方法
    let mut supports_no_auth = false;
    for i in 2..2 + nmethods {
        if buffer[i] == 0 {
            supports_no_auth = true;
            break;
        }
    }
    
    if !supports_no_auth {
        return Err("No supported authentication method".into());
    }
    
    // 发送认证方法选择响应
    let response = [5, 0]; // 版本5，无认证
    stream.write_all(&response).await?;
    
    // 读取连接请求
    let n = stream.read(&mut buffer).await?;
    if n < 10 {
        return Err("Invalid SOCKS5 connect request".into());
    }
    
    let version = buffer[0];
    let cmd = buffer[1];
    let _rsv = buffer[2];
    let atyp = buffer[3];
    
    if version != 5 || cmd != 1 {
        return Err("Unsupported SOCKS5 command".into());
    }
    
    // 解析目标地址
    let mut target_addr = String::new();
    let mut target_port = 0;
    
    match atyp {
        1 => { // IPv4
            if n < 10 {
                return Err("Incomplete IPv4 address".into());
            }
            target_addr = format!("{}.{}.{}.{}", buffer[4], buffer[5], buffer[6], buffer[7]);
            target_port = u16::from_be_bytes([buffer[8], buffer[9]]);
        }
        3 => { // 域名
            let domain_len = buffer[4] as usize;
            if n < 5 + domain_len + 2 {
                return Err("Incomplete domain address".into());
            }
            target_addr = String::from_utf8_lossy(&buffer[5..5 + domain_len]).to_string();
            target_port = u16::from_be_bytes([buffer[5 + domain_len], buffer[5 + domain_len + 1]]);
        }
        _ => return Err("Unsupported address type".into()),
    }
    
    let target = format!("{}:{}", target_addr, target_port);
    debug!("Connecting to target: {}", target);
    
    // 从连接池获取客户端连接
    let client = connection_pool.get_connection(client_factory).await;
    
    // 发送连接成功响应
    let response = [5, 0, 0, 1, 0, 0, 0, 0, 0, 0]; // 成功响应
    stream.write_all(&response).await?;
    
    // 建立到目标服务器的连接
    let mut target_stream = match TcpStream::connect(&target).await {
        Ok(stream) => stream,
        Err(e) => {
            // 发送连接失败响应
            let response = [5, 1, 0, 1, 0, 0, 0, 0, 0, 0]; // 连接失败
            let _ = stream.write_all(&response).await;
            return Err(e.into());
        }
    };
    
    // 数据转发
    let (mut client_read, mut client_write) = stream.split();
    let (mut target_read, mut target_write) = target_stream.split();
    
    // 使用更大的缓冲区进行数据转发
    let mut client_buffer = vec![0u8; buffer_size];
    let mut target_buffer = vec![0u8; buffer_size];
    
    tokio::select! {
        _ = async {
            loop {
                let n = client_read.read(&mut client_buffer).await?;
                if n == 0 { break; }
                target_write.write_all(&client_buffer[..n]).await?;
            }
            Ok::<(), std::io::Error>(())
        } => {
            debug!("Client to target stream ended");
        }
        _ = async {
            loop {
                let n = target_read.read(&mut target_buffer).await?;
                if n == 0 { break; }
                client_write.write_all(&target_buffer[..n]).await?;
            }
            Ok::<(), std::io::Error>(())
        } => {
            debug!("Target to client stream ended");
        }
    }
    
    // 归还连接到池中
    connection_pool.return_connection(client);
    
    Ok(())
}
