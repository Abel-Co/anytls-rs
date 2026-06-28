use crate::proxy::session::Client;
use crate::proxy::socks5;
use crate::proxy::uot;
use log::{error, info};
use tokio::io::copy_bidirectional;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::UdpSocket;

const UOT_DEST_HOST: &str = uot::MAGIC_ADDRESS;
const UOT_DEST_PORT: u16 = 443;

fn parse_socks5_udp_packet(
    pkt: &[u8],
) -> Result<(crate::proxy::addr_codec::SocksAddr, usize), Box<dyn std::error::Error>> {
    if pkt.len() < 4 {
        return Err("short udp packet".into());
    }
    if pkt[2] != 0 {
        return Err("fragmented udp is not supported".into());
    }
    let atyp = pkt[3];
    let mut idx = 4usize;
    let addr = match atyp {
        0x01 => {
            if pkt.len() < idx + 4 + 2 {
                return Err("short ipv4 udp header".into());
            }
            let ip = std::net::Ipv4Addr::new(pkt[idx], pkt[idx + 1], pkt[idx + 2], pkt[idx + 3]);
            idx += 4;
            let port = u16::from_be_bytes([pkt[idx], pkt[idx + 1]]);
            idx += 2;
            crate::proxy::addr_codec::SocksAddr {
                atyp: crate::proxy::addr_codec::AddressType::Ipv4,
                host: ip.to_string(),
                port,
            }
        }
        0x04 => {
            if pkt.len() < idx + 16 + 2 {
                return Err("short ipv6 udp header".into());
            }
            let mut ipb = [0u8; 16];
            ipb.copy_from_slice(&pkt[idx..idx + 16]);
            idx += 16;
            let port = u16::from_be_bytes([pkt[idx], pkt[idx + 1]]);
            idx += 2;
            crate::proxy::addr_codec::SocksAddr {
                atyp: crate::proxy::addr_codec::AddressType::Ipv6,
                host: std::net::Ipv6Addr::from(ipb).to_string(),
                port,
            }
        }
        0x03 => {
            if pkt.len() < idx + 1 {
                return Err("short domain len".into());
            }
            let len = pkt[idx] as usize;
            idx += 1;
            if pkt.len() < idx + len + 2 {
                return Err("short domain udp header".into());
            }
            let host = String::from_utf8(pkt[idx..idx + len].to_vec())?;
            idx += len;
            let port = u16::from_be_bytes([pkt[idx], pkt[idx + 1]]);
            idx += 2;
            crate::proxy::addr_codec::SocksAddr {
                atyp: crate::proxy::addr_codec::AddressType::Domain,
                host,
                port,
            }
        }
        _ => return Err("unsupported udp atyp".into()),
    };
    if pkt.len() < idx {
        return Err("short udp header".into());
    }
    Ok((addr, idx))
}

