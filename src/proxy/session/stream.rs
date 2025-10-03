use crate::proxy::session::frame::{Frame, CMD_PSH, CMD_FIN};
use bytes::Bytes;
use std::io;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot};
use std::pin::Pin;
use std::task::{Context, Poll};

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
    closed: Arc<Mutex<bool>>,
    
    // 用于通知 Stream 关闭
    close_tx: Option<oneshot::Sender<()>>,
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
            closed: Arc::new(Mutex::new(false)),
            close_tx: Some(close_tx),
        }
    }

    // /// 获取 Stream ID
    // pub fn id(&self) -> u32 {
    //     self.id
    // }

    /// 检查是否已关闭
    pub fn is_closed(&self) -> bool {
        *self.closed.lock().unwrap()
    }

    /// 标记为关闭
    fn mark_closed(&mut self) {
        *self.closed.lock().unwrap() = true;
        if let Some(tx) = self.close_tx.take() {
            let _ = tx.send(());
        }
    }

    // /// 关闭 Stream
    // pub async fn close(&self) -> io::Result<()> {
    //     if self.is_closed() {
    //         return Ok(());
    //     }

    //     // 发送 FIN 帧
    //     let frame = Frame::new(CMD_FIN, self.id);
    //     if let Err(_) = self.frame_tx.try_send(frame) {
    //         // 如果发送失败，说明 session 已经关闭
    //     }

    //     Ok(())
    // }

    // /// 本地关闭（不通知远程）
    // pub fn close_locally(&mut self) {
    //     if !self.is_closed() {
    //         self.mark_closed();
    //     }
    // }

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
                log::debug!("[Stream] Received {} bytes for stream {}, copying {}", 
                           data_len, self.id, to_copy);
                
                buf.put_slice(&data[..to_copy]);
                
                if to_copy < data_len {
                    self.read_buffer = Some(data);
                    self.read_offset = to_copy;
                    log::debug!("[Stream] Buffered {} bytes for stream {}", 
                               data_len - to_copy, self.id);
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
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.is_closed() {
            log::debug!("[Stream] Stream {} is closed, write failed", self.id);
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stream is closed",
            )));
        }
        
        log::debug!("[Stream] Writing {} bytes to stream {}", buf.len(), self.id);
        let frame = Frame::with_data(CMD_PSH, self.id, Bytes::from(buf.to_vec()));
        
        match self.frame_tx.try_send(frame) {
            Ok(()) => {
                log::debug!("[Stream] Successfully queued {} bytes for stream {}", buf.len(), self.id);
                Poll::Ready(Ok(buf.len()))
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                log::debug!("[Stream] Channel full for stream {}, waiting", self.id);
                // 通道已满，注册等待
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                log::debug!("[Stream] Channel closed for stream {}, write failed", self.id);
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "session is closed",
                )))
            }
        }
    }
    
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.is_closed() {
            return Poll::Ready(Ok(()));
        }
        
        let frame = Frame::new(CMD_FIN, self.id);
        
        match self.frame_tx.try_send(frame) {
            Ok(()) => {
                self.mark_closed();
                Poll::Ready(Ok(()))
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.mark_closed();
                Poll::Ready(Ok(()))
            }
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



