use anytls_rs::proxy::padding::DefaultPaddingFactory;
use anytls_rs::proxy::session::Session;
use anytls_rs::proxy::{HighPerfOutboundPool, LockFreeOutboundPool, OutboundConnectionPool};
use anytls_rs::util::{mkcert, PROGRAM_VERSION_NAME};
use clap::Parser;
use log::{debug, error, info};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use rustls::ServerConfig;

#[derive(Parser)]
#[command(name = "anytls-server")]
#[command(about = "AnyTLS Server")]
struct Args {
    #[arg(short = 'l', long, default_value = "0.0.0.0:8443", help = "Server listen port")]
    listen: String,
    
    #[arg(short = 'p', long, help = "Password")]
    password: String,
    
    #[arg(long, help = "Padding scheme file")]
    padding_scheme: Option<String>,
    
    #[arg(long, default_value = "100", help = "Max outbound connections per target")]
    max_outbound_connections: usize,
    
    #[arg(long, default_value = "300", help = "Max idle time for outbound connections (seconds)")]
    max_idle_time_secs: u64,
    
    #[arg(long, default_value = "5", help = "Min idle connections per target")]
    min_idle_connections: usize,
    
    #[arg(long, default_value = "lock", help = "Connection pool type: lock, lockfree, highperf")]
    pool_type: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 设置日志级别，优先使用环境变量，默认显示 info 及以上级别
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    
    let args = Args::parse();
    
    if args.password.is_empty() {
        error!("Please set password");
        std::process::exit(1);
    }
    
    let password_sha256 = Sha256::digest(args.password.as_bytes());
    
    // Load padding scheme if provided
    if let Some(padding_file) = args.padding_scheme {
        let content = tokio::fs::read(&padding_file).await?;
        if DefaultPaddingFactory::update(&content).await {
            info!("Loaded padding scheme file: {}", padding_file);
        } else {
            error!("Wrong format padding scheme file: {}", padding_file);
            std::process::exit(1);
        }
    }
    
    info!("[Server] {}", PROGRAM_VERSION_NAME);
    info!("[Server] Listening TCP {}", args.listen);
    
    let listener = TcpListener::bind(&args.listen).await?;
    
    let tls_config = create_tls_config()?;
    let acceptor = TlsAcceptor::from(tls_config);
    let padding = DefaultPaddingFactory::load();
    
    // 创建外网连接池
    let outbound_pool = create_connection_pool(
        &args.pool_type,
        args.max_outbound_connections,
        Duration::from_secs(args.max_idle_time_secs),
        args.min_idle_connections,
    )?;
    
    info!("[Server] Outbound connection pool initialized:");
    info!("  - Pool type: {}", args.pool_type);
    info!("  - Max connections per target: {}", args.max_outbound_connections);
    info!("  - Max idle time: {} seconds", args.max_idle_time_secs);
    info!("  - Min idle connections: {}", args.min_idle_connections);
    
    // 启动统计信息打印任务
    let stats_pool = outbound_pool.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            let stats = stats_pool.get_stats().await;
            let pool_status = stats_pool.get_pool_status().await;
            info!("[Pool Stats] Total: {}, Active: {}, Reused: {}, New: {}, Cleaned: {}", 
                  stats.total_connections, stats.active_connections, 
                  stats.reused_connections, stats.new_connections, stats.cleaned_connections);
            if !pool_status.is_empty() {
                info!("[Pool Status] {:?}", pool_status);
            }
        }
    });
    
    loop {
        let (stream, _addr) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let password_sha256 = password_sha256.clone();
        let padding = padding.clone();
        let outbound_pool = outbound_pool.clone();
        
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, acceptor, password_sha256.to_vec(), padding, outbound_pool).await {
                debug!("Connection error: {}", e);
            }
        });
    }
}

fn create_tls_config() -> Result<Arc<ServerConfig>, Box<dyn std::error::Error>> {
    let cert = mkcert::generate_key_pair("")?;
    Ok(Arc::new(cert))
}

