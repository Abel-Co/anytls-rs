use anytls_rs::proxy::padding::DefaultPaddingFactory;
use anytls_rs::proxy::session::{Session, Stream};
use anytls_rs::util::mkcert;
use anytls_rs::PROGRAM_VERSION_NAME;
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    if args.password.is_empty() {
        error!("Please set password");
        std::process::exit(1);
    }

    let password_sha256: [u8; 32] = Sha256::digest(args.password.as_bytes()).into();

    info!("[Server] {}", PROGRAM_VERSION_NAME);
    info!("[Server] Listening TCP {}", args.listen);

    let listener = TcpListener::bind(&args.listen).await?;
    let tls_config = Arc::new(mkcert::generate_key_pair("localhost")?);
    let tls_acceptor = TlsAcceptor::from(tls_config);
    let padding = DefaultPaddingFactory::load();

    loop {
        let (stream, peer) = listener.accept().await?;
        let tls_acceptor = tls_acceptor.clone();
        let padding = padding.clone();
        let expected = password_sha256;
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, tls_acceptor, expected, padding).await {
                debug!("[Server] Connection {} error: {}", peer, e);
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    acceptor: TlsAcceptor,
    expected_password: [u8; 32],
    padding: Arc<anytls_rs::proxy::padding::PaddingFactory>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut tls_stream = acceptor.accept(stream).await?;

    // Auth: sha256(password) + padding_len + padding0
    let mut auth_head = [0u8; 34];
    tls_stream.read_exact(&mut auth_head).await?;
    if auth_head[..32] != expected_password {
        return Ok(());
    }
    let padding_len = u16::from_be_bytes([auth_head[32], auth_head[33]]) as usize;
    if padding_len > 0 {
        let mut discard = vec![0u8; padding_len];
        tls_stream.read_exact(&mut discard).await?;
    }

    info!("[Server] Authentication successful");

    let on_new_stream: Arc<dyn Fn(Stream) + Send + Sync> = Arc::new(|stream| {
        tokio::spawn(async move {
            if let Err(e) = handle_stream(stream).await {
                debug!("[Server] Stream handler error: {}", e);
            }
        });
    });

    let session = Arc::new(Session::new_server(
        Box::new(tls_stream),
        Some(on_new_stream),
        padding,
    ));
    session.run().await?;
    Ok(())
}

async fn handle_stream(mut stream: Stream) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let target = read_socks_addr(&mut stream).await?;
    info!("[Server] Proxy to {}", target);

    let target_conn = TcpStream::connect(&target).await?;
    let (mut target_read, mut target_write) = target_conn.into_split();
    let (mut stream_read, mut stream_write) = stream.split();

    let uplink = async {
        if let Err(e) = tokio::io::copy(&mut stream_read, &mut target_write).await {
            debug!("[Server] uplink error: {}", e);
        }
    };
    let downlink = async {
        if let Err(e) = tokio::io::copy(&mut target_read, &mut stream_write).await {
            debug!("[Server] downlink error: {}", e);
        }
    };
    tokio::join!(uplink, downlink);
    Ok(())
}

async fn read_socks_addr<S>(stream: &mut S) -> Result<String, Box<dyn std::error::Error + Send + Sync>>
where
    S: AsyncReadExt + Unpin,
{
    let mut atyp = [0u8; 1];
    stream.read_exact(&mut atyp).await?;
    let host = match atyp[0] {
        0x01 => {
            let mut ip = [0u8; 4];
            stream.read_exact(&mut ip).await?;
            format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut domain).await?;
            String::from_utf8(domain)?
        }
        0x04 => {
            let mut ip = [0u8; 16];
            stream.read_exact(&mut ip).await?;
            std::net::Ipv6Addr::from(ip).to_string()
        }
        _ => return Err("unsupported atyp".into()),
    };
    let mut port = [0u8; 2];
    stream.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);
    Ok(format!("{}:{}", host, port))
}
