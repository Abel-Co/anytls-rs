use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::time::interval;

use crossbeam::queue::SegQueue;
use dashmap::DashMap;
use atomic::Atomic;

/// 无锁外网连接池管理器
pub struct LockFreeOutboundPool {
    /// 可用连接池 - 按目标地址分组，使用 DashMap 实现无锁
    pools: Arc<DashMap<String, SegQueue<PooledConnection>>>,
    /// 连接统计 - 使用原子操作
    stats: Arc<ConnectionStatsAtomic>,
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

/// 原子化的连接统计信息
#[derive(Debug, Default)]
pub struct ConnectionStatsAtomic {
    /// 总连接数
    pub total_connections: Atomic<u64>,
    /// 活跃连接数
    pub active_connections: Atomic<u64>,
    /// 重用连接数
    pub reused_connections: Atomic<u64>,
    /// 新建连接数
    pub new_connections: Atomic<u64>,
    /// 清理的连接数
    pub cleaned_connections: Atomic<u64>,
}

/// 可读的连接统计信息
#[derive(Debug, Clone, Copy)]
pub struct ConnectionStats {
    pub total_connections: u64,
    pub active_connections: u64,
    pub reused_connections: u64,
    pub new_connections: u64,
    pub cleaned_connections: u64,
}

impl LockFreeOutboundPool {
    pub fn new(
        max_connections: usize,
        max_idle_time: Duration,
        min_idle_connections: usize,
    ) -> Self {
        let pools = Arc::new(DashMap::new());
        let stats = Arc::new(ConnectionStatsAtomic::default());
        
        let pool = Self {
            pools: pools.clone(),
            stats: stats.clone(),
            max_connections,
            max_idle_time,
            min_idle_connections,
        };

        // 启动清理任务
        let cleanup_pools = pools.clone();
        let cleanup_stats = stats.clone();
        let cleanup_max_idle = max_idle_time;
        let cleanup_min_idle = min_idle_connections;

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                Self::cleanup_idle_connections(&cleanup_pools, &cleanup_stats, cleanup_max_idle, cleanup_min_idle).await;
            }
        });

        pool
    }

    /// 获取连接 - 无锁实现
    pub async fn get_connection(&self, target: &str) -> Result<TcpStream, std::io::Error> {
        // 尝试从池中获取连接
        if let Some(connection) = self.try_get_from_pool(target).await {
            self.stats.reused_connections.fetch_add(1, atomic::Ordering::Relaxed);
            self.stats.active_connections.fetch_add(1, atomic::Ordering::Relaxed);
            return Ok(connection);
        }

        // 创建新连接
        let stream = TcpStream::connect(target).await?;
        self.stats.new_connections.fetch_add(1, atomic::Ordering::Relaxed);
        self.stats.total_connections.fetch_add(1, atomic::Ordering::Relaxed);
        self.stats.active_connections.fetch_add(1, atomic::Ordering::Relaxed);

        Ok(stream)
    }

    /// 归还连接 - 无锁实现
    pub async fn return_connection(&self, target: &str, stream: TcpStream) {
        // 获取或创建目标地址的队列
        let queue = self.pools.entry(target.to_string()).or_insert_with(SegQueue::new);
        
        // 检查是否超过最大连接数
        if queue.len() < self.max_connections {
            let pooled_conn = PooledConnection {
                stream,
                created_at: Instant::now(),
                last_used: Instant::now(),
                use_count: 1,
            };
            queue.push(pooled_conn);
        }

        self.stats.active_connections.fetch_sub(1, atomic::Ordering::Relaxed);
    }

    /// 从池中尝试获取连接 - 无锁实现
    async fn try_get_from_pool(&self, target: &str) -> Option<TcpStream> {
        if let Some(queue) = self.pools.get(target) {
            if let Some(mut pooled_conn) = queue.pop() {
                pooled_conn.last_used = Instant::now();
                pooled_conn.use_count += 1;
                return Some(pooled_conn.stream);
            }
        }
        None
    }

    /// 清理空闲连接 - 无锁实现
    async fn cleanup_idle_connections(
        pools: &Arc<DashMap<String, SegQueue<PooledConnection>>>,
        stats: &Arc<ConnectionStatsAtomic>,
        max_idle_time: Duration,
        min_idle_connections: usize,
    ) {
        let now = Instant::now();
        let mut cleaned_count = 0;

        for entry in pools.iter() {
            let queue = entry.value();
            let mut to_keep = Vec::new();
            let mut active_count = 0;

            // 收集需要保留的连接
            while let Some(conn) = queue.pop() {
                if now.duration_since(conn.last_used) > max_idle_time {
                    if active_count >= min_idle_connections {
                        cleaned_count += 1;
                        // 连接被丢弃
                    } else {
                        to_keep.push(conn);
                        active_count += 1;
                    }
                } else {
                    to_keep.push(conn);
                    active_count += 1;
                }
            }

            // 将需要保留的连接放回队列
            for conn in to_keep {
                queue.push(conn);
            }
        }

        if cleaned_count > 0 {
            stats.cleaned_connections.fetch_add(cleaned_count, atomic::Ordering::Relaxed);
        }
    }

    /// 获取统计信息 - 无锁实现
    pub fn get_stats(&self) -> ConnectionStats {
        ConnectionStats {
            total_connections: self.stats.total_connections.load(atomic::Ordering::Relaxed),
            active_connections: self.stats.active_connections.load(atomic::Ordering::Relaxed),
            reused_connections: self.stats.reused_connections.load(atomic::Ordering::Relaxed),
            new_connections: self.stats.new_connections.load(atomic::Ordering::Relaxed),
            cleaned_connections: self.stats.cleaned_connections.load(atomic::Ordering::Relaxed),
        }
    }

    /// 获取池状态信息 - 无锁实现
    pub fn get_pool_status(&self) -> HashMap<String, usize> {
        self.pools
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().len()))
            .collect()
    }
}

