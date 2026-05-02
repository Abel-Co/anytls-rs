use crate::proxy::padding::PaddingFactory;
use crate::util::r#type::{AsyncReadWrite, DialOutFunc};
use bytes::{BufMut, BytesMut};
use rustls::ClientConfig;
use sha2::Digest;
use std::io;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

pub fn create_tls_config() -> Arc<ClientConfig> {
    let mut config = ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    config
        .dangerous()
        .set_certificate_verifier(Arc::new(AllowAnyCertVerifier));
    Arc::new(config)
}

pub fn create_dial_out_func(
    server_addr: String,
    tls_config: Arc<ClientConfig>,
    sni: Option<String>,
    password_sha256: [u8; 32],
    padding: Arc<PaddingFactory>,
) -> DialOutFunc {
    Arc::new(move || {
        let server_addr = server_addr.clone();
        let tls_config = tls_config.clone();
        let sni = sni.clone();
        let password_sha256 = password_sha256;
        let padding = padding.clone();

        Box::new(Box::pin(async move {
            log::debug!("[Client] Connecting to AnyTLS server at {}", server_addr);
            let tcp_stream = TcpStream::connect(&server_addr).await?;
            log::info!("[Client] TCP connection to AnyTLS server established");

            let server_name = sni.unwrap_or_else(|| "localhost".to_string());
            log::debug!("[Client] Using SNI: {}", server_name);
            let server_name = server_name
                .try_into()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

            let tls_connector = TlsConnector::from(tls_config);
            log::debug!("[Client] Starting TLS handshake");
            let mut tls_stream = tls_connector.connect(server_name, tcp_stream).await?;
            log::info!("[Client] TLS handshake completed");

            send_authentication(&mut tls_stream, password_sha256, padding.clone()).await?;
            log::info!("[Client] Authentication completed");

            Ok(Box::new(tls_stream) as Box<dyn AsyncReadWrite>)
        }))
    })
}

pub fn password_sha256(password: &str) -> [u8; 32] {
    sha2::Sha256::digest(password.as_bytes()).into()
}

async fn send_authentication(
    tls_stream: &mut tokio_rustls::client::TlsStream<TcpStream>,
    password_sha256: [u8; 32],
    padding: Arc<PaddingFactory>,
) -> io::Result<()> {
    let padding_sizes = padding.generate_record_payload_sizes(0);
    let padding_length = padding_sizes.get(0).copied().unwrap_or(30) as u16;
    let padding_data = padding.rng_vec(padding_length as usize);

    let mut auth_request = BytesMut::with_capacity(32 + 2 + padding_length as usize);
    auth_request.extend_from_slice(&password_sha256);
    auth_request.put_u16(padding_length);
    auth_request.extend_from_slice(&padding_data);

    tls_stream.write_all(&auth_request).await?;
    tls_stream.flush().await?;
    log::info!("[Client] Authentication request sent (padding: {} bytes)", padding_length);
    Ok(())
}

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
