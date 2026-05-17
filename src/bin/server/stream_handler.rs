use anytls_rs::proxy::addr_codec::read_socks_addr;
use anytls_rs::proxy::session::Stream;
use tokio::io::copy_bidirectional;
use tokio::net::TcpStream;

pub(crate) async fn handle_stream(mut stream: Stream) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let target = read_socks_addr(&mut stream).await?.to_host_port();
    log::info!("[Server] Proxy to {}", target);

    let mut target_conn = TcpStream::connect(&target).await?;
    match copy_bidirectional(&mut stream, &mut target_conn).await {
        Ok((up, down)) => {
            log::debug!(
                "[Server] relay completed: stream->target={} bytes, target->stream={} bytes",
                up, down
            );
        }
        Err(e) => {
            log::debug!("[Server] relay error: {}", e);
        }
    }
    Ok(())
}
