use std::io;
use tokio::io::{AsyncRead, AsyncReadExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressType {
    Ipv4,
    Domain,
    Ipv6,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocksAddr {
    pub atyp: AddressType,
    pub host: String,
    pub port: u16,
}

impl SocksAddr {
    pub fn to_host_port(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

pub async fn read_socks_addr<S>(stream: &mut S) -> io::Result<SocksAddr>
where
    S: AsyncRead + Unpin,
{
    let mut atyp = [0u8; 1];
    stream.read_exact(&mut atyp).await?;
    read_socks_addr_with_atyp(stream, atyp[0]).await
}

pub async fn read_socks_addr_with_atyp<S>(stream: &mut S, atyp_raw: u8) -> io::Result<SocksAddr>
where
    S: AsyncRead + Unpin,
{
    let (atyp, host) = parse_host_by_atyp(stream, atyp_raw).await?;

    let mut port = [0u8; 2];
    stream.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);

    Ok(SocksAddr { atyp, host, port })
}

async fn parse_host_by_atyp<S>(stream: &mut S, atyp_raw: u8) -> io::Result<(AddressType, String)>
where
    S: AsyncRead + Unpin,
{
    match atyp_raw {
        0x01 => {
            let mut ip = [0u8; 4];
            stream.read_exact(&mut ip).await?;
            Ok((AddressType::Ipv4, format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])))
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut domain).await?;
            let host = String::from_utf8(domain)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid domain utf8"))?;
            Ok((AddressType::Domain, host))
        }
        0x04 => {
            let mut ip = [0u8; 16];
            stream.read_exact(&mut ip).await?;
            Ok((AddressType::Ipv6, std::net::Ipv6Addr::from(ip).to_string()))
        }
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported address type")),
    }
}

pub fn build_socks_addr(addr: &SocksAddr) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(32);
    match addr.atyp {
        AddressType::Ipv4 => {
            out.push(0x01);
            let ip: std::net::Ipv4Addr = addr.host.parse().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("invalid IPv4: {e}"))
            })?;
            out.extend_from_slice(&ip.octets());
        }
        AddressType::Domain => {
            out.push(0x03);
            if addr.host.len() > u8::MAX as usize {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "domain too long"));
            }
            out.push(addr.host.len() as u8);
            out.extend_from_slice(addr.host.as_bytes());
        }
        AddressType::Ipv6 => {
            out.push(0x04);
            let ip: std::net::Ipv6Addr = addr.host.parse().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("invalid IPv6: {e}"))
            })?;
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&addr.port.to_be_bytes());
    Ok(out)
}
