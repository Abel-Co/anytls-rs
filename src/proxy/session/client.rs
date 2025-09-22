use crate::proxy::padding::PaddingFactory;
use crate::proxy::session::{Session, Stream};
use crate::util::r#type::DialOutFunc;
use indexmap::IndexMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::sync::Mutex;
use glommio::sync::RwLock;
use glommio::timer::Timer;

pub struct Client {
    dial_out: DialOutFunc,
    sessions: Arc<Mutex<IndexMap<u64, Arc<Session>>>>,
    idle_sessions: Arc<Mutex<Vec<(u64, Arc<Session>, Instant)>>>,
    session_counter: Arc<Mutex<u64>>,
    padding: Arc<RwLock<PaddingFactory>>,
    idle_session_timeout: Duration,
    min_idle_sessions: usize,
}

impl Client {
    pub fn new(
        dial_out: DialOutFunc,
        padding: Arc<RwLock<PaddingFactory>>,
        idle_session_check_interval: Duration,
        idle_session_timeout: Duration,
        min_idle_sessions: usize,
    ) -> Self {
        let client = Self {
            dial_out,
            sessions: Arc::new(Mutex::new(IndexMap::new())),
            idle_sessions: Arc::new(Mutex::new(Vec::new())),
            session_counter: Arc::new(Mutex::new(0)),
            padding,
            idle_session_timeout,
            min_idle_sessions,
        };
        
        // Start idle cleanup routine
        let idle_sessions = client.idle_sessions.clone();
        let idle_timeout = client.idle_session_timeout;
        let min_idle = client.min_idle_sessions;
        
        glommio::spawn_local(async move {
            loop {
                Timer::new(idle_session_check_interval).await;
                Self::idle_cleanup(&idle_sessions, idle_timeout, min_idle).await;
            }
        }).detach();
        
        client
    }
    
    pub async fn create_stream(&self) -> Result<Arc<Stream>, std::io::Error> {
        let session = self.find_or_create_session().await?;
        session.open_stream().await
    }
    
    async fn find_or_create_session(&self) -> Result<Arc<Session>, std::io::Error> {
        // Try to find an idle session first
        if let Some(session) = self.find_idle_session().await {
            return Ok(session);
        }
        
        // Create a new session
        self.create_session().await
    }
    
    async fn find_idle_session(&self) -> Option<Arc<Session>> {
        let mut idle_sessions = self.idle_sessions.lock().unwrap();
        if let Some((_, session, _)) = idle_sessions.pop() {
            Some(session)
        } else {
            None
        }
    }
    
    async fn create_session(&self) -> Result<Arc<Session>, std::io::Error> {
        let conn = (self.dial_out)().await?;
        let session = Arc::new(Session::new_client(conn, self.padding.clone()));
        
        let mut counter = self.session_counter.lock().unwrap();
        *counter += 1;
        let seq = *counter;
        drop(counter);
        
        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(seq, session.clone());
        
        // Start the session
        let session_clone = session.clone();
        let idle_sessions_clone = self.idle_sessions.clone();
        
        glommio::spawn_local(async move {
            if let Err(e) = session_clone.run().await {
                log::error!("Session error: {}", e);
            }
            
            // Move to idle sessions when done
            let mut idle_sessions = idle_sessions_clone.lock().unwrap();
            idle_sessions.push((seq, session_clone, Instant::now()));
        }).detach();
        
        Ok(session)
    }
    
    async fn idle_cleanup(
        idle_sessions: &Arc<Mutex<Vec<(u64, Arc<Session>, Instant)>>>,
        timeout: Duration,
        min_idle: usize,
    ) {
        let mut sessions = idle_sessions.lock().unwrap();
        let now = Instant::now();
        let mut active_count = 0;
        let mut to_remove = Vec::new();
        
        for (i, (_, _session, idle_since)) in sessions.iter().enumerate() {
            if now.duration_since(*idle_since) < timeout {
                active_count += 1;
                continue;
            }
            
            if active_count < min_idle {
                active_count += 1;
                continue;
            }
            
            to_remove.push(i);
        }
        
        // Remove old sessions
        for &i in to_remove.iter().rev() {
            if i < sessions.len() {
                let (_, session, _) = sessions.swap_remove(i);
                let _ = session.close().await;
            }
        }
    }
    
    pub async fn close(&self) -> Result<(), std::io::Error> {
        let sessions = self.sessions.lock().unwrap();
        for session in sessions.values() {
            let _ = session.close().await;
        }
        Ok(())
    }
}