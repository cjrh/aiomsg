//! Conformance test agent for the cross-language interop suite.
//!
//! A tiny, uniform CLI that the `conformance/` runner drives. The Python
//! implementation ships an agent with the same flags, so the runner can pair
//! any two languages in any role and assert they interoperate on the wire.
//!
//! Flags (all `--key value`):
//!   --role      bind | connect
//!   --host      e.g. 127.0.0.1
//!   --port      e.g. 25000
//!   --send-mode roundrobin | publish
//!   --behavior  source | sink | echo
//!   --count     number of messages to send/receive
//!   --prefix    message prefix; message i is "<prefix><i>"
//!   --delivery  at-most-once | at-least-once   (default at-most-once)
//!   --identity  32 hex chars (16 bytes)         (optional)
//!   --linger    seconds a source waits after sending (default 1.0)
//!   --tls       (flag) wrap the transport in TLS
//!   --tls-cert / --tls-key   server cert + key PEM (bind side)
//!   --tls-ca                 trusted CA PEM (connect side)
//!   --tls-server-name        name to verify (connect side; default = host)
//!
//! A `sink` prints each received message (utf-8) on its own line and exits
//! after `count` messages.

use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use aiomsg::{DeliveryGuarantee, Identity, SendMode, Socket};

fn main() -> aiomsg::Result<()> {
    let args = parse_args();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run(args))
}

fn parse_args() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut it = std::env::args().skip(1);
    while let Some(key) = it.next() {
        if let Some(name) = key.strip_prefix("--") {
            if let Some(value) = it.next() {
                map.insert(name.to_string(), value);
            }
        }
    }
    map
}

async fn run(args: HashMap<String, String>) -> aiomsg::Result<()> {
    let role = args.get("role").map(String::as_str).unwrap_or("connect");
    let host = args.get("host").map(String::as_str).unwrap_or("127.0.0.1");
    let port: u16 = args
        .get("port")
        .and_then(|p| p.parse().ok())
        .unwrap_or(25000);
    let behavior = args.get("behavior").map(String::as_str).unwrap_or("sink");
    let count: usize = args.get("count").and_then(|c| c.parse().ok()).unwrap_or(10);
    let prefix = args.get("prefix").map(String::as_str).unwrap_or("m");
    let linger: f64 = args
        .get("linger")
        .and_then(|l| l.parse().ok())
        .unwrap_or(1.0);

    let send_mode = match args.get("send-mode").map(String::as_str) {
        Some("publish") => SendMode::Publish,
        _ => SendMode::RoundRobin,
    };
    let delivery = match args.get("delivery").map(String::as_str) {
        Some("at-least-once") => DeliveryGuarantee::AtLeastOnce,
        _ => DeliveryGuarantee::AtMostOnce,
    };

    let mut builder = Socket::builder()
        .send_mode(send_mode)
        .delivery_guarantee(delivery);
    if let Some(hex) = args.get("identity") {
        builder = builder.identity(parse_identity(hex));
    }
    let sock = builder.build();

    let addr: std::net::SocketAddr = format!("{host}:{port}").parse().expect("valid addr");
    let tls = args.get("tls").map(|v| v == "true").unwrap_or(false);
    match (role, tls) {
        ("bind", false) => {
            sock.bind(addr).await?;
        }
        ("bind", true) => {
            bind_tls(&sock, addr, &args).await?;
        }
        (_, false) => {
            sock.connect(addr).await?;
        }
        (_, true) => {
            connect_tls(&sock, addr, host, &args).await?;
        }
    }

    match behavior {
        "source" => {
            for i in 0..count {
                sock.send(format!("{prefix}{i}")).await?;
            }
            tokio::time::sleep(Duration::from_secs_f64(linger)).await;
        }
        "echo" => {
            for _ in 0..count {
                if let Some((id, msg)) = sock.recv_identity().await {
                    sock.send_to(id, msg).await?;
                }
            }
            tokio::time::sleep(Duration::from_secs_f64(linger)).await;
        }
        _ => {
            // sink
            let stdout = std::io::stdout();
            let mut received = 0;
            while received < count {
                match sock.recv().await {
                    Some(msg) => {
                        let mut lock = stdout.lock();
                        let _ = lock.write_all(&msg);
                        let _ = lock.write_all(b"\n");
                        let _ = lock.flush();
                        received += 1;
                    }
                    None => break,
                }
            }
        }
    }

    sock.close().await;
    Ok(())
}

fn parse_identity(hex: &str) -> Identity {
    let mut id = [0u8; 16];
    for (i, slot) in id.iter_mut().enumerate() {
        if let Some(byte) = hex.get(i * 2..i * 2 + 2) {
            *slot = u8::from_str_radix(byte, 16).unwrap_or(0);
        }
    }
    id
}

#[cfg(feature = "tls")]
async fn bind_tls(
    sock: &Socket,
    addr: std::net::SocketAddr,
    args: &HashMap<String, String>,
) -> aiomsg::Result<()> {
    let cert = args.get("tls-cert").expect("--tls-cert required");
    let key = args.get("tls-key").expect("--tls-key required");
    sock.bind_tls(addr, tls::server_config(cert, key)).await?;
    Ok(())
}

#[cfg(feature = "tls")]
async fn connect_tls(
    sock: &Socket,
    addr: std::net::SocketAddr,
    host: &str,
    args: &HashMap<String, String>,
) -> aiomsg::Result<()> {
    let ca = args.get("tls-ca").expect("--tls-ca required");
    let name = args
        .get("tls-server-name")
        .map(String::as_str)
        .unwrap_or(host);
    sock.connect_tls(addr, name.to_string(), tls::client_config(ca))
        .await?;
    Ok(())
}

#[cfg(not(feature = "tls"))]
async fn bind_tls(
    _: &Socket,
    _: std::net::SocketAddr,
    _: &HashMap<String, String>,
) -> aiomsg::Result<()> {
    panic!("--tls requires the `tls` feature");
}

#[cfg(not(feature = "tls"))]
async fn connect_tls(
    _: &Socket,
    _: std::net::SocketAddr,
    _: &str,
    _: &HashMap<String, String>,
) -> aiomsg::Result<()> {
    panic!("--tls requires the `tls` feature");
}

#[cfg(feature = "tls")]
mod tls {
    use std::fs::File;
    use std::io::BufReader;
    use std::sync::Arc;

    use aiomsg::rustls::crypto::ring;
    use aiomsg::rustls::{ClientConfig, RootCertStore, ServerConfig};

    pub fn server_config(cert_pem: &str, key_pem: &str) -> Arc<ServerConfig> {
        let certs = rustls_pemfile::certs(&mut BufReader::new(File::open(cert_pem).unwrap()))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key = rustls_pemfile::private_key(&mut BufReader::new(File::open(key_pem).unwrap()))
            .unwrap()
            .expect("no private key in PEM");
        Arc::new(
            ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap(),
        )
    }

    pub fn client_config(ca_pem: &str) -> Arc<ClientConfig> {
        let mut roots = RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut BufReader::new(File::open(ca_pem).unwrap())) {
            roots.add(cert.unwrap()).unwrap();
        }
        Arc::new(
            ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    }
}
