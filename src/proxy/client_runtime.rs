use crate::proxy::session::Client;
use crate::proxy::socks5;
use log::{error, info};
use tokio::io::AsyncWriteExt;
use tokio::io::copy_bidirectional;
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

    match copy_bidirectional(&mut client_conn, &mut anytls_stream).await {
        Ok((c2t, t2c)) => {
            info!(
                "[Client] Bidirectional copy completed: client->target={} bytes, target->client={} bytes",
                c2t, t2c
            );
        }
        Err(e) => {
            error!("[Client] Bidirectional copy error: {}", e);
        }
    }

    Ok(())
}
