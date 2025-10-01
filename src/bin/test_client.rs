use anytls_rs::proxy::padding::DefaultPaddingFactory;
use anytls_rs::proxy::session::{Client, Session};
use anytls_rs::util::PROGRAM_VERSION_NAME;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    
    println!("[Test] {}", PROGRAM_VERSION_NAME);
    
    // 创建填充工厂
    let padding = DefaultPaddingFactory::load();
    
    // 创建测试连接
    let tcp_stream = TcpStream::connect("127.0.0.1:8080").await?;
    
    // 创建 Session
    let session = Session::new_client(Box::new(tcp_stream), padding);
    
    // 启动 Session
    let session_handle = tokio::spawn(async move {
        if let Err(e) = session.run().await {
            eprintln!("[Test] Session error: {}", e);
        }
    });
    
    // 等待一段时间
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // 取消任务
    session_handle.abort();
    
    println!("[Test] Test completed");
    Ok(())
}
