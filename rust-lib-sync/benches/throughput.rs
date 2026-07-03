//! Steady-state throughput benchmarks for the synchronous (threaded) aiomsg
//! crate.
//!
//! Three families, each parameterised by payload size (64 B, 8 KiB, 64 KiB):
//!
//! * `e2e_plain` — one bind socket sends a batch of M messages to one connect
//!   socket over loopback TCP; the timed region is "enqueue M sends, then drain
//!   M receives". Reports messages/second.
//! * `e2e_tls`   — identical, but the transport is wrapped in rustls.
//! * `framing`   — the wire-framing layer in isolation: build a `Data` envelope,
//!   `write_frame` it into an in-memory buffer, `read_frame` it back, `decode`.
//!   No sockets, so it isolates the per-message copy/allocation cost that the
//!   copy/syscall optimisations target. Reports bytes/second.
//!
//! Design notes for benchmark validity:
//!
//! * Sockets and the TCP/TLS connection are established **once** per benchmark
//!   id, outside the measured loop. This crate's accept loop polls on a ~50 ms
//!   interval, so connection setup carries that latency — which is exactly why
//!   we send one buffered "warmup" message and block until it arrives before
//!   timing anything. Steady-state send/recv itself has no poll (the reader
//!   blocks on real socket reads; the writer blocks on the broker channel).
//! * A batch is timed as "enqueue all M sends, then block until all M come back
//!   out of the receiver", forcing the full pipeline (frame, write, read,
//!   decode, route) to move every message before the timer stops.

use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aiomsg::rustls::crypto::ring;
use aiomsg::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use aiomsg::rustls::{ClientConfig, RootCertStore, ServerConfig};
use aiomsg::{Bytes, DeliveryGuarantee, SendMode, Socket};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

/// Payload sizes under test, in bytes.
const PAYLOADS: &[usize] = &[64, 8 * 1024, 64 * 1024];

/// How many messages make up one timed batch for a given payload size. Larger
/// payloads use smaller batches so bytes-in-flight and per-batch wall time stay
/// in a comparable, memory-bounded range.
fn batch_for(size: usize) -> u64 {
    match size {
        64 => 10_000,
        8_192 => 2_000,
        _ => 500,
    }
}

/// Human-readable label for a payload size (used in the benchmark id).
fn label(size: usize) -> &'static str {
    match size {
        64 => "64B",
        8_192 => "8KiB",
        _ => "64KiB",
    }
}

#[derive(Clone, Copy)]
enum Transport {
    Plain,
    Tls,
}

/// A matching (server, client) rustls config pair trusting a freshly generated
/// self-signed certificate for "localhost" — the same setup the crate's TLS
/// tests use, so nothing depends on external cert files or their expiry.
fn tls_configs() -> (Arc<ServerConfig>, Arc<ClientConfig>) {
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

/// Build the sender/receiver pair, connect them over `transport`, and block
/// until the link is proven live. Returns `(sender, receiver)` ready to time.
fn setup(transport: Transport) -> (Socket, Socket) {
    let server = Socket::builder()
        .send_mode(SendMode::RoundRobin)
        .delivery_guarantee(DeliveryGuarantee::AtMostOnce)
        .build();
    let client = Socket::builder()
        .send_mode(SendMode::RoundRobin)
        .delivery_guarantee(DeliveryGuarantee::AtMostOnce)
        .build();

    match transport {
        Transport::Plain => {
            let addr = server.bind("127.0.0.1:0").unwrap();
            client.connect(addr).unwrap();
        }
        Transport::Tls => {
            let (server_cfg, client_cfg) = tls_configs();
            let addr = server.bind_tls("127.0.0.1:0", server_cfg).unwrap();
            client.connect_tls(addr, "localhost", client_cfg).unwrap();
        }
    }

    // One buffered probe. It is delivered the instant the handshake completes,
    // so receiving it means the connection is established and steady-state
    // timing can begin. (Sends buffer while there is no peer, so this can never
    // be lost.)
    server.send(Bytes::from_static(b"__warmup__")).unwrap();
    let got = client
        .recv_timeout(Duration::from_secs(10))
        .expect("warmup timed out waiting for the connection to come up");
    assert_eq!(&got[..], b"__warmup__");

    (server, client)
}

fn bench_e2e(c: &mut Criterion, transport: Transport, group_name: &str, sizes: &[usize]) {
    let mut group = c.benchmark_group(group_name);
    for &size in sizes {
        let m = batch_for(size);
        group.throughput(Throughput::Elements(m));

        let (server, client) = setup(transport);
        // Cheap-to-clone shared payload: cloning a `Bytes` bumps a refcount, so
        // the benchmark measures the pipeline, not payload allocation.
        let payload = Bytes::from(vec![0xABu8; size]);

        group.bench_with_input(BenchmarkId::from_parameter(label(size)), &size, |b, _| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let start = Instant::now();
                    for _ in 0..m {
                        server.send(payload.clone()).unwrap();
                    }
                    for _ in 0..m {
                        black_box(client.recv().unwrap());
                    }
                    total += start.elapsed();
                }
                total
            });
        });

        server.close();
        client.close();
    }
    group.finish();
}

fn bench_framing(c: &mut Criterion) {
    use aiomsg::protocol::{data, decode, read_frame, write_frame};

    let mut group = c.benchmark_group("framing");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(3));

    for &size in PAYLOADS {
        let frame_len = 4 + 1 + size; // u32 prefix + type byte + payload
        group.throughput(Throughput::Bytes(frame_len as u64));
        let payload = vec![0xABu8; size];

        group.bench_with_input(BenchmarkId::from_parameter(label(size)), &size, |b, _| {
            b.iter(|| {
                let mut buf: Vec<u8> = Vec::with_capacity(frame_len);
                write_frame(&mut buf, &data(&payload)).unwrap();
                let mut cursor = Cursor::new(buf);
                let frame = read_frame(&mut cursor).unwrap().unwrap();
                black_box(decode(&frame))
            });
        });
    }
    group.finish();
}

fn e2e_plain(c: &mut Criterion) {
    bench_e2e(c, Transport::Plain, "e2e_plain", PAYLOADS);
}

fn e2e_tls(c: &mut Criterion) {
    // All payload sizes are benchmarked over sync TLS. Larger messages used to
    // wedge the connection — a sustained stream deadlocked the sync TLS session
    // after a few kilobytes because a blocking socket write was issued with the
    // rustls `Connection` lock held. That is fixed (the writer now extracts
    // ciphertext under the lock and writes it with the lock released), so 8 KiB
    // and 64 KiB are back in the list.
    bench_e2e(c, Transport::Tls, "e2e_tls", PAYLOADS);
}

fn framing(c: &mut Criterion) {
    bench_framing(c);
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(5));
    targets = e2e_plain, e2e_tls, framing
}
criterion_main!(benches);
