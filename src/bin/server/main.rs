use anytls_rs::proxy::padding::DefaultPaddingFactory;
use anytls_rs::util::PROGRAM_VERSION_NAME;
use clap::Parser;
use log::{debug, error, info};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use futures::{AsyncReadExt};
use glommio::net::{TcpListener, TcpStream};

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
    
    // Load padding scheme if provided
    if let Some(padding_file) = args.padding_scheme {
        let content = std::fs::read(&padding_file)?;
        if DefaultPaddingFactory::update(&content).await {
            info!("Loaded padding scheme file: {}", padding_file);
        } else {
            error!("Wrong format padding scheme file: {}", padding_file);
            std::process::exit(1);
        }
    }
    
    info!("[Server] {}", PROGRAM_VERSION_NAME);
    info!("[Server] Listening TCP {}", args.listen);
    
    let listener = TcpListener::bind(&args.listen)?;
    
    let padding = DefaultPaddingFactory::load();
    
    loop {
        let stream = listener.accept().await?;
        let password_sha256 = password_sha256.clone();
        let padding = padding.clone();
        
        glommio::spawn_local(async move {
            if let Err(e) = handle_connection(stream, password_sha256.to_vec(), padding).await {
                debug!("Connection error: {}", e);
            }
        }).detach();
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    password_sha256: Vec<u8>,
    _padding: Arc<glommio::sync::RwLock<anytls_rs::proxy::padding::PaddingFactory>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // 读取 AnyTLS 认证数据
    let mut auth_data = vec![0u8; 34]; // 32 bytes password + 2 bytes padding length
    stream.read_exact(&mut auth_data).await?;
    
    let received_password = &auth_data[..32];
    if received_password != password_sha256.as_slice() {
        debug!("Authentication failed");
        return Ok(());
    }
    
    let padding_len = u16::from_be_bytes([auth_data[32], auth_data[33]]);
    if padding_len > 0 {
        let mut padding_data = vec![0u8; padding_len as usize];
        stream.read_exact(&mut padding_data).await?;
    }
    
    debug!("Authentication successful");
    
    // 读取目标地址长度
    let mut len_buf = [0u8; 2];
    stream.read_exact(&mut len_buf).await?;
    let target_len = u16::from_be_bytes(len_buf) as usize;
    
    // 读取目标地址
    let mut target_buf = vec![0u8; target_len];
    stream.read_exact(&mut target_buf).await?;
    let target = String::from_utf8(target_buf)?;
    
    debug!("Connecting to target: {}", target);
    
    // 连接到目标服务器
    let target_stream = TcpStream::connect(&target).await?;
    
    // 开始数据转发
    let (mut client_read, mut client_write) = stream.split();
    let (mut target_read, mut target_write) = target_stream.split();
    
    // 双向数据转发
    let client_to_target = futures::io::copy(&mut client_read, &mut target_write);
    let target_to_client = futures::io::copy(&mut target_read, &mut client_write);
    
    futures::future::select(client_to_target, target_to_client).await;
    
    Ok(())
}
