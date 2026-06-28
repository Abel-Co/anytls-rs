use anytls_rs::proxy::addr_codec::read_socks_addr;
use anytls_rs::proxy::session::Stream;
use anytls_rs::proxy::uot;
use tokio::io::copy_bidirectional;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::UdpSocket;

const UOT_DEST_HOST_SUFFIX: &str = "udp-over-tcp.arpa";

async fn handle_uot_stream(
    mut stream: Stream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let request = uot::read_request(&mut stream).await?;
    log::debug!(
        "[Server][UOT] request is_connect={}, destination={}:{}",
        request.is_connect,
        request.destination.host,
        request.destination.port
    );
    let is_connect = request.is_connect;
    let fixed_destination = request.destination;
    let udp_socket = UdpSocket::bind("0.0.0.0:0").await?;
    let mut buf = vec![0u8; 65535];

    loop {
        let destination = if is_connect {
            fixed_destination.clone()
        } else {
            match uot::read_uot_addr_port(&mut stream).await {
                Ok(v) => v,
                Err(e) => {
                    log::debug!("[Server][UOT] read addr failed: {}", e);
                    break;
                }
            }
        };

        let len = match stream.read_u16().await {
            Ok(v) => v as usize,
            Err(e) => {
                log::debug!("[Server][UOT] read len failed: {}", e);
                break;
            }
        };
        let mut data = vec![0u8; len];
        if let Err(e) = stream.read_exact(&mut data).await {
            log::debug!("[Server][UOT] read payload failed: {}", e);
            break;
        }
        log::debug!(
            "[Server][UOT] recv datagram {} bytes to {}:{}",
            len,
            destination.host,
            destination.port
        );
        udp_socket
            .send_to(&data, destination.to_host_port())
            .await?;

        let recv_res = tokio::time::timeout(
            std::time::Duration::from_millis(800),
            udp_socket.recv_from(&mut buf),
        )
        .await;
        if let Ok(Ok((n, src))) = recv_res {
            log::debug!("[Server][UOT] udp response {} bytes from {}", n, src);
            if !is_connect {
                let src_addr = match src {
                    std::net::SocketAddr::V4(v4) => anytls_rs::proxy::addr_codec::SocksAddr {
                        atyp: anytls_rs::proxy::addr_codec::AddressType::Ipv4,
                        host: v4.ip().to_string(),
                        port: v4.port(),
                    },
                    std::net::SocketAddr::V6(v6) => anytls_rs::proxy::addr_codec::SocksAddr {
                        atyp: anytls_rs::proxy::addr_codec::AddressType::Ipv6,
                        host: v6.ip().to_string(),
                        port: v6.port(),
                    },
                };
                uot::write_uot_addr_port(&mut stream, &src_addr).await?;
            }
            stream.write_u16(n as u16).await?;
            stream.write_all(&buf[..n]).await?;
            stream.flush().await?;
            log::debug!("[Server][UOT] flushed response");
        }
    }

    Ok(())
}

pub(crate) async fn handle_stream(
    mut stream: Stream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let target = read_socks_addr(&mut stream).await?.to_host_port();
    log::info!("[Server] Proxy to {}", target);

    if target.contains(UOT_DEST_HOST_SUFFIX) {
        return handle_uot_stream(stream).await;
    }

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
