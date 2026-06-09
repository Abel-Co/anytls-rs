use crate::proxy::padding::PaddingFactory;
use crate::proxy::session::{Session, Stream};
use crate::util::r#type::DialOutFunc;
use linked_hash_map::LinkedHashMap;
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::sync::Mutex;
use tokio::time::Duration;

struct IdleEntry {
    session: Arc<Session>,
    idle_since_ms: u64,
}

struct IdlePool {
    entries: LinkedHashMap<usize, IdleEntry>,
}

impl IdlePool {
    fn new() -> Self {
        Self {
            entries: LinkedHashMap::new(),
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn insert_or_refresh(&mut self, session: Arc<Session>, idle_since_ms: u64) {
        let key = session_key(&session);
        let _ = self.entries.remove(&key);
        self.entries.insert(key, IdleEntry { session, idle_since_ms });
    }

    fn pop_back(&mut self) -> Option<IdleEntry> {
        self.entries.pop_back().map(|(_, entry)| entry)
    }

    fn pop_front(&mut self) -> Option<IdleEntry> {
        self.entries.pop_front().map(|(_, entry)| entry)
    }

    fn truncate_to(&mut self, keep: usize) -> Vec<Arc<Session>> {
        let mut removed = Vec::new();
        while self.len() > keep {
            if let Some(entry) = self.pop_front() {
                removed.push(entry.session);
            } else {
                break;
            }
        }
        removed
    }
}

struct GlobalControl {
    clients: Arc<StdMutex<HashMap<usize, Client>>>,
}

static GLOBAL_CONTROL: OnceLock<GlobalControl> = OnceLock::new();
static NEXT_CLIENT_ID: AtomicUsize = AtomicUsize::new(1);

fn global_control() -> &'static GlobalControl {
    GLOBAL_CONTROL.get_or_init(|| {
        let clients = Arc::new(StdMutex::new(HashMap::<usize, Client>::new()));
        let cleanup_clients = Arc::clone(&clients);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                let snapshot = {
                    let clients = cleanup_clients
                        .lock()
                        .expect("global anytls-rs clients lock poisoned");
                    clients.values().cloned().collect::<Vec<_>>()
                };

                for client in snapshot {
                    if client.closed.load(Ordering::Acquire) {
                        continue;
                    }
                    client.ensure_min_idle_sessions().await;
                    client.cleanup_idle_sessions().await;
                }
            }
        });

        GlobalControl { clients }
    })
}

pub struct Client {
    id: usize,
    dial_out: DialOutFunc,
    padding: Arc<PaddingFactory>,
    idle_sessions: Arc<Mutex<IdlePool>>,
    idle_timeout: Duration,
    min_idle_sessions: usize,
    closed: Arc<AtomicBool>,
}

impl Client {
    pub fn new(
        dial_out: DialOutFunc,
        padding: Arc<PaddingFactory>,
        idle_timeout: Duration,
        min_idle_sessions: usize,
    ) -> Self {
        let client = Self {
            id: NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed),
            dial_out,
            padding,
            idle_sessions: Arc::new(Mutex::new(IdlePool::new())),
            idle_timeout,
            min_idle_sessions,
            closed: Arc::new(AtomicBool::new(false)),
        };

        let ctl = global_control();
        let mut clients = ctl
            .clients
            .lock()
            .expect("global anytls-rs clients lock poisoned");
        clients.insert(client.id, client.clone());

