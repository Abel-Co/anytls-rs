use bytes::Bytes;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::HashMap, io};
use tokio::sync::{mpsc, oneshot, RwLock};

pub(super) struct SessionState {
    pub(super) streams: Arc<RwLock<HashMap<u32, mpsc::Sender<Bytes>>>>,
    pub(super) heartbeat_waiters: Arc<RwLock<HashMap<u32, oneshot::Sender<()>>>>,
    pub(super) synack_waiters: Arc<RwLock<HashMap<u32, oneshot::Sender<io::Result<()>>>>>,
    pub(super) next_stream_id: AtomicU32,
    pub(super) peer_version: AtomicU32,
    pub(super) closed: Arc<AtomicBool>,
    pub(super) stream_count: AtomicU32,
    pub(super) last_active_unix_ms: AtomicU64,
}

impl SessionState {
    pub(super) fn new() -> Self {
        Self {
            streams: Arc::new(RwLock::new(HashMap::new())),
            heartbeat_waiters: Arc::new(RwLock::new(HashMap::new())),
            synack_waiters: Arc::new(RwLock::new(HashMap::new())),
            next_stream_id: AtomicU32::new(1),
            peer_version: AtomicU32::new(0),
            closed: Arc::new(AtomicBool::new(false)),
            stream_count: AtomicU32::new(0),
            last_active_unix_ms: AtomicU64::new(now_unix_ms()),
        }
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub(super) fn stream_count(&self) -> u32 {
        self.stream_count.load(Ordering::Acquire)
    }

    pub(super) fn last_active_unix_ms(&self) -> u64 {
        self.last_active_unix_ms.load(Ordering::Acquire)
    }

    pub(super) fn touch_activity(&self) {
        self.last_active_unix_ms.store(now_unix_ms(), Ordering::Release);
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
