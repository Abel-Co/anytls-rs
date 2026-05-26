use crate::proxy::padding::PaddingFactory;
use crate::proxy::session::close_reason::is_expected_close_error;
use crate::proxy::session::frame::{
    Frame, CMD_FIN, CMD_HEART_REQUEST, CMD_PSH, CMD_SETTINGS, CMD_SYN, HEADER_OVERHEAD_SIZE,
};
use crate::proxy::session::state::SessionState;
use crate::proxy::session::stream::Stream;
use crate::util::r#type::AsyncReadWrite;
use crate::util::string_map::{StringMap, StringMapExt};
use bytes::Bytes;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio::time::Duration;

/// Session 管理多个 Stream 的连接复用
pub struct Session {
    pub(super) state: SessionState,
    pub(super) conn_r: Mutex<Option<ReadHalf<Box<dyn AsyncReadWrite>>>>,
    pub(super) conn_w: Mutex<Option<WriteHalf<Box<dyn AsyncReadWrite>>>>,
    pub(super) is_client: bool,
    pub(super) padding: Arc<PaddingFactory>,
    pub(super) pkt_counter: AtomicU32,
    pub(super) send_padding: AtomicBool,
    pub(super) frame_tx: mpsc::Sender<Frame>,
    pub(super) frame_rx: Mutex<Option<mpsc::Receiver<Frame>>>,
    pub(super) close_notify: Notify,
    pub(super) on_new_stream: Option<Arc<dyn Fn(Stream) + Send + Sync>>,
    pub(super) on_close: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Session {
    pub fn new_client(conn: Box<dyn AsyncReadWrite>, padding: Arc<PaddingFactory>) -> Self {
        let (conn_r, conn_w) = tokio::io::split(conn);
        let (frame_tx, frame_rx) = mpsc::channel(1024);
        Self {
            state: SessionState::new(),
            conn_r: Mutex::new(Some(conn_r)),
            conn_w: Mutex::new(Some(conn_w)),
            is_client: true,
            padding,
            pkt_counter: AtomicU32::new(0),
            send_padding: AtomicBool::new(true),
            frame_tx,
            frame_rx: Mutex::new(Some(frame_rx)),
            close_notify: Notify::new(),
            on_new_stream: None,
            on_close: None,
        }
    }

    pub fn new_server(
        conn: Box<dyn AsyncReadWrite>,
        on_new_stream: Option<Arc<dyn Fn(Stream) + Send + Sync>>,
        on_close: Option<Arc<dyn Fn() + Send + Sync>>,
        padding: Arc<PaddingFactory>,
    ) -> Self {
        let (conn_r, conn_w) = tokio::io::split(conn);
        let (frame_tx, frame_rx) = mpsc::channel(1024);
        Self {
            state: SessionState::new(),
            conn_r: Mutex::new(Some(conn_r)),
            conn_w: Mutex::new(Some(conn_w)),
            is_client: false,
            padding,
            pkt_counter: AtomicU32::new(0),
            send_padding: AtomicBool::new(false),
            frame_tx,
            frame_rx: Mutex::new(Some(frame_rx)),
            close_notify: Notify::new(),
            on_new_stream,
            on_close,
        }
    }

    /// 启动 Session。采用“后台循环 + 立即返回”的模型。
    pub async fn run(self: &Arc<Self>) -> io::Result<()> {
        log::debug!("[Session] Starting session (client: {})", self.is_client);
        if self.is_client {
            self.send_client_settings().await?;
            log::debug!("[Session] Client settings sent");
        }

        let mut writer_rx =
            self.frame_rx.lock().await.take().ok_or_else(|| {
                io::Error::other("writer loop already started")
            })?;

        let writer_session = Arc::clone(self);
        tokio::spawn(async move {
            writer_session.run_writer_loop(&mut writer_rx).await;
        });

        let recv_session = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(e) = recv_session.recv_loop().await {
                if is_expected_close_error(&e) {
                    log::debug!("Session receive loop ended: {}", e);
                } else {
                    log::error!("Session receive loop error: {}", e);
                }
            }
        });
        Ok(())
    }

    pub async fn open_stream(&self) -> io::Result<Stream> {
        if self.is_closed() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Session closed"));
        }
        self.touch_activity();

        let stream_id = self.state.next_stream_id.fetch_add(1, Ordering::AcqRel);
        let (data_tx, data_rx) = mpsc::channel(100);
        let (close_tx, _close_rx) = oneshot::channel();
        let stream = Stream::new(stream_id, data_rx, self.frame_tx.clone(), close_tx);

        {
            let mut streams = self.state.streams.write().await;
            streams.insert(stream_id, data_tx);
        }
        self.state.stream_count.fetch_add(1, Ordering::AcqRel);
        self.write_control_frame(Frame::new(CMD_SYN, stream_id)).await?;
        log::debug!("[Session] SYN frame sent for stream {}", stream_id);
        Ok(stream)
    }

    pub async fn close_stream(&self, stream_id: u32) -> io::Result<()> {
        {
            let mut streams = self.state.streams.write().await;
            if streams.remove(&stream_id).is_some() {
                self.state.stream_count.fetch_sub(1, Ordering::AcqRel);
            }
        }
        self.write_control_frame(Frame::new(CMD_FIN, stream_id)).await?;
        Ok(())
    }

    pub async fn write_data_frame(&self, stream_id: u32, data: &[u8]) -> io::Result<usize> {
        self.touch_activity();
        let frame = Frame::with_data(CMD_PSH, stream_id, Bytes::copy_from_slice(data));
        self.frame_tx
            .send(frame)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "session writer closed"))?;
        Ok(data.len() + HEADER_OVERHEAD_SIZE)
    }

    pub(super) async fn write_control_frame(&self, frame: Frame) -> io::Result<usize> {
        self.touch_activity();
        let n = frame.data.len();
        self.frame_tx
            .send(frame)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "session writer closed"))?;
        Ok(n + HEADER_OVERHEAD_SIZE)
    }

    async fn send_client_settings(&self) -> io::Result<()> {
        let settings = StringMap::from([
            ("v".to_string(), "2".to_string()),
            ("client".to_string(), crate::PROGRAM_VERSION_NAME.to_string()),
            ("padding-md5".to_string(), self.padding.md5().to_string()),
        ]);
        let frame = Frame::with_data(CMD_SETTINGS, 0, Bytes::from(settings.to_bytes()));
        let data = frame.to_bytes();
        let mut conn_guard = self.conn_w.lock().await;
        let conn = conn_guard.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "session write half closed")
        })?;
        conn.write_all(&data).await?;
        Ok(())
    }

    pub fn is_closed(&self) -> bool {
        self.state.is_closed()
    }

    pub fn stream_count(&self) -> u32 {
        self.state.stream_count()
    }

    pub fn last_active_unix_ms(&self) -> u64 {
        self.state.last_active_unix_ms()
    }

    pub async fn close(&self) -> io::Result<()> {
        if self.state.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.close_notify.notify_waiters();

        {
            let mut w = self.conn_w.lock().await;
            if let Some(mut wh) = w.take() {
                let _ = wh.shutdown().await;
            }
        }
        {
            let mut r = self.conn_r.lock().await;
            let _ = r.take();
        }
        {
            let mut streams = self.state.streams.write().await;
            streams.clear();
        }
        self.state.stream_count.store(0, Ordering::Release);
        if let Some(cb) = &self.on_close {
            cb();
        }
        Ok(())
    }

    pub(super) fn touch_activity(&self) {
        self.state.touch_activity();
    }

    pub async fn heartbeat_probe(&self, timeout: Duration) -> io::Result<Duration> {
        if self.is_closed() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Session closed"));
        }
        let sid = self.state.next_stream_id.fetch_add(1, Ordering::AcqRel);
        let (tx, rx) = oneshot::channel();
        {
            let mut waiters = self.state.heartbeat_waiters.write().await;
            waiters.insert(sid, tx);
        }

        let start = std::time::Instant::now();
        if let Err(e) = self.write_control_frame(Frame::new(CMD_HEART_REQUEST, sid)).await {
            let mut waiters = self.state.heartbeat_waiters.write().await;
            let _ = waiters.remove(&sid);
            return Err(e);
        }

        let waited = tokio::time::timeout(timeout, rx).await;
        match waited {
            Ok(Ok(())) => Ok(start.elapsed()),
            Ok(Err(_)) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "heartbeat probe channel closed",
            )),
            Err(_) => {
                let mut waiters = self.state.heartbeat_waiters.write().await;
                let _ = waiters.remove(&sid);
                Err(io::Error::new(io::ErrorKind::TimedOut, "heartbeat probe timeout"))
            }
        }
    }
}
