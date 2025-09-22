use crate::proxy::pipe::PipeDeadline;
use std::io;
use std::sync::Arc;
use std::sync::{mpsc, Mutex};

pub struct PipeReader {
    pub inner: Arc<Mutex<PipeInner>>,
}

pub struct PipeWriter {
    pub inner: Arc<Mutex<PipeInner>>,
}

pub struct PipeInner {
    read_deadline: PipeDeadline,
    write_deadline: PipeDeadline,
    closed: bool,
    read_error: Option<io::Error>,
    write_error: Option<io::Error>,
    data_channel: mpsc::Sender<Vec<u8>>,
    data_receiver: Option<mpsc::Receiver<Vec<u8>>>,
}

impl PipeReader {
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        
        if inner.closed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Pipe closed"));
        }
        
        if let Some(ref mut receiver) = inner.data_receiver {
            if let Ok(data) = receiver.recv() {
                let len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                Ok(len)
            } else {
                Err(io::Error::new(io::ErrorKind::UnexpectedEof, "No more data"))
            }
        } else {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "No receiver"))
        }
    }
    
    pub fn close_with_error(&self, error: Option<io::Error>) {
        let inner = self.inner.clone();
        glommio::spawn_local(async move {
            let mut inner = inner.lock().unwrap();
            inner.read_error = error;
            inner.closed = true;
        }).detach();
    }
    
    pub async fn set_read_deadline(&self, deadline: std::time::SystemTime) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.read_deadline.set(deadline);
        Ok(())
    }
}

impl PipeWriter {
    pub async fn write(&self, buf: &[u8]) -> io::Result<usize> {
        let inner = self.inner.lock().unwrap();
        
        if inner.closed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Pipe closed"));
        }
        
        if let Err(_) = inner.data_channel.send(buf.to_vec()) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Channel closed"));
        }
        
        Ok(buf.len())
    }
    
    pub fn close_with_error(&self, error: Option<io::Error>) {
        let inner = self.inner.clone();
        glommio::spawn_local(async move {
            let mut inner = inner.lock().unwrap();
            inner.write_error = error;
            inner.closed = true;
        }).detach();
    }
    
    pub async fn set_write_deadline(&self, deadline: std::time::SystemTime) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.write_deadline.set(deadline);
        Ok(())
    }
}

pub fn pipe() -> (PipeReader, PipeWriter) {
    let (tx, rx) = mpsc::channel();
    
    let inner = Arc::new(Mutex::new(PipeInner {
        read_deadline: PipeDeadline::new(),
        write_deadline: PipeDeadline::new(),
        closed: false,
        read_error: None,
        write_error: None,
        data_channel: tx,
        data_receiver: Some(rx),
    }));
    
    (PipeReader { inner: inner.clone() }, PipeWriter { inner })
}