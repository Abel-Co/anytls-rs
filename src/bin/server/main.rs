mod auth;
mod registry;
mod stream_handler;

use anytls_rs::proxy::padding::DefaultPaddingFactory;
use anytls_rs::proxy::session::{Session, Stream};
use anytls_rs::util::mkcert;
use anytls_rs::PROGRAM_VERSION_NAME;
use clap::Parser;
use log::{debug, error, info};
use registry::SessionRegistry;
use std::sync::Arc;
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

    #[arg(long, default_value_t = 30, help = "Idle session timeout in seconds")]
    idle_session_timeout: u64,

    #[arg(long, default_value_t = 1, help = "Keep at least N idle sessions")]
    min_idle_session: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    if args.password.is_empty() {
        error!("Please set password");
        std::process::exit(1);
    }

    let expected_password = auth::password_sha256(&args.password);

    info!("[Server] {}", PROGRAM_VERSION_NAME);
    info!("[Server] Listening TCP {}", args.listen);

    let listener = TcpListener::bind(&args.listen).await?;
    let tls_config = Arc::new(mkcert::generate_key_pair("localhost")?);
    let tls_acceptor = TlsAcceptor::from(tls_config);
    let padding = DefaultPaddingFactory::load();
    let registry = SessionRegistry::new();
    let session_seq = Arc::new(std::sync::atomic::AtomicU64::new(1));

    registry.spawn_idle_cleanup(args.idle_session_timeout * 1000, args.min_idle_session);

    loop {
        let (stream, peer) = listener.accept().await?;
        let tls_acceptor = tls_acceptor.clone();
        let padding = padding.clone();
        let registry = registry.clone();
        let expected = expected_password;
        let session_id = session_seq.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(
                stream,
                tls_acceptor,
                expected,
                padding,
                registry,
                session_id,
            )
            .await
            {
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
    registry: SessionRegistry,
    session_id: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut tls_stream = acceptor.accept(stream).await?;
    if !auth::authenticate(&mut tls_stream, expected_password).await? {
        return Ok(());
    }

    info!("[Server] Authentication successful");

    let on_new_stream: Arc<dyn Fn(Stream) + Send + Sync> = Arc::new(|stream| {
        tokio::spawn(async move {
            if let Err(e) = stream_handler::handle_stream(stream).await {
                debug!("[Server] Stream handler error: {}", e);
            }
        });
    });

    let on_close = registry.make_on_close(session_id);

    let session = Arc::new(Session::new_server(
        Box::new(tls_stream),
        Some(on_new_stream),
        Some(on_close),
        padding,
    ));
    registry.insert(session_id, Arc::clone(&session)).await;
    session.run().await?;
    Ok(())
}