/// 创建连接池
fn create_connection_pool(
    pool_type: &str,
    max_connections: usize,
    max_idle_time: Duration,
    min_idle_connections: usize,
) -> Result<Arc<dyn ConnectionPoolTrait + Send + Sync>, Box<dyn std::error::Error>> {
    match pool_type {
        "lock" => {
            let pool = OutboundConnectionPool::new(max_connections, max_idle_time, min_idle_connections);
            Ok(Arc::new(WrapperPool::Lock(pool)))
        }
        "lockfree" => {
            let pool = LockFreeOutboundPool::new(max_connections, max_idle_time, min_idle_connections);
            Ok(Arc::new(WrapperPool::LockFree(pool)))
        }
        "highperf" => {
            let pool = HighPerfOutboundPool::new(max_connections, max_idle_time, min_idle_connections);
            Ok(Arc::new(WrapperPool::HighPerf(pool)))
        }
        _ => Err(format!("Unknown pool type: {}", pool_type).into()),
    }
}

/// 连接池特征
#[async_trait::async_trait]
trait ConnectionPoolTrait {
    async fn get_connection(&self, target: &str) -> Result<TcpStream, std::io::Error>;
    async fn return_connection(&self, target: &str, stream: TcpStream);
    async fn get_stats(&self) -> PoolStats;
    async fn get_pool_status(&self) -> std::collections::HashMap<String, usize>;
}

/// 连接池统计信息
#[derive(Debug, Clone, Copy)]
struct PoolStats {
    total_connections: u64,
    active_connections: u64,
    reused_connections: u64,
    new_connections: u64,
    cleaned_connections: u64,
}

/// 包装器枚举
enum WrapperPool {
    Lock(OutboundConnectionPool),
    LockFree(LockFreeOutboundPool),
    HighPerf(HighPerfOutboundPool),
}

#[async_trait::async_trait]
impl ConnectionPoolTrait for WrapperPool {
    async fn get_connection(&self, target: &str) -> Result<TcpStream, std::io::Error> {
        match self {
            WrapperPool::Lock(pool) => pool.get_connection(target).await,
            WrapperPool::LockFree(pool) => pool.get_connection(target).await,
            WrapperPool::HighPerf(pool) => pool.get_connection(target).await,
        }
    }

    async fn return_connection(&self, target: &str, stream: TcpStream) {
        match self {
            WrapperPool::Lock(pool) => pool.return_connection(target, stream).await,
            WrapperPool::LockFree(pool) => pool.return_connection(target, stream).await,
            WrapperPool::HighPerf(pool) => pool.return_connection(target, stream).await,
        }
    }

    async fn get_stats(&self) -> PoolStats {
        match self {
            WrapperPool::Lock(pool) => {
                let stats = pool.get_stats().await;
                PoolStats {
                    total_connections: stats.total_connections,
                    active_connections: stats.active_connections,
                    reused_connections: stats.reused_connections,
                    new_connections: stats.new_connections,
                    cleaned_connections: stats.cleaned_connections,
                }
            }
            WrapperPool::LockFree(pool) => {
                let stats = pool.get_stats();
                PoolStats {
                    total_connections: stats.total_connections,
                    active_connections: stats.active_connections,
                    reused_connections: stats.reused_connections,
                    new_connections: stats.new_connections,
                    cleaned_connections: stats.cleaned_connections,
                }
            }
            WrapperPool::HighPerf(pool) => {
                let stats = pool.get_stats();
                PoolStats {
                    total_connections: stats.total_connections,
                    active_connections: stats.active_connections,
                    reused_connections: stats.reused_connections,
                    new_connections: stats.new_connections,
                    cleaned_connections: stats.cleaned_connections,
                }
            }
        }
    }

    async fn get_pool_status(&self) -> std::collections::HashMap<String, usize> {
        match self {
            WrapperPool::Lock(pool) => pool.get_pool_status().await,
            WrapperPool::LockFree(pool) => pool.get_pool_status(),
            WrapperPool::HighPerf(pool) => pool.get_pool_status(),
        }
    }
}

