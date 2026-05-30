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
    let ok = match role {
        "bind" => sock.bind(&addr).map(|_| ()),
        _ => sock.connect(&addr),
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
