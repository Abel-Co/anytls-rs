use std::time::Duration;
use glommio::net::TcpStream;
use glommio::timer::timeout;

pub struct SystemDialer;

impl SystemDialer {
    pub async fn dial_context(addr: &str) -> Result<TcpStream, std::io::Error> {
        Ok(timeout(Duration::from_secs(5), TcpStream::connect(addr)).await?)
    }
}
