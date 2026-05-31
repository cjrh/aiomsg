//! Conformance test agent for the cross-language interop suite (synchronous
//! Rust). Same CLI as the Python, Go, and async-Rust agents.

use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use aiomsg::{DeliveryGuarantee, Identity, SendMode, Socket};

fn main() {
    let args = parse_args();

    let role = arg(&args, "role", "connect");
    let host = arg(&args, "host", "127.0.0.1");
    let port: u16 = arg(&args, "port", "25000").parse().unwrap_or(25000);
    let behavior = arg(&args, "behavior", "sink");
    let count: usize = arg(&args, "count", "10").parse().unwrap_or(10);
    let prefix = arg(&args, "prefix", "m");
    let linger: f64 = arg(&args, "linger", "1.0").parse().unwrap_or(1.0);

    let send_mode = if arg(&args, "send-mode", "roundrobin") == "publish" {
        SendMode::Publish
    } else {
        SendMode::RoundRobin
    };
    let delivery = if arg(&args, "delivery", "at-most-once") == "at-least-once" {
        DeliveryGuarantee::AtLeastOnce
    } else {
        DeliveryGuarantee::AtMostOnce
    };

    let mut builder = Socket::builder()
        .send_mode(send_mode)
        .delivery_guarantee(delivery);
    if let Some(hex) = args.get("identity") {
        builder = builder.identity(parse_identity(hex));
    }
    let sock = builder.build();

    let addr = format!("{host}:{port}");
    let tls = arg(&args, "tls", "false") == "true";
    let ok = match (role, tls) {
        ("bind", false) => sock.bind(&addr).map(|_| ()),
        ("bind", true) => bind_tls(&sock, &addr, &args).map(|_| ()),
        (_, false) => sock.connect(&addr),
        (_, true) => connect_tls(&sock, &addr, host, &args),
    };
    if let Err(e) = ok {
        eprintln!("{role} failed: {e}");
        std::process::exit(1);
    }

    let linger_dur = Duration::from_secs_f64(linger);
    match behavior {
        "source" => {
            for i in 0..count {
                let _ = sock.send(format!("{prefix}{i}"));
            }
            std::thread::sleep(linger_dur);
        }
        "echo" => {
            for _ in 0..count {
                if let Some((id, msg)) = sock.recv_identity() {
                    let _ = sock.send_to(id, msg);
                }
            }
            std::thread::sleep(linger_dur);
        }
        _ => {
            let stdout = std::io::stdout();
            let mut received = 0;
            while received < count {
                match sock.recv() {
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

    sock.close();
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

fn arg<'a>(args: &'a HashMap<String, String>, key: &str, default: &'a str) -> &'a str {
    args.get(key).map(String::as_str).unwrap_or(default)
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
fn bind_tls(sock: &Socket, addr: &str, args: &HashMap<String, String>) -> aiomsg::Result<()> {
    let cert = arg(args, "tls-cert", "");
    let key = arg(args, "tls-key", "");
    sock.bind_tls(addr, tls::server_config(cert, key))?;
    Ok(())
}

#[cfg(feature = "tls")]
fn connect_tls(
    sock: &Socket,
    addr: &str,
    host: &str,
    args: &HashMap<String, String>,
) -> aiomsg::Result<()> {
    let ca = arg(args, "tls-ca", "");
    let name = arg(args, "tls-server-name", host).to_string();
    sock.connect_tls(addr, name, tls::client_config(ca))
}

#[cfg(not(feature = "tls"))]
fn bind_tls(_: &Socket, _: &str, _: &HashMap<String, String>) -> aiomsg::Result<()> {
    panic!("--tls requires the `tls` feature");
}

#[cfg(not(feature = "tls"))]
fn connect_tls(_: &Socket, _: &str, _: &str, _: &HashMap<String, String>) -> aiomsg::Result<()> {
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
