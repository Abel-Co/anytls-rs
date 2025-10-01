use crate::proxy::padding::PaddingFactory;
use crate::proxy::session::{Session, Stream};
use crate::util::r#type::DialOutFunc;
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::Duration;

/// 客户端连接管理器，实现连接复用
pub struct Client {
    // 连接函数
    dial_out: DialOutFunc,
    
    // 填充工厂
    padding: Arc<PaddingFactory>,
    
    // 空闲 Session 管理
    idle_sessions: Arc<Mutex<VecDeque<Arc<Session>>>>,
    
    // 配置
    idle_timeout: Duration,
    min_idle_sessions: usize,
    
    // 状态
    closed: AtomicBool,
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
            idle_sessions: Arc::new(Mutex::new(VecDeque::new())),
            idle_timeout,
            min_idle_sessions,
            closed: AtomicBool::new(false),
        };
        
        // 启动定期清理任务
        client.start_cleanup_task();
        
        client
    }

    /// 创建新的 Stream
    pub async fn create_stream(&self) -> io::Result<Stream> {
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Client closed"));
        }

        // 尝试获取空闲的 Session
        if let Some(session) = self.get_idle_session().await {
            log::debug!("Reusing idle session");
            return session.open_stream().await;
        }

        // 创建新的 Session
        let session = self.create_session().await?;
        log::debug!("Created new session");
        
        session.open_stream().await
    }

    /// 手动将 Session 放回空闲池（由外部调用）
    pub async fn return_session_to_idle(&self, session: Arc<Session>) {
        self.return_to_idle(session).await;
    }

    /// 获取空闲的 Session
    async fn get_idle_session(&self) -> Option<Arc<Session>> {
        let mut idle_sessions = self.idle_sessions.lock().await;
        
        // 直接返回第一个可用的 Session
        // 注意：这里没有过期检查，因为 VecDeque 设计更简单
        // 如果需要过期检查，可以考虑在 Session 内部添加时间戳
        idle_sessions.pop_front()
    }

    /// 创建新的 Session
    async fn create_session(&self) -> io::Result<Arc<Session>> {
        // 建立连接
        let conn = (self.dial_out)().await?;
        
        // 创建 Session
        let session = Arc::new(Session::new_client(conn, self.padding.clone()));

        // 启动 Session
        let session_clone = session.clone();
        tokio::spawn(async move {
            if let Err(e) = session_clone.run().await {
                log::error!("Session run error: {}", e);
            }
        });

        Ok(session)
    }

    /// 将 Session 放回空闲池
    async fn return_to_idle(&self, session: Arc<Session>) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }

        let mut idle_sessions = self.idle_sessions.lock().await;
        idle_sessions.push_back(session);
        
        // 保持最小空闲 Session 数量
        if idle_sessions.len() > self.min_idle_sessions * 2 {
            idle_sessions.truncate(self.min_idle_sessions);
        }
        
        log::debug!("Session returned to idle pool");
    }



    /// 启动定期清理任务
    fn start_cleanup_task(&self) {
        let client = self.clone();
        let cleanup_interval = Duration::from_secs(30); // 每30秒清理一次
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(cleanup_interval);
            loop {
                interval.tick().await;
                
                if client.closed.load(Ordering::Acquire) {
                    break;
                }
                
                client.cleanup_idle_sessions().await;
                log::debug!("Performed idle session cleanup");
            }
        });
    }

    /// 清理空闲的 Session
    pub async fn cleanup_idle_sessions(&self) {
        let mut idle_sessions = self.idle_sessions.lock().await;
        
        // 保持最小空闲 Session 数量
        if idle_sessions.len() > self.min_idle_sessions {
            let excess = idle_sessions.len() - self.min_idle_sessions;
            for _ in 0..excess {
                if let Some(session) = idle_sessions.pop_front() {
                    session.close().await.ok();
                }
            }
        }
    }

    /// 关闭客户端
    pub async fn close(&self) -> io::Result<()> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }

        // 清空空闲 Session
        let mut idle_sessions = self.idle_sessions.lock().await;
        for session in idle_sessions.drain(..) {
            session.close().await.ok();
        }

        Ok(())
    }
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            dial_out: self.dial_out.clone(),
            padding: self.padding.clone(),
            idle_sessions: self.idle_sessions.clone(),
            idle_timeout: self.idle_timeout,
            min_idle_sessions: self.min_idle_sessions,
            closed: AtomicBool::new(self.closed.load(Ordering::Acquire)),
        }
    }
}
