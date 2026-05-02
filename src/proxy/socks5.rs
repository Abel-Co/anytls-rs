use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, Copy)]
pub enum AddressType {
    Ipv4,
    Domain,
    Ipv6,
}

#[derive(Debug, Clone)]
pub struct ConnectRequest {
    pub atyp: AddressType,
    pub host: String,
    pub port: u16,
}

pub async fn accept_no_auth<S>(stream: &mut S) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported SOCKS version"));
    }
    let n_methods = head[1] as usize;
    let mut methods = vec![0u8; n_methods];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&0x00) {
        stream.write_all(&[0x05, 0xFF]).await?;
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "no acceptable auth method",
        ));
    }
    stream.write_all(&[0x05, 0x00]).await?;
    Ok(())
}

pub async fn read_connect_request<S>(stream: &mut S) -> io::Result<ConnectRequest>
where
    S: AsyncRead + Unpin,
{
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported SOCKS version"));
    }
    if head[1] != 0x01 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "unsupported SOCKS command"));
    }
    let atyp = match head[3] {
        0x01 => AddressType::Ipv4,
        0x03 => AddressType::Domain,
        0x04 => AddressType::Ipv6,
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported address type")),
    };

    let host = match atyp {
        AddressType::Ipv4 => {
            let mut ip = [0u8; 4];
            stream.read_exact(&mut ip).await?;
            format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
        }
        AddressType::Domain => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut domain).await?;
            String::from_utf8(domain)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid domain utf8"))?
        }
        AddressType::Ipv6 => {
            let mut ip = [0u8; 16];
            stream.read_exact(&mut ip).await?;
            std::net::Ipv6Addr::from(ip).to_string()
        }
    };

    let mut port = [0u8; 2];
    stream.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);

    Ok(ConnectRequest { atyp, host, port })
}

pub async fn write_success_reply<S>(stream: &mut S) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    // VER=5 REP=0 RSV=0 ATYP=IPv4 BND.ADDR=0.0.0.0 BND.PORT=0
    stream.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await
}

pub fn build_socks_addr(req: &ConnectRequest) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(32);
    match req.atyp {
        AddressType::Ipv4 => {
            out.push(0x01);
            let ip: std::net::Ipv4Addr = req
                .host
                .parse()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("invalid IPv4: {e}")))?;
            out.extend_from_slice(&ip.octets());
        }
        AddressType::Domain => {
            out.push(0x03);
            if req.host.len() > u8::MAX as usize {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "domain too long"));
            }
            out.push(req.host.len() as u8);
            out.extend_from_slice(req.host.as_bytes());
        }
        AddressType::Ipv6 => {
            out.push(0x04);
            let ip: std::net::Ipv6Addr = req
                .host
                .parse()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("invalid IPv6: {e}")))?;
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&req.port.to_be_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_socks_addr_domain() {
        let req = ConnectRequest {
            atyp: AddressType::Domain,
            host: "www.google.com".to_string(),
            port: 443,
        };
        let out = build_socks_addr(&req).unwrap();
        assert_eq!(out[0], 0x03);
        assert_eq!(out[1] as usize, "www.google.com".len());
        assert_eq!(&out[2..2 + "www.google.com".len()], b"www.google.com");
        assert_eq!(&out[out.len() - 2..], &443u16.to_be_bytes());
    }

    #[test]
    fn test_build_socks_addr_ipv4() {
        let req = ConnectRequest {
            atyp: AddressType::Ipv4,
            host: "1.2.3.4".to_string(),
            port: 8080,
        };
        let out = build_socks_addr(&req).unwrap();
        assert_eq!(out, vec![0x01, 1, 2, 3, 4, 0x1f, 0x90]);
    }
}
