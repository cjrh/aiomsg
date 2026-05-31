//! TLS integration test: the same socket API, but the transport is wrapped in
//! rustls. Proves `bind_tls`/`connect_tls` complete a real handshake and that
//! the protocol rides over TLS unchanged.
#![cfg(feature = "tls")]

use std::sync::Arc;
use std::time::Duration;

use aiomsg::rustls::crypto::ring;
use aiomsg::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use aiomsg::rustls::{ClientConfig, RootCertStore, ServerConfig};
use aiomsg::Socket;
use tokio::time::timeout;

/// Build matching server/client rustls configs from a freshly generated
/// self-signed certificate for `localhost`. The client trusts exactly that
/// certificate — no system roots, no external files.
fn local_tls_configs() -> (Arc<ServerConfig>, Arc<ClientConfig>) {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
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

#[tokio::test]
async fn tls_roundtrip() {
    let (server_cfg, client_cfg) = local_tls_configs();

    let server = Socket::new();
    let addr = server.bind_tls("127.0.0.1:0", server_cfg).await.unwrap();

    let client = Socket::new();
    client
        .connect_tls(addr, "localhost", client_cfg)
        .await
        .unwrap();

    // Sent before the handshake necessarily completes, so this also exercises
    // buffering on top of the TLS transport.
    server.send("secure hello").await.unwrap();

    let got = timeout(Duration::from_secs(5), client.recv())
        .await
        .ok()
        .flatten();
    assert_eq!(got.as_deref(), Some(&b"secure hello"[..]));

    server.close().await;
    client.close().await;
}

#[tokio::test]
async fn connect_tls_rejects_bad_server_name() {
    let (_server_cfg, client_cfg) = local_tls_configs();
    let client = Socket::new();
    // A name with an interior NUL is never a valid DNS name or IP.
    let err = client
        .connect_tls("127.0.0.1:0", "bad\0name", client_cfg)
        .await;
    assert!(matches!(err, Err(aiomsg::Error::InvalidServerName)));
}
