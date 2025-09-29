use anytls_rs::proxy::padding::DefaultPaddingFactory;
use anytls_rs::util::{mkcert, PROGRAM_VERSION_NAME};
use clap::Parser;
use log::{debug, error, info};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    let tls_config = Arc::new(mkcert::generate_key_pair("")?);
    let tls_acceptor = TlsAcceptor::from(tls_config);
    let padding = DefaultPaddingFactory::load();
    
    loop {
        let (stream, _addr) = listener.accept().await?;
        let tls_acceptor = tls_acceptor.clone();
        let password_sha256 = password_sha256.clone();
        let padding = padding.clone();
        
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, tls_acceptor, password_sha256.to_vec(), padding).await {
                debug!("Connection error: {}", e);
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    acceptor: TlsAcceptor,
    password_sha256: Vec<u8>,
    padding: Arc<anytls_rs::proxy::padding::PaddingFactory>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    
    info!("Authentication successful, starting session");
    
    // 创建服务器Session
    let session = anytls_rs::proxy::session::Session::new_server(
        Box::new(tls_stream),
        Box::new(|stream| {
            // 处理新流的回调
            tokio::spawn(async move {
                if let Err(e) = handle_new_stream(stream).await {
                    error!("Stream handling error: {}", e);
                }
            });
        }),
        padding,
    );
    
    // 运行Session
    if let Err(e) = session.run().await {
        error!("Session error: {}", e);
    }
    
    Ok(())
}

async fn handle_new_stream(stream: Arc<anytls_rs::proxy::session::Stream>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    
    // 读取目标地址
    let mut addr_data = Vec::new();
    let mut buffer = [0u8; 1];
    
    // 读取地址类型
    stream.read(&mut buffer).await?;
    let addr_type = buffer[0];
    addr_data.push(addr_type);
    
    match addr_type {
        1 => { // IPv4
            let mut ip = [0u8; 4];
            stream.read(&mut ip).await?;
            addr_data.extend_from_slice(&ip);
            
            let mut port = [0u8; 2];
            stream.read(&mut port).await?;
            addr_data.extend_from_slice(&port);
        }
        3 => { // 域名
            let mut len = [0u8; 1];
            stream.read(&mut len).await?;
            addr_data.push(len[0]);
            
            let mut domain = vec![0u8; len[0] as usize];
            stream.read(&mut domain).await?;
            addr_data.extend_from_slice(&domain);
            
            let mut port = [0u8; 2];
            stream.read(&mut port).await?;
            addr_data.extend_from_slice(&port);
        }
        4 => { // IPv6
            let mut ip = [0u8; 16];
            stream.read(&mut ip).await?;
            addr_data.extend_from_slice(&ip);
            
            let mut port = [0u8; 2];
            stream.read(&mut port).await?;
            addr_data.extend_from_slice(&port);
        }
        _ => {
            return Err("Unsupported address type".into());
        }
    }
    
    // 解析目标地址
    let target_addr = parse_socks_addr(&addr_data)?;
    info!("Connecting to: {}", target_addr);
    
    // 连接到目标
    let target_stream = match tokio::net::TcpStream::connect(&target_addr).await {
        Ok(stream) => stream,
        Err(e) => {
            error!("Failed to connect to target {}: {}", target_addr, e);
            // 发送连接失败响应
            let response = vec![5, 1, 0, 1, 0, 0, 0, 0, 0, 0]; // SOCKS5 connection failed
            let _ = stream.write(&response).await;
            return Err(e.into());
        }
    };
    
    // 发送连接成功响应
    let response = vec![5, 0, 0, 1, 0, 0, 0, 0, 0, 0]; // SOCKS5 success response
    if let Err(e) = stream.write(&response).await {
        error!("Failed to send SOCKS5 success response: {}", e);
        return Err(e.into());
    }
    
    info!("Successfully connected to target: {}", target_addr);
    
    // 开始数据转发
    let (mut target_read, mut target_write) = target_stream.into_split();
    let (mut stream_read, mut stream_write) = stream.split_ref();
    
    // 双向数据转发
    let client_to_target = async move {
        if let Err(e) = tokio::io::copy(&mut stream_read, &mut target_write).await {
            error!("Client to target error: {}", e);
        }
    };
    
    let target_to_client = async move {
        if let Err(e) = tokio::io::copy(&mut target_read, &mut stream_write).await {
            error!("Target to client error: {}", e);
        }
    };
    
    tokio::select! {
        _ = client_to_target => {
            debug!("Client to target stream ended");
        }
        _ = target_to_client => {
            debug!("Target to client stream ended");
        }
    }
    
    Ok(())
}

fn parse_socks_addr(addr_data: &[u8]) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    if addr_data.is_empty() {
        return Err("Empty address data".into());
    }
    
    let addr_type = addr_data[0];
    let mut pos = 1;
    
    match addr_type {
        1 => { // IPv4
            if addr_data.len() < 7 {
                return Err("Invalid IPv4 address".into());
            }
            let ip = &addr_data[pos..pos+4];
            pos += 4;
            let port = u16::from_be_bytes([addr_data[pos], addr_data[pos+1]]);
            Ok(format!("{}:{}", 
                ip.iter().map(|b| b.to_string()).collect::<Vec<_>>().join("."), 
                port))
        }
        3 => { // 域名
            if addr_data.len() < 4 {
                return Err("Invalid domain address".into());
            }
            let domain_len = addr_data[pos] as usize;
            pos += 1;
            if addr_data.len() < pos + domain_len + 2 {
                return Err("Invalid domain address length".into());
            }
            let domain = String::from_utf8(addr_data[pos..pos+domain_len].to_vec())?;
            pos += domain_len;
            let port = u16::from_be_bytes([addr_data[pos], addr_data[pos+1]]);
            Ok(format!("{}:{}", domain, port))
        }
        4 => { // IPv6
            if addr_data.len() < 19 {
                return Err("Invalid IPv6 address".into());
            }
            let ip = &addr_data[pos..pos+16];
            pos += 16;
            let port = u16::from_be_bytes([addr_data[pos], addr_data[pos+1]]);
            let ip_str = ip.chunks(2)
                .map(|chunk| format!("{:02x}{:02x}", chunk[0], chunk[1]))
                .collect::<Vec<_>>()
                .join(":");
            Ok(format!("[{}]:{}", ip_str, port))
        }
        _ => Err("Unsupported address type".into())
    }
}