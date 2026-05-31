//! TLS integration test: the same blocking socket API, transport wrapped in
//! rustls. Proves `bind_tls`/`connect_tls` complete a real handshake and that
//! the protocol rides over TLS unchanged.
#![cfg(feature = "tls")]

use std::sync::Arc;
use std::time::Duration;

use aiomsg::rustls::crypto::ring;
use aiomsg::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use aiomsg::rustls::{ClientConfig, RootCertStore, ServerConfig};
use aiomsg::Socket;

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

#[test]
fn tls_roundtrip() {
    let (server_cfg, client_cfg) = local_tls_configs();

    let server = Socket::new();
    let addr = server.bind_tls("127.0.0.1:0", server_cfg).unwrap();

    let client = Socket::new();
    client.connect_tls(addr, "localhost", client_cfg).unwrap();

    // Sent before the handshake necessarily completes, so this also exercises
    // buffering on top of the TLS transport.
    server.send("secure hello").unwrap();

    let got = client.recv_timeout(Duration::from_secs(5));
    assert_eq!(got.as_deref(), Some(&b"secure hello"[..]));

    server.close();
    client.close();
}

#[test]
fn connect_tls_rejects_bad_server_name() {
    let (_server_cfg, client_cfg) = local_tls_configs();
    let client = Socket::new();
    let err = client.connect_tls("127.0.0.1:0", "bad\0name", client_cfg);
    assert!(matches!(err, Err(aiomsg::Error::InvalidServerName)));
}
