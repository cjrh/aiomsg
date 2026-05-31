//! Integration tests: two (or more) sockets talking over real TCP loopback,
//! exercising the full synchronous stack.

use std::collections::HashSet;
use std::thread::sleep;
use std::time::Duration;

use aiomsg::{DeliveryGuarantee, SendMode, Socket};

const SHORT: Duration = Duration::from_secs(3);

#[test]
fn bind_sends_to_connect_with_buffering() {
    let server = Socket::new();
    let addr = server.bind("127.0.0.1:0").unwrap();

    let client = Socket::new();
    client.connect(addr).unwrap();

    // Sent immediately — exercises buffering until the first peer connects.
    server.send("hello").unwrap();

    assert_eq!(client.recv_timeout(SHORT).as_deref(), Some(&b"hello"[..]));

    server.close();
    client.close();
}

#[test]
fn round_robin_distributes_one_each() {
    let server = Socket::builder().send_mode(SendMode::RoundRobin).build();
    let addr = server.bind("127.0.0.1:0").unwrap();

    let c1 = Socket::new();
    let c2 = Socket::new();
    c1.connect(addr).unwrap();
    c2.connect(addr).unwrap();
    sleep(Duration::from_millis(300));

    for i in 0..4u8 {
        server.send(vec![i]).unwrap();
    }

    let mut seen: HashSet<u8> = HashSet::new();
    let mut count1 = 0;
    let mut count2 = 0;
    for _ in 0..2 {
        if let Some(m) = c1.recv_timeout(SHORT) {
            seen.insert(m[0]);
            count1 += 1;
        }
        if let Some(m) = c2.recv_timeout(SHORT) {
            seen.insert(m[0]);
            count2 += 1;
        }
    }
    assert_eq!(count1, 2);
    assert_eq!(count2, 2);
    assert_eq!(seen, HashSet::from([0, 1, 2, 3]));

    server.close();
    c1.close();
    c2.close();
}

#[test]
fn publish_sends_to_every_peer() {
    let server = Socket::builder().send_mode(SendMode::Publish).build();
    let addr = server.bind("127.0.0.1:0").unwrap();

    let c1 = Socket::new();
    let c2 = Socket::new();
    c1.connect(addr).unwrap();
    c2.connect(addr).unwrap();
    sleep(Duration::from_millis(300));

    server.send("news").unwrap();

    assert_eq!(c1.recv_timeout(SHORT).as_deref(), Some(&b"news"[..]));
    assert_eq!(c2.recv_timeout(SHORT).as_deref(), Some(&b"news"[..]));

    server.close();
    c1.close();
    c2.close();
}

#[test]
fn identity_routing_targets_one_peer() {
    let server = Socket::new();
    let addr = server.bind("127.0.0.1:0").unwrap();

    let id1 = [1u8; 16];
    let id2 = [2u8; 16];
    let c1 = Socket::builder().identity(id1).build();
    let c2 = Socket::builder().identity(id2).build();
    c1.connect(addr).unwrap();
    c2.connect(addr).unwrap();
    sleep(Duration::from_millis(300));

    server.send_to(id1, "for-one").unwrap();
    server.send_to(id2, "for-two").unwrap();

    assert_eq!(c1.recv_timeout(SHORT).as_deref(), Some(&b"for-one"[..]));
    assert_eq!(c2.recv_timeout(SHORT).as_deref(), Some(&b"for-two"[..]));

    server.close();
    c1.close();
    c2.close();
}

#[test]
fn at_least_once_delivers_and_acks() {
    let server = Socket::builder()
        .send_mode(SendMode::RoundRobin)
        .delivery_guarantee(DeliveryGuarantee::AtLeastOnce)
        .build();
    let addr = server.bind("127.0.0.1:0").unwrap();

    let client = Socket::new();
    client.connect(addr).unwrap();
    sleep(Duration::from_millis(200));

    server.send("reliable").unwrap();

    assert_eq!(
        client.recv_timeout(SHORT).as_deref(),
        Some(&b"reliable"[..])
    );
    // No duplicate well within the 5s resend window.
    assert_eq!(client.recv_timeout(Duration::from_millis(500)), None);

    server.close();
    client.close();
}

#[test]
fn messages_iterator_yields_in_order() {
    let server = Socket::builder().send_mode(SendMode::Publish).build();
    let addr = server.bind("127.0.0.1:0").unwrap();

    let client = Socket::new();
    client.connect(addr).unwrap();
    sleep(Duration::from_millis(200));

    for i in 0..5u8 {
        server.send(vec![i]).unwrap();
    }

    let mut got = Vec::new();
    for _ in 0..5 {
        got.push(client.recv_timeout(SHORT).unwrap()[0]);
    }
    assert_eq!(got, vec![0, 1, 2, 3, 4]);

    server.close();
    client.close();
}

#[test]
fn connect_end_reconnects_after_server_restart() {
    let server1 = Socket::builder().send_mode(SendMode::Publish).build();
    let addr = server1.bind("127.0.0.1:0").unwrap();

    let client = Socket::new();
    client.connect(addr).unwrap();

    server1.send("first").unwrap();
    assert_eq!(client.recv_timeout(SHORT).as_deref(), Some(&b"first"[..]));

    server1.close();

    let server2 = Socket::builder().send_mode(SendMode::Publish).build();
    server2.bind(addr).unwrap();

    server2.send("second").unwrap();
    assert_eq!(
        client.recv_timeout(Duration::from_secs(5)).as_deref(),
        Some(&b"second"[..])
    );

    server2.close();
    client.close();
}
