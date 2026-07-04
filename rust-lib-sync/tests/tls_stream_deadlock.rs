//! Regression test for the sync-TLS streaming deadlock.
//!
//! A sustained stream of multi-kilobyte messages over TLS used to wedge the
//! connection: a blocking socket write was issued while the rustls `Connection`
//! mutex was held, so once backpressure appeared, the reader (which needs that
//! mutex to decrypt) and the writer (parked in the mutex-held socket write)
//! blocked each other. Prior repro delivered a couple hundred messages then went
//! silent. These tests drive enough bytes to trigger the backpressure and assert
//! that every message is delivered.
#![cfg(feature = "tls")]

use std::sync::Arc;
use std::time::Duration;

use aiomsg::rustls::crypto::ring;
use aiomsg::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use aiomsg::rustls::{ClientConfig, RootCertStore, ServerConfig};
use aiomsg::{Bytes, DeliveryGuarantee, SendMode, Socket};

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

fn pair() -> (Socket, Socket) {
    let (server_cfg, client_cfg) = local_tls_configs();
    let server = Socket::builder()
        .send_mode(SendMode::RoundRobin)
        .delivery_guarantee(DeliveryGuarantee::AtMostOnce)
        .build();
    let client = Socket::builder()
        .send_mode(SendMode::RoundRobin)
        .delivery_guarantee(DeliveryGuarantee::AtMostOnce)
        .build();
    let addr = server.bind_tls("127.0.0.1:0", server_cfg).unwrap();
    client.connect_tls(addr, "localhost", client_cfg).unwrap();

    // Prove the link is live before timing/streaming (accept polls on ~50 ms).
    server.send(Bytes::from_static(b"__warmup__")).unwrap();
    assert_eq!(
        client
            .recv_timeout(Duration::from_secs(10))
            .expect("warmup timed out")
            .as_ref(),
        b"__warmup__"
    );
    (server, client)
}

/// One-directional sustained stream: the case the benchmark comment says wedges.
fn one_directional(size: usize, count: usize) {
    let (server, client) = pair();
    let payload = Bytes::from(vec![0xABu8; size]);

    let sender = {
        let server = server.clone();
        let payload = payload.clone();
        std::thread::spawn(move || {
            for _ in 0..count {
                server.send(payload.clone()).unwrap();
            }
        })
    };

    // Each message must arrive within a window generous enough to exclude
    // scheduling noise but far under any real deadlock.
    for i in 0..count {
        let got = client
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|| panic!("deadlock: only {i}/{count} of {size}-byte msgs delivered"));
        assert_eq!(got.len(), size);
    }

    sender.join().unwrap();
    server.close();
    client.close();
}

#[test]
fn tls_stream_8kib_one_directional() {
    one_directional(8 * 1024, 2000);
}

#[test]
fn tls_stream_64kib_one_directional() {
    one_directional(64 * 1024, 500);
}

/// Both peers stream heavily at once — the classic backpressure deadlock, where
/// each side's writer is blocked on a full socket while its reader waits for the
/// connection mutex.
#[test]
fn tls_stream_bidirectional() {
    let size = 8 * 1024;
    let count = 2000usize;
    let (a, b) = pair();
    let payload = Bytes::from(vec![0xCDu8; size]);

    let a_send = {
        let a = a.clone();
        let payload = payload.clone();
        std::thread::spawn(move || {
            for _ in 0..count {
                a.send(payload.clone()).unwrap();
            }
        })
    };
    let b_send = {
        let b = b.clone();
        let payload = payload.clone();
        std::thread::spawn(move || {
            for _ in 0..count {
                b.send(payload.clone()).unwrap();
            }
        })
    };
    let a_recv = {
        let a = a.clone();
        std::thread::spawn(move || {
            for i in 0..count {
                a.recv_timeout(Duration::from_secs(10))
                    .unwrap_or_else(|| panic!("deadlock: A got only {i}/{count}"));
            }
        })
    };
    let b_recv = {
        let b = b.clone();
        std::thread::spawn(move || {
            for i in 0..count {
                b.recv_timeout(Duration::from_secs(10))
                    .unwrap_or_else(|| panic!("deadlock: B got only {i}/{count}"));
            }
        })
    };

    a_send.join().unwrap();
    b_send.join().unwrap();
    a_recv.join().unwrap();
    b_recv.join().unwrap();
    a.close();
    b.close();
}
