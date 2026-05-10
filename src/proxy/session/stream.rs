use crate::proxy::session::frame::{Frame, CMD_FIN, CMD_PSH};
use bytes::Bytes;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot};

type PendingFrameSend = Pin<Box<dyn Future<Output = Result<(), mpsc::error::SendError<Frame>>> + Send>>;

/// Stream 实现 AsyncRead 和 AsyncWrite，提供读写缓冲区
pub struct Stream {
    pub id: u32,

    // 用于从 session 读取数据
    rx: mpsc::Receiver<Bytes>,

    // 用于向 session 写入帧
    frame_tx: mpsc::Sender<Frame>,

    // 部分读取的缓冲区
    read_buffer: Option<Bytes>,
    read_offset: usize,

    // Stream 状态
    closed: Arc<AtomicBool>,

    // 用于通知 Stream 关闭
    close_tx: Option<oneshot::Sender<()>>,

    // 异步发送状态（用于正确处理背压）
    pending_send: Option<PendingFrameSend>,
    pending_send_len: usize,
    pending_shutdown: Option<PendingFrameSend>,
}

impl Stream {
    pub fn new(
        id: u32,
        rx: mpsc::Receiver<Bytes>,
        frame_tx: mpsc::Sender<Frame>,
        close_tx: oneshot::Sender<()>,
    ) -> Self {
        Self {
            id,
            rx,
            frame_tx,
            read_buffer: None,
            read_offset: 0,
            closed: Arc::new(AtomicBool::new(false)),
            close_tx: Some(close_tx),
            pending_send: None,
            pending_send_len: 0,
            pending_shutdown: None,
        }
    }

    /// 检查是否已关闭
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// 标记为关闭
    fn mark_closed(&mut self) {
        self.closed.store(true, Ordering::Release);
        if let Some(tx) = self.close_tx.take() {
            let _ = tx.send(());
        }
    }

    /// 分割 Stream 为读写两部分
    /// 使用 tokio::io::split 创建真正的读写分离
    pub fn split(self) -> (tokio::io::ReadHalf<Self>, tokio::io::WriteHalf<Self>) {
        tokio::io::split(self)
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.is_closed() {
            log::debug!("[Stream] Stream {} is closed, returning EOF", self.id);
            return Poll::Ready(Ok(()));
        }

        // 首先尝试从现有缓冲区读取
        if let Some(data) = &self.read_buffer {
            let remaining = data.len() - self.read_offset;
            let to_copy = remaining.min(buf.remaining());
            
            log::debug!("[Stream] Reading {} bytes from buffer for stream {} (remaining: {})", to_copy, self.id, remaining);
            
            buf.put_slice(&data[self.read_offset..self.read_offset + to_copy]);

            let new_offset = self.read_offset + to_copy;
            if new_offset >= data.len() {
                self.read_buffer = None;
                self.read_offset = 0;
            } else {
                self.read_offset = new_offset;
            }

            return Poll::Ready(Ok(()));
        }

        // 尝试接收新数据
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(data)) => {
                let data_len = data.len();
                let to_copy = data_len.min(buf.remaining());
                log::debug!(
                    "[Stream] Received {} bytes for stream {}, copying {}",
                    data_len,
                    self.id,
                    to_copy
                );

                buf.put_slice(&data[..to_copy]);

                if to_copy < data_len {
                    self.read_buffer = Some(data);
                    self.read_offset = to_copy;
                    log::debug!(
                        "[Stream] Buffered {} bytes for stream {}",
                        data_len - to_copy,
                        self.id
                    );
                }

                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => {
                log::debug!("[Stream] Channel closed for stream {}, marking as closed", self.id);
                self.mark_closed();
                Poll::Ready(Ok(()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        if self.is_closed() {
            log::debug!("[Stream] Stream {} is closed, write failed", self.id);
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stream is closed",
            )));
        }

        if self.pending_send.is_none() {
            log::debug!("[Stream] Writing {} bytes to stream {}", buf.len(), self.id);
            let frame = Frame::with_data(CMD_PSH, self.id, Bytes::copy_from_slice(buf));
            let tx = self.frame_tx.clone();
            self.pending_send = Some(Box::pin(async move { tx.send(frame).await }));
            self.pending_send_len = buf.len();
        }

        if let Some(fut) = self.pending_send.as_mut() {
            match fut.as_mut().poll(cx) {
                Poll::Ready(Ok(())) => {
                    let n = self.pending_send_len;
                    self.pending_send = None;
                    self.pending_send_len = 0;
                    log::debug!("[Stream] Successfully queued {} bytes for stream {}", n, self.id);
                    Poll::Ready(Ok(n))
                }
                Poll::Ready(Err(_)) => {
                    self.pending_send = None;
                    self.pending_send_len = 0;
                    log::debug!("[Stream] Channel closed for stream {}, write failed", self.id);
                    Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "session is closed",
                    )))
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            Poll::Pending
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.is_closed() {
            return Poll::Ready(Ok(()));
        }

        if self.pending_send.is_some() {
            match self.as_mut().poll_write(cx, &[]) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }

        if self.pending_shutdown.is_none() {
            let frame = Frame::new(CMD_FIN, self.id);
            let tx = self.frame_tx.clone();
            self.pending_shutdown = Some(Box::pin(async move { tx.send(frame).await }));
        }

        if let Some(fut) = self.pending_shutdown.as_mut() {
            match fut.as_mut().poll(cx) {
                Poll::Ready(_) => {
                    self.pending_shutdown = None;
                    self.mark_closed();
                    Poll::Ready(Ok(()))
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            Poll::Pending
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if !self.is_closed() {
            let frame = Frame::new(CMD_FIN, self.id);
            let _ = self.frame_tx.try_send(frame);
            self.mark_closed();
        }
    }
}
