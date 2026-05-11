use crate::proxy::session::Client;
use crate::proxy::socks5;
use log::{error, info};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

pub async fn handle_client_connection(
    mut client_conn: TcpStream,
    client: Client,
) -> Result<(), Box<dyn std::error::Error>> {
    socks5::accept_no_auth(&mut client_conn).await?;
    let req = socks5::read_connect_request(&mut client_conn).await?;

    info!("[Client] Connecting to {}:{}", req.host, req.port);

    log::debug!("[Client] Creating AnyTLS stream");
    let mut anytls_stream = client.create_stream().await?;
    log::info!("[Client] AnyTLS stream created successfully");

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

    let (mut client_read, mut client_write) = client_conn.split();
    let (mut anytls_read, mut anytls_write) = anytls_stream.split();

    let client_to_target = async move {
        match tokio::io::copy(&mut client_read, &mut anytls_write).await {
            Ok(bytes) => info!("[Client] Client to target copy completed: {} bytes", bytes),
            Err(e) => error!("[Client] Client to target copy error: {}", e),
        }
    };

    let target_to_client = async move {
        match tokio::io::copy(&mut anytls_read, &mut client_write).await {
            Ok(bytes) => info!("[Client] Target to client copy completed: {} bytes", bytes),
            Err(e) => error!("[Client] Target to client copy error: {}", e),
        }
    };

    tokio::join!(client_to_target, target_to_client);

    Ok(())
}
