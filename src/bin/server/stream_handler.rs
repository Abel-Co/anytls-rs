use anytls_rs::proxy::addr_codec::read_socks_addr;
use anytls_rs::proxy::session::Stream;
use tokio::net::TcpStream;

pub(crate) async fn handle_stream(mut stream: Stream) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let target = read_socks_addr(&mut stream).await?.to_host_port();
    log::info!("[Server] Proxy to {}", target);

    let target_conn = TcpStream::connect(&target).await?;
    let (mut target_read, mut target_write) = target_conn.into_split();
    let (mut stream_read, mut stream_write) = stream.split();

    let uplink = async {
        if let Err(e) = tokio::io::copy(&mut stream_read, &mut target_write).await {
            log::debug!("[Server] uplink error: {}", e);
        }
    };
    let downlink = async {
        if let Err(e) = tokio::io::copy(&mut target_read, &mut stream_write).await {
            log::debug!("[Server] downlink error: {}", e);
        }
    };
    tokio::join!(uplink, downlink);
    Ok(())
}
