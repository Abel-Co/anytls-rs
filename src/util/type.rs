use std::future::Future;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static {}

impl<T> AsyncReadWrite for T where T: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static {}

pub type DialOutFunc = Arc<dyn Fn() -> Box<dyn Future<Output = Result<Box<dyn AsyncReadWrite>, std::io::Error>> + Send + Unpin> + Send + Sync>;