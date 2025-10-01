use anytls_rs::proxy::padding::{DefaultPaddingFactory, PaddingFactory};
use anytls_rs::proxy::session::Client;
use anytls_rs::util::PROGRAM_VERSION_NAME;
use clap::Parser;
use log::{error, info};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use bytes::{BufMut, BytesMut};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;
use rustls::ClientConfig;
use sha2::{Digest, Sha256};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "anytls-client")]
#[command(about = "AnyTLS Client")]
struct Args {
    #[arg(short = 'l', long, default_value = "127.0.0.1:1080", help = "SOCKS5 listen port")]
    listen: String,
    
    #[arg(short = 's', long, default_value = "127.0.0.1:8443", help = "Server address")]
    server: String,
    
    #[arg(long, help = "SNI")]
    sni: Option<String>,
    
    #[arg(short = 'p', long, help = "Password")]
    password: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    
    let args = Args::parse();
    
    if args.password.is_empty() {
        error!("Please set password");
        std::process::exit(1);
    }
    
    let password_sha256: [u8; 32] = Sha256::digest(args.password.as_bytes()).into();
    
    info!("[Client] {}", PROGRAM_VERSION_NAME);
    info!("[Client] SOCKS5 {} => {}", args.listen, args.server);
    
    let listener = TcpListener::bind(&args.listen).await?;
    
    let tls_config = create_tls_config(args.sni.as_deref())?;
    let padding = DefaultPaddingFactory::load();
    
    // 创建客户端
    let dial_out = create_dial_out_func(args.server.clone(), tls_config, args.sni, password_sha256, padding.clone());
    let client = Client::new(
        dial_out,
        padding,
        Duration::from_secs(30), // 空闲超时
        1, // 最小空闲连接数
    );
    
    info!("[Client] Listening on {}", args.listen);
    
    // 监听 SOCKS5 连接
    loop {
        match listener.accept().await {
            Ok((client_conn, addr)) => {
                info!("[Client] New connection from {}", addr);
                
                // 为每个连接创建新的任务
                let client_clone = client.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client_connection(client_conn, client_clone).await {
                        error!("[Client] Connection error: {}", e);
                    }
                });
            }
            Err(e) => {
                error!("[Client] Accept error: {}", e);
            }
        }
    }
}

