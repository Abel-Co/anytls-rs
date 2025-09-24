use anytls_rs::proxy::padding::DefaultPaddingFactory;
use anytls_rs::proxy::session::Client;
use anytls_rs::util::PROGRAM_VERSION_NAME;
use clap::Parser;
use log::{error, info};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;
use rustls::ClientConfig;
use sha2::{Digest, Sha256};

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
    
    let password_sha256 = Sha256::digest(args.password.as_bytes());
    
    info!("[Client] {}", PROGRAM_VERSION_NAME);
    info!("[Client] SOCKS5 {} => {}", args.listen, args.server);
    
    let listener = TcpListener::bind(&args.listen).await?;
    
    let tls_config = create_tls_config(args.sni.as_deref())?;
    let padding = DefaultPaddingFactory::load();
    
    let padding_clone = padding.clone();
    let client = Arc::new(Client::new(
        Box::new(move || {
            let server = args.server.clone();
            let tls_config = tls_config.clone();
            let password_sha256 = password_sha256.clone();
            let padding = padding_clone.clone();
            
            Box::new(Box::pin(async move {
                let stream = TcpStream::connect(&server).await?;
                let connector = TlsConnector::from(tls_config);
                let mut tls_stream = connector.connect("127.0.0.1".try_into().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?, stream).await?;
                
                // Send authentication
                let mut auth_data = Vec::new();
                auth_data.extend_from_slice(&password_sha256);
                
                let padding_factory = padding.read().await;
                let padding_sizes = padding_factory.generate_record_payload_sizes(0);
                let padding_len = if !padding_sizes.is_empty() {
                    padding_sizes[0] as u16
                } else {
                    0
                };
                
                auth_data.extend_from_slice(&padding_len.to_be_bytes());
                if padding_len > 0 {
                    auth_data.resize(auth_data.len() + padding_len as usize, 0);
                }
                
                // Send auth data
                tls_stream.write_all(&auth_data).await?;
                
                Ok(Box::new(tls_stream) as Box<dyn anytls_rs::util::r#type::AsyncReadWrite>)
            }))
        }),
        padding,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_secs(30),
        5,
    ));
    
    loop {
        let (stream, _addr) = listener.accept().await?;
        let client = client.clone();
        
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, client).await {
                error!("Connection error: {}", e);
            }
        });
    }
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

async fn handle_connection(
    mut stream: TcpStream,
    client: Arc<Client>,
) -> Result<(), Box<dyn std::error::Error>> {
    // SOCKS5 握手处理
    let mut buffer = vec![0u8; 1024];
    
    // 读取客户端版本和认证方法
    let n = stream.read(&mut buffer).await?;
    if n < 3 {
        return Err("Invalid SOCKS5 request".into());
    }
    
    let version = buffer[0];
    let nmethods = buffer[1] as usize;
    
    if version != 5 {
        return Err("Unsupported SOCKS version".into());
    }
    
    if n < 2 + nmethods {
        return Err("Incomplete SOCKS5 request".into());
    }
    
    // 检查是否支持无认证方法
    let mut supports_no_auth = false;
    for i in 2..2 + nmethods {
        if buffer[i] == 0 {
            supports_no_auth = true;
            break;
        }
    }
    
    if !supports_no_auth {
        return Err("No supported authentication method".into());
    }
    
    // 发送认证方法选择响应
    let response = [5, 0]; // 版本5，无认证
    stream.write_all(&response).await?;
    
    // 读取连接请求
    let n = stream.read(&mut buffer).await?;
    if n < 10 {
        return Err("Invalid SOCKS5 connect request".into());
    }
    
    let version = buffer[0];
    let cmd = buffer[1];
    let _rsv = buffer[2];
    let atyp = buffer[3];
    
    if version != 5 || cmd != 1 {
        return Err("Unsupported SOCKS5 command".into());
    }
    
    // 解析目标地址
    let target_addr: String;
    let target_port: u16;
    
    match atyp {
        1 => { // IPv4
            if n < 10 {
                return Err("Incomplete IPv4 address".into());
            }
            target_addr = format!("{}.{}.{}.{}", buffer[4], buffer[5], buffer[6], buffer[7]);
            target_port = u16::from_be_bytes([buffer[8], buffer[9]]);
        }
        3 => { // 域名
            let domain_len = buffer[4] as usize;
            if n < 5 + domain_len + 2 {
                return Err("Incomplete domain address".into());
            }
            target_addr = String::from_utf8_lossy(&buffer[5..5 + domain_len]).to_string();
            target_port = u16::from_be_bytes([buffer[5 + domain_len], buffer[5 + domain_len + 1]]);
        }
        _ => return Err("Unsupported address type".into()),
    }
    
    let target = format!("{}:{}", target_addr, target_port);
    log::info!("Connecting to target: {}", target);
    
    // 创建到代理服务器的连接
    let proxy_stream = client.create_stream().await?;
    
    // 发送目标地址到代理服务器
    let mut addr_data = Vec::new();
    addr_data.push(atyp); // 地址类型
    match atyp {
        1 => { // IPv4
            addr_data.extend_from_slice(&buffer[4..8]); // IP地址
            addr_data.extend_from_slice(&buffer[8..10]); // 端口
        }
        3 => { // 域名
            let domain_len = buffer[4] as usize;
            addr_data.push(domain_len as u8);
            addr_data.extend_from_slice(&buffer[5..5 + domain_len]);
            addr_data.extend_from_slice(&buffer[5 + domain_len..5 + domain_len + 2]);
        }
        _ => return Err("Unsupported address type".into()),
    }
    
    // 发送地址数据到代理服务器
    proxy_stream.write(&addr_data).await?;
    
    // 发送连接成功响应
    let response = [5, 0, 0, 1, 0, 0, 0, 0, 0, 0]; // 成功响应
    stream.write_all(&response).await?;
    
    // 开始数据转发：客户端 <-> 代理服务器
    let (mut client_read, mut client_write) = stream.split();
    let (mut proxy_read, mut proxy_write) = proxy_stream.split_ref();
    
    // 双向数据转发
    tokio::select! {
        _ = tokio::io::copy(&mut client_read, &mut proxy_write) => {
            log::debug!("Client to proxy stream ended");
        }
        _ = tokio::io::copy(&mut proxy_read, &mut client_write) => {
            log::debug!("Proxy to client stream ended");
        }
    }
    
    Ok(())
}