use anytls_rs::util::PROGRAM_VERSION_NAME;
use clap::Parser;
use log::{error, info, debug};
use futures::{AsyncReadExt, AsyncWriteExt};
use glommio::net::{TcpListener, TcpStream};
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(name = "anytls-client")]
#[command(about = "AnyTLS Client")]
struct Args {
    #[arg(short = 'l', long, default_value = "127.0.0.1:1080", help = "Local listen port")]
    listen: String,
    
    #[arg(short = 's', long, help = "Server address")]
    server: String,
    
    #[arg(short = 'p', long, help = "Password")]
    password: String,
    
    #[arg(long, help = "SNI")]
    sni: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    glommio::LocalExecutorBuilder::default()
        .spawn(|| async move {
            if let Err(e) = run().await {
                eprintln!("Error: {}", e);
            }
        })?;
    
    // 保持主线程运行
    std::thread::park();
    Ok(())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // env_logger::init();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    
    let args = Args::parse();
    
    if args.password.is_empty() {
        error!("Please set password");
        std::process::exit(1);
    }
    
    let password_sha256 = Sha256::digest(args.password.as_bytes());
    
    info!("[Client] {}", PROGRAM_VERSION_NAME);
    info!("[Client] SOCKS5 {} => {}", args.listen, args.server);
    
    let listener = TcpListener::bind(&args.listen)?;
    let server_addr = args.server.clone();
    
    loop {
        let stream = listener.accept().await?;
        let server_addr = server_addr.clone();
        let password_sha256 = password_sha256.clone();
        
        glommio::spawn_local(async move {
            if let Err(e) = handle_socks5_connection(stream, server_addr, password_sha256.to_vec()).await {
                error!("SOCKS5 connection error: {}", e);
            }
        }).detach();
    }
}

async fn handle_socks5_connection(
    mut client_stream: TcpStream,
    server_addr: String,
    password_sha256: Vec<u8>,
) -> Result<(), Box<dyn std::error::Error>> {
    // 处理 SOCKS5 握手
    let mut buffer = [0u8; 1024];
    let n = client_stream.read(&mut buffer).await?;
    
    if n < 3 {
        return Err("Invalid SOCKS5 request".into());
    }
    
    // 检查 SOCKS5 版本
    if buffer[0] != 0x05 {
        return Err("Unsupported SOCKS version".into());
    }
    
    // 发送 SOCKS5 响应
    let response = [0x05, 0x00]; // SOCKS5, No authentication required
    client_stream.write_all(&response).await?;
    
    // 读取连接请求
    let n = client_stream.read(&mut buffer).await?;
    if n < 10 {
        return Err("Invalid SOCKS5 connect request".into());
    }
    
    // 解析目标地址
    let addr_type = buffer[3];
    let target = match addr_type {
        0x01 => { // IPv4
            let ip = [buffer[4], buffer[5], buffer[6], buffer[7]];
            let port = u16::from_be_bytes([buffer[8], buffer[9]]);
            format!("{}:{}", std::net::Ipv4Addr::from(ip), port)
        }
        0x03 => { // Domain name
            let domain_len = buffer[4] as usize;
            if n < 5 + domain_len + 2 {
                return Err("Invalid domain name length".into());
            }
            let domain = String::from_utf8_lossy(&buffer[5..5+domain_len]);
            let port = u16::from_be_bytes([buffer[5+domain_len], buffer[5+domain_len+1]]);
            format!("{}:{}", domain, port)
        }
        _ => return Err("Unsupported address type".into()),
    };
    
    debug!("SOCKS5 target: {}", target);
    
    // 连接到 AnyTLS 服务器
    let mut server_stream = TcpStream::connect(&server_addr).await?;
    
    // 发送 AnyTLS 认证数据
    let mut auth_data = Vec::new();
    auth_data.extend_from_slice(&password_sha256);
    auth_data.extend_from_slice(&[0u8; 2]); // 无填充
    server_stream.write_all(&auth_data).await?;
    
    // 发送目标地址信息到服务器（使用简单的格式）
    let target_bytes = target.as_bytes();
    let target_len = target_bytes.len() as u16;
    server_stream.write_all(&target_len.to_be_bytes()).await?;
    server_stream.write_all(target_bytes).await?;
    
    // 发送 SOCKS5 连接成功响应给客户端
    let success_response = [0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    client_stream.write_all(&success_response).await?;
    
    // 开始数据转发
    let (mut client_read, mut client_write) = client_stream.split();
    let (mut server_read, mut server_write) = server_stream.split();
    
    // 双向数据转发
    let client_to_server = futures::io::copy(&mut client_read, &mut server_write);
    let server_to_client = futures::io::copy(&mut server_read, &mut client_write);
    
    futures::future::select(client_to_server, server_to_client).await;
    
    Ok(())
}