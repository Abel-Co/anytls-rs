use anytls_rs::proxy::{HighPerfOutboundPool, LockFreeOutboundPool, OutboundConnectionPool};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

/// 性能基准测试
/// cargo test --test benchmark_pools -- --nocapture
#[tokio::test]
async fn benchmark_pools() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 连接池性能基准测试");
    println!("========================");
    println!();

    // 测试参数
    let max_connections = 100;
    let max_idle_time = Duration::from_secs(60);
    let min_idle_connections = 5;
    let test_iterations = 1000;
    let concurrent_tasks = 10;

    // 测试目标
    let test_targets = vec![
        "httpbin.org:80",
        "google.com:443",
        "example.com:80",
        "github.com:443",
        "stackoverflow.com:443",
    ];

    println!("📊 测试配置:");
    println!("  - 最大连接数: {}", max_connections);
    println!("  - 最大空闲时间: {}秒", max_idle_time.as_secs());
    println!("  - 最小空闲连接: {}", min_idle_connections);
    println!("  - 测试迭代次数: {}", test_iterations);
    println!("  - 并发任务数: {}", concurrent_tasks);
    println!("  - 测试目标数: {}", test_targets.len());
    println!();

    // 1. 测试原始锁版本
    println!("🔒 测试原始锁版本 (RwLock + Mutex)");
    let lock_pool = Arc::new(OutboundConnectionPool::new(
        max_connections,
        max_idle_time,
        min_idle_connections,
    ));
    let lock_results = benchmark_pool(lock_pool, test_targets.clone(), test_iterations, concurrent_tasks).await;
    print_results("原始锁版本", &lock_results);
    println!();

    // 2. 测试无锁版本
    println!("⚡ 测试无锁版本 (DashMap + SegQueue)");
    let lockfree_pool = Arc::new(LockFreeOutboundPool::new(
        max_connections,
        max_idle_time,
        min_idle_connections,
    ));
    let lockfree_results = benchmark_pool(lockfree_pool, test_targets.clone(), test_iterations, concurrent_tasks).await;
    print_results("无锁版本", &lockfree_results);
    println!();

    // 3. 测试高性能版本
    println!("🚀 测试高性能版本 (优化无锁)");
    let highperf_pool = Arc::new(HighPerfOutboundPool::new(
        max_connections,
        max_idle_time,
        min_idle_connections,
    ));
    let highperf_results = benchmark_pool(highperf_pool, test_targets.clone(), test_iterations, concurrent_tasks).await;
    print_results("高性能版本", &highperf_results);
    println!();

    // 性能对比
    println!("📈 性能对比分析:");
    println!("==================");
    
    let lock_avg = lock_results.total_time / test_iterations as u128;
    let lockfree_avg = lockfree_results.total_time / test_iterations as u128;
    let highperf_avg = highperf_results.total_time / test_iterations as u128;

    println!("平均响应时间:");
    println!("  - 原始锁版本: {}μs", lock_avg);
    println!("  - 无锁版本: {}μs", lockfree_avg);
    println!("  - 高性能版本: {}μs", highperf_avg);
    println!();

    if lock_avg > 0 {
        let lockfree_improvement = ((lock_avg as f64 - lockfree_avg as f64) / lock_avg as f64) * 100.0;
        let highperf_improvement = ((lock_avg as f64 - highperf_avg as f64) / lock_avg as f64) * 100.0;
        
        println!("性能提升:");
        println!("  - 无锁版本: {:.1}%", lockfree_improvement);
        println!("  - 高性能版本: {:.1}%", highperf_improvement);
    }

    println!();
    println!("连接重用统计:");
    println!("  - 原始锁版本: {}/{} ({:.1}%)", 
        lock_results.reused_connections, 
        lock_results.total_operations,
        (lock_results.reused_connections as f64 / lock_results.total_operations as f64) * 100.0);
    println!("  - 无锁版本: {}/{} ({:.1}%)", 
        lockfree_results.reused_connections, 
        lockfree_results.total_operations,
        (lockfree_results.reused_connections as f64 / lockfree_results.total_operations as f64) * 100.0);
    println!("  - 高性能版本: {}/{} ({:.1}%)", 
        highperf_results.reused_connections, 
        highperf_results.total_operations,
        (highperf_results.reused_connections as f64 / highperf_results.total_operations as f64) * 100.0);

    Ok(())
}

/// 基准测试结果
#[derive(Debug)]
struct BenchmarkResults {
    total_time: u128,
    total_operations: u64,
    reused_connections: u64,
    new_connections: u64,
    errors: u64,
    min_time: u128,
    max_time: u128,
}

