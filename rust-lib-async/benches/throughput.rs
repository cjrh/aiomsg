//! Steady-state throughput benchmarks for the async (tokio) aiomsg crate.
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
//!   id, outside the measured loop. A single buffered "warmup" message is sent
//!   and awaited so we only start timing after the handshake has completed and
//!   the peer is registered with the broker — critical because connection setup
//!   is not the thing under test.
//! * The whole family shares **one** multi-threaded tokio runtime; `block_on`
//!   drives each measured batch, so we never build a runtime per iteration.
//! * Sends only enqueue onto the broker's channel, so a batch is timed as
//!   "enqueue all M, then block until all M come back out of the receiver" —
//!   that forces the full pipeline (frame, write syscall, read, decode, route)
//!   to actually move every message before the timer stops.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aiomsg::rustls::crypto::ring;
use aiomsg::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use aiomsg::rustls::{ClientConfig, RootCertStore, ServerConfig};
use aiomsg::{Bytes, DeliveryGuarantee, SendMode, Socket};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tokio::runtime::Runtime;
use tokio::time::timeout;

/// Payload sizes under test, in bytes.
const PAYLOADS: &[usize] = &[64, 8 * 1024, 64 * 1024];

/// How many messages make up one timed batch for a given payload size. Larger
/// payloads use smaller batches so the bytes-in-flight and per-batch wall time
/// stay in a comparable, memory-bounded range.
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

/// A shared multi-threaded runtime for a whole benchmark family. Fixed worker
/// count keeps the numbers reproducible on a busy 16-core box (only a handful of
/// tasks — two brokers plus the connection's reader/writer — are ever hot).
fn runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
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
async fn setup(transport: Transport) -> (Socket, Socket) {
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
            let addr = server.bind("127.0.0.1:0").await.unwrap();
            client.connect(addr).await.unwrap();
        }
        Transport::Tls => {
            let (server_cfg, client_cfg) = tls_configs();
            let addr = server.bind_tls("127.0.0.1:0", server_cfg).await.unwrap();
            client
                .connect_tls(addr, "localhost", client_cfg)
                .await
                .unwrap();
        }
    }

    // One buffered probe. It is delivered the instant the handshake completes,
    // so receiving it means the connection is established and steady-state
    // timing can begin. (Sends buffer while there is no peer, so this can never
    // be lost.)
    server
        .send(Bytes::from_static(b"__warmup__"))
        .await
        .unwrap();
    let got = timeout(Duration::from_secs(10), client.recv())
        .await
        .expect("warmup timed out waiting for the connection to come up")
        .expect("receiver closed during warmup");
    assert_eq!(&got[..], b"__warmup__");

    (server, client)
}

fn bench_e2e(
    c: &mut Criterion,
    rt: &Runtime,
    transport: Transport,
    group_name: &str,
    sizes: &[usize],
) {
    let mut group = c.benchmark_group(group_name);
    for &size in sizes {
        let m = batch_for(size);
        group.throughput(Throughput::Elements(m));

        let (server, client) = rt.block_on(setup(transport));
        // Cheap-to-clone shared payload: cloning a `Bytes` bumps a refcount, so
        // the benchmark measures the pipeline, not payload allocation.
        let payload = Bytes::from(vec![0xABu8; size]);

        group.bench_with_input(BenchmarkId::from_parameter(label(size)), &size, |b, _| {
            b.iter_custom(|iters| {
                rt.block_on(async {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let start = Instant::now();
                        for _ in 0..m {
                            server.send(payload.clone()).await.unwrap();
                        }
                        for _ in 0..m {
                            black_box(client.recv().await.unwrap());
                        }
                        total += start.elapsed();
                    }
                    total
                })
            });
        });

        rt.block_on(async {
            server.close().await;
            client.close().await;
        });
    }
    group.finish();
}

fn bench_framing(c: &mut Criterion, rt: &Runtime) {
    use aiomsg::protocol::{data, decode, read_frame, write_frame};

    let mut group = c.benchmark_group("framing");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(3));

    for &size in PAYLOADS {
        let frame_len = (4 + 1 + size) as u64; // u32 prefix + type byte + payload
        group.throughput(Throughput::Bytes(frame_len));
        let payload = vec![0xABu8; size];

        group.bench_with_input(BenchmarkId::from_parameter(label(size)), &size, |b, _| {
            b.iter_custom(|iters| {
                // One block_on for the whole sample amortises its fixed cost;
                // each inner pass is a single encode -> frame -> deframe ->
                // decode round-trip through in-memory buffers.
                rt.block_on(async {
                    let start = Instant::now();
                    for _ in 0..iters {
                        let mut buf: Vec<u8> = Vec::with_capacity(frame_len as usize);
                        write_frame(&mut buf, &data(&payload)).await.unwrap();
                        let mut cursor = std::io::Cursor::new(buf);
                        let frame = read_frame(&mut cursor).await.unwrap().unwrap();
                        black_box(decode(frame));
                    }
                    start.elapsed()
                })
            });
        });
    }
    group.finish();
}

fn e2e_plain(c: &mut Criterion) {
    let rt = runtime();
    bench_e2e(c, &rt, Transport::Plain, "e2e_plain", PAYLOADS);
}

fn e2e_tls(c: &mut Criterion) {
    let rt = runtime();
    // The async (tokio-rustls) TLS path drives read/write through the reactor
    // without holding a lock across a blocking write, so all payload sizes are
    // safe here — unlike the sync crate, which deadlocks above 64 B.
    bench_e2e(c, &rt, Transport::Tls, "e2e_tls", PAYLOADS);
}

fn framing(c: &mut Criterion) {
    let rt = runtime();
    bench_framing(c, &rt);
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
