//! Self-contained TLS demo: a bind socket and a connect socket talking over
//! rustls in one process. Run with:
//!
//! ```text
//! cargo run --example tls
//! ```
//!
//! It generates a throwaway self-signed certificate at startup so it runs with
//! no setup. A real deployment loads its certificate chain and private key from
//! PEM files instead — see [`server_config_from_pem`] below for that shape.

use std::sync::Arc;

use aiomsg::rustls::crypto::ring;
use aiomsg::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use aiomsg::rustls::{ClientConfig, RootCertStore, ServerConfig};
use aiomsg::Socket;

#[tokio::main]
async fn main() -> aiomsg::Result<()> {
    let (server_cfg, client_cfg) = self_signed_configs("localhost");

    let server = Socket::new();
    let addr = server.bind_tls("127.0.0.1:0", server_cfg).await?;
    println!("server bound (TLS) on {addr}");

    let client = Socket::new();
    // The TCP target is `addr`, but the certificate is validated against the
    // name "localhost" — these are independent.
    client.connect_tls(addr, "localhost", client_cfg).await?;

    server.send("hello over TLS").await?;
    if let Some(msg) = client.recv().await {
        println!("client received: {}", String::from_utf8_lossy(&msg));
    }

    server.close().await;
    client.close().await;
    Ok(())
}

/// Generate a matching (server, client) config pair for `name`, trusting only
/// the freshly minted certificate. Handy for tests and demos.
fn self_signed_configs(name: &str) -> (Arc<ServerConfig>, Arc<ClientConfig>) {
    let ck = rcgen::generate_simple_self_signed(vec![name.to_string()]).unwrap();
    let cert = CertificateDer::from(ck.cert.der().to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der()));
    let provider = Arc::new(ring::default_provider());

    let server = ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .unwrap();

    let mut roots = RootCertStore::empty();
    roots.add(cert).unwrap();
    let client = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();

    (Arc::new(server), Arc::new(client))
}

/// How a real server builds its config: read the certificate chain and private
/// key from PEM files (e.g. from your CA or Let's Encrypt). Unused by the demo;
/// shown here as the realistic counterpart to [`self_signed_configs`].
#[allow(dead_code)]
fn server_config_from_pem(cert_pem: &str, key_pem: &str) -> std::io::Result<Arc<ServerConfig>> {
    use std::io::{BufReader, Error, ErrorKind};

    let certs = rustls_pemfile::certs(&mut BufReader::new(std::fs::File::open(cert_pem)?))
        .collect::<std::io::Result<Vec<_>>>()?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(std::fs::File::open(key_pem)?))?
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "no private key in PEM"))?;

    let cfg = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::new(ErrorKind::InvalidData, e))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
    Ok(Arc::new(cfg))
}
