use anytls_rs::proxy::padding::DefaultPaddingFactory;
use anytls_rs::proxy::session::Client;
use anytls_rs::proxy::socks5;
use anytls_rs::proxy::transport;
use clap::Parser;
use log::{error, info};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use std::time::Duration;
use anytls_rs::PROGRAM_VERSION_NAME;

#[derive(Parser)]
#[command(name = "anytls-client")]
#[command(about = "AnyTLS Client")]
struct Args {
    #[arg(short = 'l', long, default_value = "127.0.0.1:1080", help = "SOCKS5 listen port")]
    listen: String,
    
    #[arg(short = 's', long, default_value = "127.0.0.1:8443", help = "Server address")]
    server: String,
    
    #[arg(long, help = "SNI")]
    sni: Option<String>,
    
    #[arg(short = 'p', long, help = "Password")]
    password: String,

    #[arg(long, default_value_t = false, help = "Enable session frame padding (experimental for anytls-go interop)")]
    enable_padding: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    
    let args = Args::parse();
    
    if args.password.is_empty() {
        error!("Please set password");
        std::process::exit(1);
    }
    
    let password_sha256 = transport::password_sha256(&args.password);
    
    info!("[Client] {}", PROGRAM_VERSION_NAME);
    info!("[Client] SOCKS5 {} => {}", args.listen, args.server);
    info!("[Client] Session padding enabled: {}", args.enable_padding);
    
    let listener = TcpListener::bind(&args.listen).await?;
    
    let tls_config = transport::create_tls_config();
    let padding = DefaultPaddingFactory::load();
    
    // 创建客户端
    let dial_out = transport::create_dial_out_func(
        args.server.clone(),
        tls_config,
        args.sni,
        password_sha256,
        padding.clone(),
    );
    let client = Client::new(
        dial_out,
        padding,
        Duration::from_secs(30), // 空闲超时
        1, // 最小空闲连接数
        args.enable_padding,
    );
    
    info!("[Client] Listening on {}", args.listen);
    
    // 监听 SOCKS5 连接
    loop {
        match listener.accept().await {
            Ok((client_conn, addr)) => {
                info!("[Client] New connection from {}", addr);
                
                // 为每个连接创建新的任务
                let client_clone = client.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client_connection(client_conn, client_clone).await {
                        error!("[Client] Connection error: {}", e);
                    }
                });
            }
            Err(e) => {
                error!("[Client] Accept error: {}", e);
            }
        }
    }
}

async fn handle_client_connection(
    mut client_conn: TcpStream,
    client: Client,
) -> Result<(), Box<dyn std::error::Error>> {
    socks5::accept_no_auth(&mut client_conn).await?;
    let req = socks5::read_connect_request(&mut client_conn).await?;

    info!("[Client] Connecting to {}:{}", req.host, req.port);
    
    // 创建 AnyTLS 流
    log::debug!("[Client] Creating AnyTLS stream");
    let (session, mut anytls_stream) = client.create_stream().await?;
    log::info!("[Client] AnyTLS stream created successfully");

    // 按协议要求，Stream 建立后首先发送目标地址（SocksAddr）
    // 服务端收到后才会发起真实出站连接。
    let target_socks_addr = socks5::build_socks_addr(&req)?;
    anytls_stream.write_all(&target_socks_addr).await?;
    anytls_stream.flush().await?;
    log::debug!(
        "[Client] Sent SocksAddr to server: {}:{} ({} bytes)",
        req.host,
        req.port,
        target_socks_addr.len()
    );
    
    socks5::write_success_reply(&mut client_conn).await?;
    log::debug!("[Client] Sent SOCKS5 connection success response");
    
    // 使用真正的并行数据转发
    let (mut client_read, mut client_write) = client_conn.split();
    let (mut anytls_read, mut anytls_write) = anytls_stream.split();
    
    // 使用 tokio::join! 而不是 tokio::spawn 来避免生命周期问题
    let client_to_target = async move {
        match tokio::io::copy(&mut client_read, &mut anytls_write).await {
            Ok(bytes) => {
                info!("[Client] Client to target copy completed: {} bytes", bytes);
            }
            Err(e) => {
                error!("[Client] Client to target copy error: {}", e);
            }
        }
    };
    
    let target_to_client = async move {
        match tokio::io::copy(&mut anytls_read, &mut client_write).await {
            Ok(bytes) => {
                info!("[Client] Target to client copy completed: {} bytes", bytes);
            }
            Err(e) => {
                error!("[Client] Target to client copy error: {}", e);
            }
        }
    };
    
    // 等待两个任务都完成
    tokio::join!(client_to_target, target_to_client);

    if !session.is_closed() {
        client.return_session_to_idle(session).await;
    }

    Ok(())
}
