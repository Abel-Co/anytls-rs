use sha2::{Digest, Sha256};
use std::io;
use tokio::io::AsyncReadExt;
use tokio_rustls::server::TlsStream;

pub fn password_sha256(password: &str) -> [u8; 32] {
    Sha256::digest(password.as_bytes()).into()
}

pub async fn authenticate(
    tls_stream: &mut TlsStream<tokio::net::TcpStream>,
    expected_password: [u8; 32],
) -> io::Result<bool> {
    // Auth: sha256(password) + padding_len + padding0
    let mut auth_head = [0u8; 34];
    tls_stream.read_exact(&mut auth_head).await?;
    if auth_head[..32] != expected_password {
        return Ok(false);
    }
    let padding_len = u16::from_be_bytes([auth_head[32], auth_head[33]]) as usize;
    if padding_len > 0 {
        let mut discard = vec![0u8; padding_len];
        tls_stream.read_exact(&mut discard).await?;
    }
    Ok(true)
}
