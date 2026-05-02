use anytls_rs::proxy::session::Stream;
use std::io;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

pub async fn handle_stream(mut stream: Stream) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let target = read_socks_addr(&mut stream).await?;
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

pub async fn read_socks_addr<S>(stream: &mut S) -> Result<String, Box<dyn std::error::Error + Send + Sync>>
where
    S: AsyncRead + Unpin,
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
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported atyp").into()),
    };
    let mut port = [0u8; 2];
    stream.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);
    Ok(format!("{}:{}", host, port))
}

