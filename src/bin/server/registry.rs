use anytls_rs::proxy::session::Session;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

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
}

