use anytls_rs::proxy::padding::DefaultPaddingFactory;
use anytls_rs::proxy::session::Session;
use anytls_rs::util::{mkcert, PROGRAM_VERSION_NAME};
use clap::Parser;
use log::{debug, error, info};
use sha2::{Digest, Sha256};
use std::sync::Arc;
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
    
    loop {
        let (stream, _addr) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let password_sha256 = password_sha256.clone();
        let padding = padding.clone();
        
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, acceptor, password_sha256.to_vec(), padding).await {
                debug!("Connection error: {}", e);
            }
        });
    }
}

fn create_tls_config() -> Result<Arc<ServerConfig>, Box<dyn std::error::Error>> {
    let cert = mkcert::generate_key_pair("")?;
    Ok(Arc::new(cert))
}

async fn handle_connection(
    stream: TcpStream,
    acceptor: TlsAcceptor,
    password_sha256: Vec<u8>,
    padding: Arc<tokio::sync::RwLock<anytls_rs::proxy::padding::PaddingFactory>>,
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
        Box::new(|stream| {
            // Handle new stream
            tokio::spawn(async move {
                if let Err(e) = handle_stream(stream).await {
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
) -> Result<(), Box<dyn std::error::Error>> {
    // Simplified stream handling
    // In a real implementation, you'd implement the full proxy logic
    
    // Read destination address (SOCKS format)
    let mut addr_buf = vec![0u8; 256];
    let n = stream.read(&mut addr_buf).await?;
    addr_buf.truncate(n);
    
    // Parse destination and create outbound connection
    // This is simplified - in reality you'd parse SOCKS address format
    let destination = "example.com:80"; // Simplified
    
    let mut outbound = TcpStream::connect(destination).await?;
    
    // Report success
    stream.handshake_success().await?;
    
    // Relay data - simplified implementation
    let (mut stream_read, mut stream_write) = stream.split_ref();
    let (mut outbound_read, mut outbound_write) = outbound.split();
    
    tokio::select! {
        _ = tokio::io::copy(&mut stream_read, &mut outbound_write) => {},
        _ = tokio::io::copy(&mut outbound_read, &mut stream_write) => {},
    }
    
    Ok(())
}