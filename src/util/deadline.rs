use std::sync::Arc;
use std::time::Duration;
use std::sync::{Condvar, Mutex};
use glommio::timer::sleep;

pub struct DeadlineWatcher {
    notify: Arc<(Mutex<bool>, Condvar)>,
    #[allow(unused)]
    timeout: Duration,
}

impl DeadlineWatcher {
    pub fn new(timeout: Duration, callback: impl FnOnce() + Send + 'static) -> Self {
        let notify = Arc::new((Mutex::new(false), Condvar::new()));
        let notify_clone = notify.clone();
        
        glommio::spawn_local(async move {
            sleep(timeout).await;
            callback();
            let (lock, cvar) = &*notify_clone;
            let mut notified = lock.lock().unwrap();
            *notified = true;
            cvar.notify_one();
        }).detach();
        
        Self { notify, timeout }
    }
    
    pub async fn wait(&self) {
        let (lock, cvar) = &*self.notify;
        let mut notified = lock.lock().unwrap();
        while !*notified {
            notified = cvar.wait(notified).unwrap();
        }
    }
}
