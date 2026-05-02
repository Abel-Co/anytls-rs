use anytls_rs::proxy::session::Session;
use log::info;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration};

#[derive(Clone, Default)]
pub struct SessionRegistry {
    inner: Arc<Mutex<HashMap<u64, Arc<Session>>>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert(&self, id: u64, session: Arc<Session>) {
        self.inner.lock().await.insert(id, session);
    }

    pub async fn remove(&self, id: u64) {
        self.inner.lock().await.remove(&id);
    }

    pub async fn snapshot(&self) -> Vec<(u64, Arc<Session>)> {
        let map = self.inner.lock().await;
        map.iter().map(|(k, v)| (*k, Arc::clone(v))).collect()
    }

    pub fn make_on_close(&self, id: u64) -> Arc<dyn Fn() + Send + Sync> {
        let registry = self.clone();
        Arc::new(move || {
            let registry = registry.clone();
            tokio::spawn(async move {
                registry.remove(id).await;
            });
        })
    }

    pub fn spawn_idle_cleanup(&self, idle_timeout_ms: u64, min_idle: usize) {
        let registry = self.clone();
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(30));
            loop {
                ticker.tick().await;
                registry.cleanup_once(idle_timeout_ms, min_idle).await;
            }
        });
    }

    async fn cleanup_once(&self, idle_timeout_ms: u64, min_idle: usize) {
        let snapshot = self.snapshot().await;
        let now_ms = now_unix_ms();
        let mut idle: Vec<(u64, Arc<Session>)> = snapshot
            .iter()
            .filter_map(|(id, s)| {
                if s.stream_count() == 0
                    && now_ms.saturating_sub(s.last_active_unix_ms()) > idle_timeout_ms
                {
                    Some((*id, Arc::clone(s)))
                } else {
                    None
                }
            })
            .collect();

        if idle.len() > min_idle {
            idle.sort_by_key(|(id, _)| *id);
            let close_count = idle.len() - min_idle;
            for (_, s) in idle.into_iter().take(close_count) {
                let _ = s.close().await;
            }
        }

        let active_streams: u32 = snapshot.iter().map(|(_, s)| s.stream_count()).sum();
        info!(
            "[Server] sessions={}, active_streams={}",
            snapshot.len(),
            active_streams
        );
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