async fn handle_connection(
    stream: TcpStream,
    acceptor: TlsAcceptor,
    password_sha256: Vec<u8>,
    padding: Arc<tokio::sync::RwLock<anytls_rs::proxy::padding::PaddingFactory>>,
    outbound_pool: Arc<dyn ConnectionPoolTrait + Send + Sync>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tls_stream = acceptor.accept(stream).await?;
    
    // Read authentication
    let mut auth_data = vec![0u8; 34]; // 32 bytes password + 2 bytes padding length
    tls_stream.read_exact(&mut auth_data).await?;
    
    let received_password = &auth_data[..32];
    if received_password != password_sha256.as_slice() {
        debug!("Authentication failed for {}", tls_stream.get_ref().0.peer_addr()?);
        return Ok(());
    }
    
    let padding_len = u16::from_be_bytes([auth_data[32], auth_data[33]]);
    if padding_len > 0 {
        let mut padding_data = vec![0u8; padding_len as usize];
        tls_stream.read_exact(&mut padding_data).await?;
    }
    
    // Create session
    let session = Session::new_server(
        Box::new(tls_stream),
        Box::new(move |stream| {
            // Handle new stream
            let outbound_pool = outbound_pool.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_stream(stream, outbound_pool).await {
                    debug!("Stream error: {}", e);
                }
            });
        }),
        padding,
    );
    
    session.run().await?;
    Ok(())
}

async fn handle_stream(
    stream: Arc<anytls_rs::proxy::session::Stream>,
    outbound_pool: Arc<dyn ConnectionPoolTrait + Send + Sync>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Read destination address (SOCKS format)
    let mut addr_buf = vec![0u8; 256];
    let n = stream.read(&mut addr_buf).await?;
    addr_buf.truncate(n);
    
    // Parse destination - 简化的 SOCKS 地址解析
    let destination = parse_socks_destination(&addr_buf)?;
    info!("[Stream] Connecting to: {}", destination);
    
    // 从连接池获取连接
    let mut outbound = outbound_pool.get_connection(&destination).await?;
    
    // Report success
    stream.handshake_success().await?;
    
    // Relay data
    let (mut stream_read, mut stream_write) = stream.split_ref();
    let (mut outbound_read, mut outbound_write) = outbound.split();
    
    // 使用 tokio::select! 进行双向数据转发
    let result = tokio::select! {
        result1 = tokio::io::copy(&mut stream_read, &mut outbound_write) => {
            match result1 {
                Ok(n) => {
                    debug!("[Stream] Stream -> Outbound: {} bytes", n);
                    Ok(())
                }
                Err(e) => {
                    debug!("[Stream] Stream -> Outbound error: {}", e);
                    Err(e)
                }
            }
        },
        result2 = tokio::io::copy(&mut outbound_read, &mut stream_write) => {
            match result2 {
                Ok(n) => {
                    debug!("[Stream] Outbound -> Stream: {} bytes", n);
                    Ok(())
                }
                Err(e) => {
                    debug!("[Stream] Outbound -> Stream error: {}", e);
                    Err(e)
                }
            }
        }
    };
    
    // 将连接归还到池中
    outbound_pool.return_connection(&destination, outbound).await;
    
    result.map_err(|e| e.into())
}

/// 简化的 SOCKS 地址解析
fn parse_socks_destination(data: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    if data.len() < 3 {
        return Err("Invalid SOCKS address format".into());
    }
    
    let atyp = data[0];
    match atyp {
        1 => { // IPv4
            if data.len() < 7 {
                return Err("Incomplete IPv4 address".into());
            }
            let addr = format!("{}.{}.{}.{}", data[1], data[2], data[3], data[4]);
            let port = u16::from_be_bytes([data[5], data[6]]);
            Ok(format!("{}:{}", addr, port))
        }
        3 => { // 域名
            if data.len() < 2 {
                return Err("Incomplete domain address".into());
            }
            let domain_len = data[1] as usize;
            if data.len() < 2 + domain_len + 2 {
                return Err("Incomplete domain address".into());
            }
            let domain = String::from_utf8_lossy(&data[2..2 + domain_len]);
            let port = u16::from_be_bytes([data[2 + domain_len], data[2 + domain_len + 1]]);
            Ok(format!("{}:{}", domain, port))
        }
        _ => Err("Unsupported address type".into()),
    }
}