fn build_socks5_udp_packet(addr: &crate::proxy::addr_codec::SocksAddr, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + payload.len());
    out.extend_from_slice(&[0, 0, 0]); // RSV RSV FRAG
    match addr.atyp {
        crate::proxy::addr_codec::AddressType::Ipv4 => {
            out.push(0x01);
            if let Ok(ip) = addr.host.parse::<std::net::Ipv4Addr>() {
                out.extend_from_slice(&ip.octets());
            } else {
                out.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
        crate::proxy::addr_codec::AddressType::Ipv6 => {
            out.push(0x04);
            if let Ok(ip) = addr.host.parse::<std::net::Ipv6Addr>() {
                out.extend_from_slice(&ip.octets());
            } else {
                out.extend_from_slice(&[0u8; 16]);
            }
        }
        crate::proxy::addr_codec::AddressType::Domain => {
            out.push(0x03);
            let b = addr.host.as_bytes();
            let l = b.len().min(255);
            out.push(l as u8);
            out.extend_from_slice(&b[..l]);
        }
    }
    out.extend_from_slice(&addr.port.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

async fn handle_udp_associate(
    mut client_conn: TcpStream,
    client: Client,
) -> Result<(), Box<dyn std::error::Error>> {
    let udp_socket = UdpSocket::bind("0.0.0.0:0").await?;
    let bind_port = udp_socket.local_addr()?.port();
    socks5::write_udp_associate_reply(&mut client_conn, bind_port).await?;
    info!("[Client] UDP associate established on 0.0.0.0:{}", bind_port);

    let mut stream = client.create_stream().await?;
    let uot_req = socks5::SocksRequest {
        command: 0x01,
        atyp: crate::proxy::addr_codec::AddressType::Domain,
        host: UOT_DEST_HOST.to_string(),
        port: UOT_DEST_PORT,
    };
    let target_socks_addr = socks5::build_socks_addr(&uot_req)?;
    stream.write_all(&target_socks_addr).await?;
    stream.flush().await?;
    let uot_request = uot::Request {
        is_connect: false,
        destination: crate::proxy::addr_codec::SocksAddr {
            atyp: crate::proxy::addr_codec::AddressType::Ipv4,
            host: "0.0.0.0".to_string(),
            port: 0,
        },
    };
    uot::write_request(&mut stream, &uot_request).await?;
    stream.flush().await?;

    let (mut stream_r, mut stream_w) = stream.split();
    let mut tcp_probe = [0u8; 1];

    let relay = async {
        let mut last_peer = None;
        let mut buf = vec![0u8; 65535];
        loop {
            tokio::select! {
                addr = uot::read_uot_addr_port(&mut stream_r) => {
                    let addr = match addr {
                        Ok(a) => a,
                        Err(e) => {
                            log::debug!("[Client][UOT] read addr failed: {}", e);
                            break;
                        },
                    };
                    let len = match stream_r.read_u16().await {
                        Ok(v) => v as usize,
                        Err(e) => {
                            log::debug!("[Client][UOT] read len failed: {}", e);
                            break;
                        },
                    };
                    let mut data = vec![0u8; len];
                    if let Err(e) = stream_r.read_exact(&mut data).await {
                        log::debug!("[Client][UOT] read payload failed: {}", e);
                        break;
                    }
                    log::debug!("[Client][UOT] recv datagram {} bytes from {}:{}", len, addr.host, addr.port);
                    let pkt = build_socks5_udp_packet(&addr, &data);
                    if let Some(peer) = last_peer {
                        udp_socket.send_to(&pkt, peer).await?;
                        log::debug!("[Client][UOT] delivered to socks5 udp peer {}", peer);
                    }
                }
                recv = udp_socket.recv_from(&mut buf) => {
                    let (n, peer) = recv?;
                    last_peer = Some(peer);
                    let (addr, payload_offset) = parse_socks5_udp_packet(&buf[..n])?;
                    let payload = &buf[payload_offset..n];
                    log::debug!(
                        "[Client][UOT] send datagram {} bytes to {}:{}",
                        payload.len(),
                        addr.host,
                        addr.port
                    );
                    uot::write_uot_addr_port(&mut stream_w, &addr).await?;
                    stream_w.write_u16(payload.len() as u16).await?;
                    stream_w.write_all(payload).await?;
                    stream_w.flush().await?;
                }
                n = client_conn.read(&mut tcp_probe) => {
                    match n {
                        Ok(0) => break,
                        Ok(_) => continue,
                        Err(_) => break,
                    };
                }
            }
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    };

    relay.await
}

pub async fn handle_client_connection(
    mut client_conn: TcpStream,
    client: Client,
) -> Result<(), Box<dyn std::error::Error>> {
    socks5::accept_no_auth(&mut client_conn).await?;
    let req = socks5::read_request(&mut client_conn).await?;
    if req.command == 0x03 {
        return handle_udp_associate(client_conn, client).await;
    }
    if req.command != 0x01 {
        socks5::write_command_not_supported_reply(&mut client_conn).await?;
        return Ok(());
    }

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