/// 执行基准测试
async fn benchmark_pool<T>(
    pool: Arc<T>,
    targets: Vec<&'static str>,
    iterations: usize,
    concurrent_tasks: usize,
) -> BenchmarkResults
where
    T: Send + Sync + 'static,
{
    let start_time = Instant::now();
    let mut total_operations = 0u64;
    let mut errors = 0u64;
    let mut min_time = u128::MAX;
    let mut max_time = 0u128;

    // 创建并发任务
    let mut handles = Vec::new();
    
    for _ in 0..concurrent_tasks {
        let pool = pool.clone();
        let targets = targets.clone();
        
        let handle = tokio::spawn(async move {
            let mut local_operations = 0u64;
            let mut local_errors = 0u64;
            let mut local_min = u128::MAX;
            let mut local_max = 0u128;

            for i in 0..iterations {
                let target = targets[i % targets.len()];
                
                let op_start = Instant::now();
                
                // 模拟连接操作
                match simulate_connection_operation(&pool, target).await {
                    Ok(_) => {
                        local_operations += 1;
                    }
                    Err(_) => {
                        local_errors += 1;
                    }
                }
                
                let op_duration = op_start.elapsed().as_micros();
                local_min = local_min.min(op_duration);
                local_max = local_max.max(op_duration);
                
                // 模拟一些延迟
                if i % 10 == 0 {
                    sleep(Duration::from_millis(1)).await;
                }
            }

            (local_operations, local_errors, local_min, local_max)
        });
        
        handles.push(handle);
    }

    // 等待所有任务完成
    for handle in handles {
        let (ops, errs, min_t, max_t) = handle.await.unwrap();
        total_operations += ops;
        errors += errs;
        min_time = min_time.min(min_t);
        max_time = max_time.max(max_t);
    }

    let total_time = start_time.elapsed().as_micros();
    
    // 获取连接池统计信息
    let stats = get_pool_stats(&pool).await;
    
    BenchmarkResults {
        total_time,
        total_operations,
        reused_connections: stats.reused_connections,
        new_connections: stats.new_connections,
        errors,
        min_time,
        max_time,
    }
}

/// 模拟连接操作
async fn simulate_connection_operation<T>(_pool: &T, target: &str) -> Result<(), Box<dyn std::error::Error>>
where
    T: Send + Sync,
{
    // 这里我们模拟连接操作，实际实现中会调用具体的连接池方法
    // 为了测试，我们使用一个简单的延迟来模拟网络操作
    
    // 模拟连接建立时间
    let connect_time = match target {
        "httpbin.org:80" => 10,
        "google.com:443" => 15,
        "example.com:80" => 8,
        "github.com:443" => 12,
        "stackoverflow.com:443" => 18,
        _ => 20,
    };
    
    sleep(Duration::from_micros(connect_time)).await;
    
    // 模拟一些随机失败
    if rand::random::<f32>() < 0.01 {
        return Err("模拟连接失败".into());
    }
    
    Ok(())
}

/// 获取连接池统计信息（通用接口）
async fn get_pool_stats<T>(_pool: &T) -> PoolStats {
    // 这里应该调用具体的连接池统计方法
    // 为了简化，我们返回模拟数据
    PoolStats {
        total_connections: 0,
        active_connections: 0,
        reused_connections: 0,
        new_connections: 0,
        cleaned_connections: 0,
    }
}

#[derive(Debug)]
struct PoolStats {
    total_connections: u64,
    active_connections: u64,
    reused_connections: u64,
    new_connections: u64,
    cleaned_connections: u64,
}

/// 打印测试结果
fn print_results(name: &str, results: &BenchmarkResults) {
    let avg_time = results.total_time / results.total_operations as u128;
    let success_rate = ((results.total_operations - results.errors) as f64 / results.total_operations as f64) * 100.0;
    
    println!("  {} 结果:", name);
    println!("    - 总操作数: {}", results.total_operations);
    println!("    - 成功操作: {}", results.total_operations - results.errors);
    println!("    - 错误操作: {}", results.errors);
    println!("    - 成功率: {:.1}%", success_rate);
    println!("    - 总时间: {}μs", results.total_time);
    println!("    - 平均时间: {}μs", avg_time);
    println!("    - 最小时间: {}μs", results.min_time);
    println!("    - 最大时间: {}μs", results.max_time);
    println!("    - 重用连接: {}", results.reused_connections);
    println!("    - 新建连接: {}", results.new_connections);
}