        client
    }

    pub async fn create_stream(&self) -> io::Result<Stream> {
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Client closed"));
        }

        let (session, mut stream) = match self.open_stream_from_available_session().await {
            Ok(v) => v,
            Err(first_err) => {
                log::debug!("Idle session open failed, creating replacement: {}", first_err);
                let session = self.create_session().await?;
                log::debug!("Created new session");
                let stream = session.open_stream().await?;
                (session, stream)
            }
        };
        let this = self.clone();
        let session_for_hook = Arc::clone(&session);
        stream.set_on_close(Box::new(move || {
            tokio::spawn(async move {
                this.return_to_idle(session_for_hook).await;
            });
        }));
        Ok(stream)
    }

    async fn open_stream_from_available_session(&self) -> io::Result<(Arc<Session>, Stream)> {
        let session = if let Some(session) = self.get_idle_session().await {
            log::debug!("Reusing idle session");
            session
        } else {
            let session = self.create_session().await?;
            log::debug!("Created new session");
            session
        };

        match session.open_stream().await {
            Ok(stream) => Ok((session, stream)),
            Err(e) => {
                let _ = session.close().await;
                Err(e)
            }
        }
    }

    pub async fn heartbeat_probe(&self, timeout: Duration) -> io::Result<Duration> {
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Client closed"));
        }
        let session = if let Some(session) = self.get_idle_session().await {
            session
        } else {
            self.create_session().await?
        };
        let rtt = session.heartbeat_probe(timeout).await?;
        self.return_to_idle(session).await;
        Ok(rtt)
    }

    async fn get_idle_session(&self) -> Option<Arc<Session>> {
        let mut to_close = Vec::new();
        let mut selected = None;
        {
            let mut idle_sessions = self.idle_sessions.lock().await;
            let now = now_unix_ms();
            let timeout_ms = self.idle_timeout.as_millis() as u64;

            while let Some(entry) = idle_sessions.pop_back() {
                if entry.session.is_closed() {
                    continue;
                }
                if timeout_ms > 0 && now.saturating_sub(entry.idle_since_ms) > timeout_ms {
                    to_close.push(entry.session);
                    continue;
                }
                selected = Some(entry.session);
                break;
            }
        }

        for session in to_close {
            let _ = session.close().await;
        }
        selected
    }

    async fn create_session(&self) -> io::Result<Arc<Session>> {
        let conn = (self.dial_out)().await?;
        let session = Arc::new(Session::new_client(conn, self.padding.clone()));
        session.run().await?;
        Ok(session)
    }

    async fn return_to_idle(&self, session: Arc<Session>) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if session.is_closed() {
            return;
        }

        let mut idle_sessions = self.idle_sessions.lock().await;
        idle_sessions.insert_or_refresh(session, now_unix_ms());

        if idle_sessions.len() > self.min_idle_sessions * 2 {
            let to_close = idle_sessions.truncate_to(self.min_idle_sessions);
            drop(idle_sessions);
            for session in to_close {
                let _ = session.close().await;
            }
            return;
        }

        log::debug!("Session returned to idle pool");
    }

    async fn ensure_min_idle_sessions(&self) {
        if self.min_idle_sessions == 0 || self.closed.load(Ordering::Acquire) {
            return;
        }

        let need = {
            let idle_sessions = self.idle_sessions.lock().await;
            self.min_idle_sessions.saturating_sub(idle_sessions.len())
        };

        if need == 0 {
            return;
        }

        for _ in 0..need {
            if self.closed.load(Ordering::Acquire) {
                break;
            }
            match self.create_session().await {
                Ok(session) => self.return_to_idle(session).await,
                Err(e) => {
                    log::debug!("Prewarm session failed: {}", e);
                    break;
                }
            }
        }
    }

    pub async fn cleanup_idle_sessions(&self) {
        let mut to_close = Vec::new();
        {
            let mut idle_sessions = self.idle_sessions.lock().await;
            let now = now_unix_ms();
            let timeout_ms = self.idle_timeout.as_millis() as u64;
            let keep_min = self.min_idle_sessions;

            let mut kept = 0usize;
            let mut survivors: Vec<IdleEntry> = Vec::with_capacity(idle_sessions.len());
            while let Some(entry) = idle_sessions.pop_front() {
                if entry.session.is_closed() {
                    continue;
                }
                let expired = timeout_ms > 0 && now.saturating_sub(entry.idle_since_ms) > timeout_ms;
                if expired && kept >= keep_min {
                    to_close.push(entry.session);
                    continue;
                }
                kept += 1;
                survivors.push(entry);
            }

            for entry in survivors {
                idle_sessions.insert_or_refresh(entry.session, entry.idle_since_ms);
            }

            if idle_sessions.len() > keep_min {
                let excess = idle_sessions.len() - keep_min;
                for _ in 0..excess {
                    if let Some(entry) = idle_sessions.pop_front() {
                        to_close.push(entry.session);
                    }
                }
            }
        }

        for session in to_close {
            let _ = session.close().await;
        }
    }

    pub async fn close(&self) -> io::Result<()> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }

        {
            let ctl = global_control();
            let mut clients = ctl
                .clients
                .lock()
                .expect("global anytls-rs clients lock poisoned");
            clients.remove(&self.id);
        }

        let mut idle_sessions = self.idle_sessions.lock().await;
        let mut to_close = Vec::new();
        while let Some(entry) = idle_sessions.pop_front() {
            to_close.push(entry.session);
        }
        drop(idle_sessions);

        for session in to_close {
            session.close().await.ok();
        }

        Ok(())
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            dial_out: self.dial_out.clone(),
            padding: self.padding.clone(),
            idle_sessions: self.idle_sessions.clone(),
            idle_timeout: self.idle_timeout,
            min_idle_sessions: self.min_idle_sessions,
            closed: self.closed.clone(),
        }
    }
}
