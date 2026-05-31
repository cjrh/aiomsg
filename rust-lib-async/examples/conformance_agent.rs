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
    match role {
        "bind" => {
            sock.bind(addr).await?;
        }
        _ => {
            sock.connect(addr).await?;
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