async fn handle_client_connection(
    mut client_conn: TcpStream,
    client: Client,
) -> Result<(), Box<dyn std::error::Error>> {
    // 读取 SOCKS5 握手
    let mut buffer = [0u8; 1024];
    let n = client_conn.read(&mut buffer).await?;
    
    if n < 3 {
        return Err("Invalid SOCKS5 handshake".into());
    }
    
    // 检查版本
    if buffer[0] != 0x05 {
        return Err("Unsupported SOCKS version".into());
    }
    
    // 检查认证方法数量
    let nmethods = buffer[1] as usize;
    if n < 2 + nmethods {
        return Err("Invalid SOCKS5 handshake".into());
    }
    
    // 检查是否有无认证方法
    let mut has_no_auth = false;
    for i in 2..2 + nmethods {
        if buffer[i] == 0x00 {
            has_no_auth = true;
            break;
        }
    }
    
    if !has_no_auth {
        return Err("No acceptable authentication method".into());
    }
    
    // 发送认证响应
    client_conn.write_all(&[0x05, 0x00]).await?;
    
    // 读取连接请求
    let n = client_conn.read(&mut buffer).await?;
    if n < 10 {
        return Err("Invalid SOCKS5 request".into());
    }
    
    // 检查版本和命令
    if buffer[0] != 0x05 || buffer[1] != 0x01 {
        return Err("Unsupported SOCKS5 command".into());
    }
    
    // 解析目标地址
    let addr_type = buffer[3];
    let target_addr: String;
    let port: u16;
    
    (target_addr, port) = match addr_type {
        0x01 => { // IPv4
            if n < 10 {
                return Err("Invalid IPv4 address".into());
            }
            let addr = format!("{}.{}.{}.{}", buffer[4], buffer[5], buffer[6], buffer[7]);
            let port = u16::from_be_bytes([buffer[8], buffer[9]]);
            (addr, port)
        }
        0x03 => { // 域名
            let domain_len = buffer[4] as usize;
            if n < 7 + domain_len {
                return Err("Invalid domain name".into());
            }
            let addr = String::from_utf8_lossy(&buffer[5..5 + domain_len]).to_string();
            let port = u16::from_be_bytes([buffer[5 + domain_len], buffer[6 + domain_len]]);
            (addr, port)
        }
        _ => {
            return Err("Unsupported address type".into());
        }
    };
    
    info!("[Client] Connecting to {}:{}", target_addr, port);
    
    // 创建到目标服务器的连接
    let _target_conn = TcpStream::connect(format!("{}:{}", target_addr, port)).await?;
    
    // 发送连接成功响应
    let response = [0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    client_conn.write_all(&response).await?;
    
    // 创建 AnyTLS 流
    let anytls_stream = client.create_stream().await?;
    
    // 使用真正的并行数据转发
    let (mut client_read, mut client_write) = client_conn.split();
    let (mut anytls_read, mut anytls_write) = anytls_stream.split();
    
    // 使用 tokio::join! 而不是 tokio::spawn 来避免生命周期问题
    let client_to_target = async move {
        match tokio::io::copy(&mut client_read, &mut anytls_write).await {
            Ok(bytes) => {
                info!("[Client] Client to target copy completed: {} bytes", bytes);
            }
            Err(e) => {
                error!("[Client] Client to target copy error: {}", e);
            }
        }
    };
    
    let target_to_client = async move {
        match tokio::io::copy(&mut anytls_read, &mut client_write).await {
            Ok(bytes) => {
                info!("[Client] Target to client copy completed: {} bytes", bytes);
            }
            Err(e) => {
                error!("[Client] Target to client copy error: {}", e);
            }
        }
    };
    
    // 等待两个任务都完成
    tokio::join!(client_to_target, target_to_client);
    
    // 注意：这里我们没有显式地将 Session 放回空闲池
    // 因为当前的架构中，每个连接都创建新的 Stream
    // 在实际使用中，应该根据业务逻辑来决定是否复用 Session
    
    Ok(())
}

fn create_dial_out_func(
    server_addr: String,
    tls_config: Arc<ClientConfig>,
    sni: Option<String>,
    password_sha256: [u8; 32],
    padding: Arc<PaddingFactory>,
) -> anytls_rs::util::r#type::DialOutFunc {
    Arc::new(move || {
        let server_addr = server_addr.clone();
        let tls_config = tls_config.clone();
        let sni = sni.clone();
        let password_sha256 = password_sha256;
        let padding = padding.clone();
        
        Box::new(Box::pin(async move {
            // 建立 TCP 连接
            let tcp_stream = TcpStream::connect(&server_addr).await?;
            
            // 建立 TLS 连接
            let server_name = sni.unwrap_or_else(|| "localhost".to_string());
            let server_name = server_name.try_into().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
            
            let tls_connector = TlsConnector::from(tls_config);
            let mut tls_stream = tls_connector.connect(server_name, tcp_stream).await?;
            
            // 发送认证请求
            send_authentication(&mut tls_stream, password_sha256, padding.clone()).await?;
            
            Ok(Box::new(tls_stream) as Box<dyn anytls_rs::util::r#type::AsyncReadWrite>)
        }))
    })
}

/// 发送认证请求
async fn send_authentication(
    tls_stream: &mut tokio_rustls::client::TlsStream<TcpStream>,
    password_sha256: [u8; 32],
    padding: Arc<PaddingFactory>,
) -> Result<(), std::io::Error> {
    // 根据协议文档，认证请求格式为：
    // | sha256(password) | padding0 length | padding0 |
    // | 32 Bytes        | Big-Endian uint16 | Variable length |
    
    // 使用统一的 padding 方案生成填充数据
    // 对于认证请求，我们使用 pkt=0 的配置
    let padding_sizes = padding.generate_record_payload_sizes(0);
    
    // 选择第一个有效的填充大小，如果没有则使用默认值
    let padding_length = padding_sizes.get(0).copied().unwrap_or(30) as u16;
    
    // 使用统一的填充数据生成方法
    let padding_data = padding.rng_vec(padding_length as usize);
    
    // 构建认证请求
    let mut auth_request = BytesMut::with_capacity(32 + 2 + padding_length as usize);
    
    // 添加 SHA256 哈希
    auth_request.extend_from_slice(&password_sha256);
    
    // 添加 padding0 长度（大端序）
    auth_request.put_u16(padding_length);
    
    // 添加 padding0 数据
    auth_request.extend_from_slice(&padding_data);
    
    // 发送认证请求
    tls_stream.write_all(&auth_request).await?;
    tls_stream.flush().await?;
    
    info!("[Client] Authentication request sent (padding: {} bytes)", padding_length);
    
    Ok(())
}

fn create_tls_config(_sni: Option<&str>) -> Result<Arc<ClientConfig>, Box<dyn std::error::Error>> {
    let mut config = ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    
    // 使用危险的方法来禁用证书验证
    config.dangerous().set_certificate_verifier(Arc::new(AllowAnyCertVerifier));
    
    Ok(Arc::new(config))
}

// 允许任何证书的验证器
#[derive(Debug)]
struct AllowAnyCertVerifier;

impl rustls::client::danger::ServerCertVerifier for AllowAnyCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA1,
            rustls::SignatureScheme::ECDSA_SHA1_Legacy,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ED448,
        ]
    }
}