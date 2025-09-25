use anyhow::Result;
use rustls::ServerConfig;
use rcgen::generate_simple_self_signed;

pub fn generate_key_pair(server_name: &str) -> Result<ServerConfig> {
    let cert_key = generate_simple_self_signed(vec![server_name.to_string()])?;
    let cert_chain = vec![rustls::pki_types::CertificateDer::from(cert_key.cert.der().to_vec())];
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert_key.signing_key.serialize_der().into());
    
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)?;
    
    Ok(config)
}