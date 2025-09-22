use std::sync::Arc;
use std::sync::{Condvar, Mutex};
use glommio::timer::sleep;

pub struct PipeDeadline {
    notify: Arc<(Mutex<bool>, Condvar)>,
    timer: Option<glommio::task::JoinHandle<()>>,
}

impl PipeDeadline {
    pub fn new() -> Self {
        Self {
            notify: Arc::new((Mutex::new(false), Condvar::new())),
            timer: None,
        }
    }
    
    pub fn set(&mut self, deadline: std::time::SystemTime) {
        if let Some(timer) = self.timer.take() {
            // glommio JoinHandle doesn't have abort, just drop it
            drop(timer);
        }
        
        if let Ok(duration) = deadline.duration_since(std::time::SystemTime::UNIX_EPOCH) {
            let notify = self.notify.clone();
            self.timer = Some(glommio::spawn_local(async move {
                sleep(duration).await;
                let (lock, cvar) = &*notify;
                let mut notified = lock.lock().unwrap();
                *notified = true;
                cvar.notify_one();
            }).detach());
        }
    }
    
    pub fn wait(&self) -> &(Mutex<bool>, Condvar) {
        &self.notify
    }
}

impl Default for PipeDeadline {
    fn default() -> Self {
        Self::new()
    }
}
