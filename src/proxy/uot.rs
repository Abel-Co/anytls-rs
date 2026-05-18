use crate::proxy::addr_codec::{read_socks_addr, AddressType, SocksAddr};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u8 = 2;
pub const MAGIC_ADDRESS: &str = "sp.v2.udp-over-tcp.arpa";

#[derive(Debug, Clone)]
pub struct Request {
    pub is_connect: bool,
    pub destination: SocksAddr,
}

pub async fn read_request<S>(stream: &mut S) -> io::Result<Request>
where
    S: AsyncRead + Unpin,
{
    let is_connect = stream.read_u8().await? != 0;
    let destination = read_socks_addr(stream).await?;
    Ok(Request {
        is_connect,
        destination,
    })
}

pub async fn write_request<S>(stream: &mut S, request: &Request) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    stream.write_u8(if request.is_connect { 1 } else { 0 }).await?;
    write_socks_addr(stream, &request.destination).await?;
    Ok(())
}

async fn write_socks_addr<S>(stream: &mut S, addr: &SocksAddr) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    match addr.atyp {
        AddressType::Ipv4 => {
            stream.write_u8(0x01).await?;
            let ip: std::net::Ipv4Addr = addr.host.parse().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("invalid IPv4: {e}"))
            })?;
            stream.write_all(&ip.octets()).await?;
        }
        AddressType::Domain => {
            if addr.host.len() > u8::MAX as usize {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "domain too long"));
            }
            stream.write_u8(0x03).await?;
            stream.write_u8(addr.host.len() as u8).await?;
            stream.write_all(addr.host.as_bytes()).await?;
        }
        AddressType::Ipv6 => {
            stream.write_u8(0x04).await?;
            let ip: std::net::Ipv6Addr = addr.host.parse().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("invalid IPv6: {e}"))
            })?;
            stream.write_all(&ip.octets()).await?;
        }
    }
    stream.write_u16(addr.port).await
}

pub async fn read_uot_addr_port<S>(stream: &mut S) -> io::Result<SocksAddr>
where
    S: AsyncRead + Unpin,
{
    let atyp = stream.read_u8().await?;
    match atyp {
        0x00 => {
            let mut ip = [0u8; 4];
            stream.read_exact(&mut ip).await?;
            let port = stream.read_u16().await?;
            Ok(SocksAddr {
                atyp: AddressType::Ipv4,
                host: format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
                port,
            })
        }
        0x01 => {
            let mut ip = [0u8; 16];
            stream.read_exact(&mut ip).await?;
            let port = stream.read_u16().await?;
            Ok(SocksAddr {
                atyp: AddressType::Ipv6,
                host: std::net::Ipv6Addr::from(ip).to_string(),
                port,
            })
        }
        0x02 => {
            let len = stream.read_u8().await? as usize;
            let mut domain = vec![0u8; len];
            stream.read_exact(&mut domain).await?;
            let port = stream.read_u16().await?;
            let host = String::from_utf8(domain)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid uot fqdn"))?;
            Ok(SocksAddr {
                atyp: AddressType::Domain,
                host,
                port,
            })
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported uot address family",
        )),
    }
}

pub async fn write_uot_addr_port<S>(stream: &mut S, addr: &SocksAddr) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    match addr.atyp {
        AddressType::Ipv4 => {
            stream.write_u8(0x00).await?;
            let ip: std::net::Ipv4Addr = addr.host.parse().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("invalid IPv4: {e}"))
            })?;
            stream.write_all(&ip.octets()).await?;
        }
        AddressType::Ipv6 => {
            stream.write_u8(0x01).await?;
            let ip: std::net::Ipv6Addr = addr.host.parse().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("invalid IPv6: {e}"))
            })?;
            stream.write_all(&ip.octets()).await?;
        }
        AddressType::Domain => {
            if addr.host.len() > u8::MAX as usize {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "domain too long"));
            }
            stream.write_u8(0x02).await?;
            stream.write_u8(addr.host.len() as u8).await?;
            stream.write_all(addr.host.as_bytes()).await?;
        }
    }
    stream.write_u16(addr.port).await
}
