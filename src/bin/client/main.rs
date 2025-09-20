use anytls_rs::proxy::padding::DefaultPaddingFactory;
use anytls_rs::proxy::session::Client;
use anytls_rs::util::PROGRAM_VERSION_NAME;
use clap::Parser;
use log::{error, info};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;
use rustls::ClientConfig;
use sha2::{Digest, Sha256};

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
    env_logger::init();
    
    let args = Args::parse();
    
    if args.password.is_empty() {
        error!("Please set password");
        std::process::exit(1);
    }
    
    let password_sha256 = Sha256::digest(args.password.as_bytes());
    
    info!("[Client] {}", PROGRAM_VERSION_NAME);
    info!("[Client] SOCKS5 {} => {}", args.listen, args.server);
    
    let listener = TcpListener::bind(&args.listen).await?;
    
    let tls_config = create_tls_config(args.sni.as_deref())?;
    let padding = DefaultPaddingFactory::load();
    
    let padding_clone = padding.clone();
    let client = Arc::new(Client::new(
        Box::new(move || {
            let server = args.server.clone();
            let tls_config = tls_config.clone();
            let password_sha256 = password_sha256.clone();
            let padding = padding_clone.clone();
            
            Box::new(Box::pin(async move {
                let stream = TcpStream::connect(&server).await?;
                let connector = TlsConnector::from(tls_config);
                let mut tls_stream = connector.connect("127.0.0.1".try_into().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?, stream).await?;
                
                // Send authentication
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
                
                // Send auth data
                tls_stream.write_all(&auth_data).await?;
                
                Ok(Box::new(tls_stream) as Box<dyn anytls_rs::util::r#type::AsyncReadWrite>)
            }))
        }),
        padding,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_secs(30),
        5,
    ));
    
    loop {
        let (stream, _addr) = listener.accept().await?;
        let client = client.clone();
        
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, client).await {
                error!("Connection error: {}", e);
            }
        });
    }
}

fn create_tls_config(_sni: Option<&str>) -> Result<Arc<ClientConfig>, Box<dyn std::error::Error>> {
    let config = ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    
    Ok(Arc::new(config))
}

async fn handle_connection(
    mut stream: TcpStream,
    client: Arc<Client>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Simplified SOCKS5 handling
    // In a real implementation, you'd implement full SOCKS5 protocol
    
    let _proxy_stream = client.create_stream().await?;
    
    // Simple relay - simplified implementation
    let (mut client_read, mut client_write) = stream.split();
    
    // For now, just echo back what we receive
    let mut buffer = vec![0u8; 1024];
    loop {
        let n = client_read.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        client_write.write_all(&buffer[..n]).await?;
    }
    
    Ok(())
}