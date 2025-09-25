use std::future::Future;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

pub trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send + Sync {}

impl<T> AsyncReadWrite for T where T: AsyncRead + AsyncWrite + Unpin + Send + Sync {}

pub type DialOutFunc = Box<dyn Fn() -> Box<dyn Future<Output = Result<Arc<crate::proxy::session::Session>, std::io::Error>> + Send + Unpin> + Send + Sync>;