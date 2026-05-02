use anytls_rs::proxy::padding::DefaultPaddingFactory;
use anytls_rs::proxy::client_runtime;
use anytls_rs::proxy::session::Client;
use anytls_rs::proxy::transport;
use clap::Parser;
use log::{error, info};
use tokio::net::TcpListener;
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
    info!("[Client] Session padding enabled: true");
    
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
                    if let Err(e) = client_runtime::handle_client_connection(client_conn, client_clone).await {
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