impl Clone for LockFreeOutboundPool {
    fn clone(&self) -> Self {
        // 创建新的空池，因为 SegQueue 不支持克隆
        Self {
            pools: Arc::new(DashMap::new()),
            stats: self.stats.clone(),
            max_connections: self.max_connections,
            max_idle_time: self.max_idle_time,
            min_idle_connections: self.min_idle_connections,
        }
    }
}

/// 高性能连接池 - 使用更激进的无锁优化
pub struct HighPerfOutboundPool {
    /// 使用 DashMap 的无锁哈希表
    pools: Arc<DashMap<String, SegQueue<PooledConnection>>>,
    /// 使用原子操作的统计信息
    stats: Arc<ConnectionStatsAtomic>,
    /// 配置参数
    max_connections: usize,
    max_idle_time: Duration,
    min_idle_connections: usize,
    /// 清理任务句柄
    cleanup_handle: Option<tokio::task::JoinHandle<()>>,
}

impl HighPerfOutboundPool {
    pub fn new(
        max_connections: usize,
        max_idle_time: Duration,
        min_idle_connections: usize,
    ) -> Self {
        let pools = Arc::new(DashMap::new());
        let stats = Arc::new(ConnectionStatsAtomic::default());
        
        let cleanup_pools = pools.clone();
        let cleanup_stats = stats.clone();
        let cleanup_max_idle = max_idle_time;
        let cleanup_min_idle = min_idle_connections;

        let cleanup_handle = tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(30));
            loop {
                interval.tick().await;
                Self::cleanup_idle_connections(&cleanup_pools, &cleanup_stats, cleanup_max_idle, cleanup_min_idle).await;
            }
        });

        Self {
            pools,
            stats,
            max_connections,
            max_idle_time,
            min_idle_connections,
            cleanup_handle: Some(cleanup_handle),
        }
    }

    /// 获取连接 - 高性能无锁实现
    #[inline]
    pub async fn get_connection(&self, target: &str) -> Result<TcpStream, std::io::Error> {
        // 快速路径：尝试从池中获取
        if let Some(queue) = self.pools.get(target) {
            if let Some(mut conn) = queue.pop() {
                conn.last_used = Instant::now();
                conn.use_count += 1;
                
                // 原子操作更新统计
                self.stats.reused_connections.fetch_add(1, atomic::Ordering::Relaxed);
                self.stats.active_connections.fetch_add(1, atomic::Ordering::Relaxed);
                
                return Ok(conn.stream);
            }
        }

        // 慢路径：创建新连接
        let stream = TcpStream::connect(target).await?;
        
        // 原子操作更新统计
        self.stats.new_connections.fetch_add(1, atomic::Ordering::Relaxed);
        self.stats.total_connections.fetch_add(1, atomic::Ordering::Relaxed);
        self.stats.active_connections.fetch_add(1, atomic::Ordering::Relaxed);

        Ok(stream)
    }

    /// 归还连接 - 高性能无锁实现
    #[inline]
    pub async fn return_connection(&self, target: &str, stream: TcpStream) {
        let queue = self.pools.entry(target.to_string()).or_insert_with(SegQueue::new);
        
        // 检查队列大小（近似检查，避免锁）
        if queue.len() < self.max_connections {
            let pooled_conn = PooledConnection {
                stream,
                created_at: Instant::now(),
                last_used: Instant::now(),
                use_count: 1,
            };
            queue.push(pooled_conn);
        }

        self.stats.active_connections.fetch_sub(1, atomic::Ordering::Relaxed);
    }

    /// 清理空闲连接
    async fn cleanup_idle_connections(
        pools: &Arc<DashMap<String, SegQueue<PooledConnection>>>,
        stats: &Arc<ConnectionStatsAtomic>,
        max_idle_time: Duration,
        min_idle_connections: usize,
    ) {
        let now = Instant::now();
        let mut cleaned_count = 0;

        for entry in pools.iter() {
            let queue = entry.value();
            let mut to_keep = Vec::new();
            let mut active_count = 0;

            // 批量处理连接
            while let Some(conn) = queue.pop() {
                if now.duration_since(conn.last_used) > max_idle_time {
                    if active_count >= min_idle_connections {
                        cleaned_count += 1;
                    } else {
                        to_keep.push(conn);
                        active_count += 1;
                    }
                } else {
                    to_keep.push(conn);
                    active_count += 1;
                }
            }

            // 批量放回
            for conn in to_keep {
                queue.push(conn);
            }
        }

        if cleaned_count > 0 {
            stats.cleaned_connections.fetch_add(cleaned_count, atomic::Ordering::Relaxed);
        }
    }

    /// 获取统计信息
    pub fn get_stats(&self) -> ConnectionStats {
        ConnectionStats {
            total_connections: self.stats.total_connections.load(atomic::Ordering::Relaxed),
            active_connections: self.stats.active_connections.load(atomic::Ordering::Relaxed),
            reused_connections: self.stats.reused_connections.load(atomic::Ordering::Relaxed),
            new_connections: self.stats.new_connections.load(atomic::Ordering::Relaxed),
            cleaned_connections: self.stats.cleaned_connections.load(atomic::Ordering::Relaxed),
        }
    }

    /// 获取池状态
    pub fn get_pool_status(&self) -> HashMap<String, usize> {
        self.pools
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().len()))
            .collect()
    }
}

impl Clone for HighPerfOutboundPool {
    fn clone(&self) -> Self {
        // 创建新的空池，因为 SegQueue 不支持克隆
        Self {
            pools: Arc::new(DashMap::new()),
            stats: self.stats.clone(),
            max_connections: self.max_connections,
            max_idle_time: self.max_idle_time,
            min_idle_connections: self.min_idle_connections,
            cleanup_handle: None, // 不克隆清理任务
        }
    }
}

impl Drop for HighPerfOutboundPool {
    fn drop(&mut self) {
        if let Some(handle) = self.cleanup_handle.take() {
            handle.abort();
        }
    }
}
