use crate::proxy::padding::PaddingFactory;
use crate::proxy::session::{Session, Stream};
use crate::util::r#type::DialOutFunc;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::Duration;

struct IdleEntry {
    session: Arc<Session>,
    idle_since_ms: u64,
}

struct IdlePool {
    entries: HashMap<usize, IdleEntry>,
    order: VecDeque<usize>,
}

impl IdlePool {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn len(&self) -> usize {
        self.order.len()
    }

    fn insert_or_refresh(&mut self, session: Arc<Session>, idle_since_ms: u64) {
        let key = session_key(&session);
        if self.entries.contains_key(&key) {
            self.order.retain(|k| *k != key);
        }
        self.entries.insert(key, IdleEntry { session, idle_since_ms });
        self.order.push_back(key);
    }

    fn pop_back(&mut self) -> Option<IdleEntry> {
        while let Some(key) = self.order.pop_back() {
            if let Some(entry) = self.entries.remove(&key) {
                return Some(entry);
            }
        }
        None
    }

    fn pop_front(&mut self) -> Option<IdleEntry> {
        while let Some(key) = self.order.pop_front() {
            if let Some(entry) = self.entries.remove(&key) {
                return Some(entry);
            }
        }
        None
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

/// 客户端连接管理器，实现连接复用
pub struct Client {
    // 连接函数
    dial_out: DialOutFunc,

    // 填充工厂
    padding: Arc<PaddingFactory>,

    // 空闲 Session 管理（按 session 唯一键去重）
    idle_sessions: Arc<Mutex<IdlePool>>,

    // 配置
    idle_timeout: Duration,
    min_idle_sessions: usize,

    // 状态
    closed: Arc<AtomicBool>,
}

impl Client {
    /// 创建新的客户端
    pub fn new(
        dial_out: DialOutFunc,
        padding: Arc<PaddingFactory>,
        idle_timeout: Duration,
        min_idle_sessions: usize,
    ) -> Self {
        let client = Self {
            dial_out,
            padding,
            idle_sessions: Arc::new(Mutex::new(IdlePool::new())),
            idle_timeout,
            min_idle_sessions,
            closed: Arc::new(AtomicBool::new(false)),
        };

        // 启动定期清理任务
        client.start_cleanup_task();

        client
    }

    /// 创建新的 Stream
    pub async fn create_stream(&self) -> io::Result<(Arc<Session>, Stream)> {
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Client closed"));
        }

        let session = if let Some(session) = self.get_idle_session().await {
            log::debug!("Reusing idle session");
            session
        } else {
            let session = self.create_session().await?;
            log::debug!("Created new session");
            session
        };

        let mut stream = session.open_stream().await?;
        let this = self.clone();
        let session_for_hook = Arc::clone(&session);
        stream.set_on_close(Box::new(move || {
            tokio::spawn(async move {
                this.return_session_to_idle(session_for_hook).await;
            });
        }));
        Ok((session, stream))
    }

    /// 手动将 Session 放回空闲池（由外部调用）
    pub async fn return_session_to_idle(&self, session: Arc<Session>) {
        self.return_to_idle(session).await;
    }

    /// 获取空闲的 Session
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

    /// 创建新的 Session
    async fn create_session(&self) -> io::Result<Arc<Session>> {
        let conn = (self.dial_out)().await?;
        let session = Arc::new(Session::new_client(conn, self.padding.clone()));
        session.run().await?;
        Ok(session)
    }

    /// 将 Session 放回空闲池
    async fn return_to_idle(&self, session: Arc<Session>) {
        if self.closed.load(Ordering::Acquire) {
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

    /// 启动定期清理任务
    fn start_cleanup_task(&self) {
        let client = self.clone();
        let cleanup_interval = Duration::from_secs(30);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(cleanup_interval);
            loop {
                interval.tick().await;

                if client.closed.load(Ordering::Acquire) {
                    break;
                }

                client.ensure_min_idle_sessions().await;
                client.cleanup_idle_sessions().await;
                log::debug!("Performed idle session cleanup");
            }
        });
    }

    /// 主动预热：保持至少 min_idle_sessions 个空闲会话
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
                Ok(session) => self.return_session_to_idle(session).await,
                Err(e) => {
                    log::debug!("Prewarm session failed: {}", e);
                    break;
                }
            }
        }
    }

    /// 清理空闲的 Session
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

    /// 关闭客户端
    pub async fn close(&self) -> io::Result<()> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
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
            dial_out: self.dial_out.clone(),
            padding: self.padding.clone(),
            idle_sessions: self.idle_sessions.clone(),
            idle_timeout: self.idle_timeout,
            min_idle_sessions: self.min_idle_sessions,
            closed: self.closed.clone(),
        }
    }
}
