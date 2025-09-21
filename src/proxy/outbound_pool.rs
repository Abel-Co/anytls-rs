use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio::time::interval;

/// 外网连接池管理器
pub struct OutboundConnectionPool {
    /// 可用连接池 - 按目标地址分组
    pools: Arc<RwLock<HashMap<String, Vec<PooledConnection>>>>,
    /// 连接统计
    stats: Arc<RwLock<ConnectionStats>>,
    /// 最大连接数
    max_connections: usize,
    /// 最大空闲时间
    max_idle_time: Duration,
    /// 最小空闲连接数
    min_idle_connections: usize,
}

/// 池化连接
pub struct PooledConnection {
    /// 实际连接
    pub stream: TcpStream,
    /// 创建时间
    pub created_at: Instant,
    /// 最后使用时间
    pub last_used: Instant,
    /// 使用次数
    pub use_count: u64,
}

/// 连接统计信息
#[derive(Debug, Default, Clone, Copy)]
pub struct ConnectionStats {
    /// 总连接数
    pub total_connections: u64,
    /// 活跃连接数
    pub active_connections: u64,
    /// 重用连接数
    pub reused_connections: u64,
    /// 新建连接数
    pub new_connections: u64,
    /// 清理的连接数
    pub cleaned_connections: u64,
}

impl OutboundConnectionPool {
    pub fn new(
        max_connections: usize,
        max_idle_time: Duration,
        min_idle_connections: usize,
    ) -> Self {
        let pool = Self {
            pools: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(ConnectionStats::default())),
            max_connections,
            max_idle_time,
            min_idle_connections,
        };

        // 启动清理任务
        let pools = pool.pools.clone();
        let stats = pool.stats.clone();
        let max_idle_time = pool.max_idle_time;
        let min_idle_connections = pool.min_idle_connections;

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                Self::cleanup_idle_connections(&pools, &stats, max_idle_time, min_idle_connections).await;
            }
        });

        pool
    }

    /// 获取连接
    pub async fn get_connection(&self, target: &str) -> Result<TcpStream, std::io::Error> {
        // 尝试从池中获取连接
        if let Some(connection) = self.try_get_from_pool(target).await {
            let mut stats = self.stats.write().await;
            stats.reused_connections += 1;
            stats.active_connections += 1;
            return Ok(connection);
        }

        // 创建新连接
        let stream = TcpStream::connect(target).await?;
        let mut stats = self.stats.write().await;
        stats.new_connections += 1;
        stats.total_connections += 1;
        stats.active_connections += 1;
        drop(stats);

        Ok(stream)
    }

    /// 归还连接
    pub async fn return_connection(&self, target: &str, stream: TcpStream) {
        let mut pools = self.pools.write().await;
        let pool = pools.entry(target.to_string()).or_insert_with(Vec::new);

        // 检查是否超过最大连接数
        if pool.len() < self.max_connections {
            let pooled_conn = PooledConnection {
                stream,
                created_at: Instant::now(),
                last_used: Instant::now(),
                use_count: 1,
            };
            pool.push(pooled_conn);
        }

        let mut stats = self.stats.write().await;
        stats.active_connections = stats.active_connections.saturating_sub(1);
    }

    /// 从池中尝试获取连接
    async fn try_get_from_pool(&self, target: &str) -> Option<TcpStream> {
        let mut pools = self.pools.write().await;
        if let Some(pool) = pools.get_mut(target) {
            if let Some(mut pooled_conn) = pool.pop() {
                pooled_conn.last_used = Instant::now();
                pooled_conn.use_count += 1;
                return Some(pooled_conn.stream);
            }
        }
        None
    }

    /// 清理空闲连接
    async fn cleanup_idle_connections(
        pools: &Arc<RwLock<HashMap<String, Vec<PooledConnection>>>>,
        stats: &Arc<RwLock<ConnectionStats>>,
        max_idle_time: Duration,
        min_idle_connections: usize,
    ) {
        let now = Instant::now();
        let mut pools = pools.write().await;
        let mut cleaned_count = 0;

        for (_, pool) in pools.iter_mut() {
            let mut to_remove = Vec::new();
            let mut active_count = 0;

            for (i, conn) in pool.iter().enumerate() {
                if now.duration_since(conn.last_used) > max_idle_time {
                    if active_count >= min_idle_connections {
                        to_remove.push(i);
                        cleaned_count += 1;
                    } else {
                        active_count += 1;
                    }
                } else {
                    active_count += 1;
                }
            }

            // 从后往前删除，避免索引问题
            for &i in to_remove.iter().rev() {
                pool.remove(i);
            }
        }

        if cleaned_count > 0 {
            let mut stats = stats.write().await;
            stats.cleaned_connections += cleaned_count;
        }
    }

    /// 获取统计信息
    pub async fn get_stats(&self) -> ConnectionStats {
        *self.stats.read().await
    }

    /// 获取池状态信息
    pub async fn get_pool_status(&self) -> HashMap<String, usize> {
        let pools = self.pools.read().await;
        pools.iter().map(|(k, v)| (k.clone(), v.len())).collect()
    }
}

impl Clone for OutboundConnectionPool {
    fn clone(&self) -> Self {
        Self {
            pools: self.pools.clone(),
            stats: self.stats.clone(),
            max_connections: self.max_connections,
            max_idle_time: self.max_idle_time,
            min_idle_connections: self.min_idle_connections,
        }
    }
}
