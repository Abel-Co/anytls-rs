use crate::proxy::addr_codec::{
    build_socks_addr as build_socks_addr_raw, read_socks_addr_with_atyp, AddressType, SocksAddr,
};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone)]
pub struct SocksRequest {
    pub command: u8,
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

pub async fn read_request<S>(stream: &mut S) -> io::Result<SocksRequest>
where
    S: AsyncRead + Unpin,
{
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported SOCKS version"));
    }
    let command = head[1];
    if command != 0x01 && command != 0x03 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "unsupported SOCKS command"));
    }
    match head[3] {
        0x01 | 0x03 | 0x04 => {}
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported address type")),
    }

    let addr = read_socks_addr_with_atyp(stream, head[3]).await?;
    Ok(SocksRequest {
        command,
        atyp: addr.atyp,
        host: addr.host,
        port: addr.port,
    })
}

pub async fn write_success_reply<S>(stream: &mut S) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    // VER=5 REP=0 RSV=0 ATYP=IPv4 BND.ADDR=0.0.0.0 BND.PORT=0
    stream.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await
}

pub fn build_socks_addr(req: &SocksRequest) -> io::Result<Vec<u8>> {
    build_socks_addr_raw(&SocksAddr {
        atyp: req.atyp,
        host: req.host.clone(),
        port: req.port,
    })
}

pub async fn write_command_not_supported_reply<S>(stream: &mut S) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    // VER=5 REP=7 command not supported RSV=0 ATYP=IPv4 BND.ADDR=0.0.0.0 BND.PORT=0
    stream.write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await
}

pub async fn write_udp_associate_reply<S>(stream: &mut S, bind_port: u16) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    let port = bind_port.to_be_bytes();
    // VER=5 REP=0 RSV=0 ATYP=IPv4 BND.ADDR=0.0.0.0 BND.PORT=bind_port
    stream
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, port[0], port[1]])
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_socks_addr_domain() {
        let req = SocksRequest {
            command: 0x01,
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
        let req = SocksRequest {
            command: 0x01,
            atyp: AddressType::Ipv4,
            host: "1.2.3.4".to_string(),
            port: 8080,
        };
        let out = build_socks_addr(&req).unwrap();
        assert_eq!(out, vec![0x01, 1, 2, 3, 4, 0x1f, 0x90]);
    }
}
