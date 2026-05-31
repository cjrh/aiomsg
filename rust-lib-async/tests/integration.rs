//! Integration tests: two (or more) sockets talking to each other over real
//! TCP loopback, exercising the full stack (handshake, routing, buffering,
//! delivery guarantee, reconnection).

use std::collections::HashSet;
use std::time::Duration;

use aiomsg::{Bytes, DeliveryGuarantee, SendMode, Socket};
use tokio::time::timeout;
use tokio_stream::StreamExt;

const SHORT: Duration = Duration::from_secs(3);

async fn recv_within(sock: &Socket, dur: Duration) -> Option<Bytes> {
    timeout(dur, sock.recv()).await.ok().flatten()
}

#[tokio::test]
async fn bind_sends_to_connect_with_buffering() {
    let server = Socket::new();
    let addr = server.bind("127.0.0.1:0").await.unwrap();

    let client = Socket::new();
    client.connect(addr).await.unwrap();

    // Sent immediately — possibly before the handshake completes — so this also
    // exercises send buffering until the first peer is connected.
    server.send("hello").await.unwrap();

    let got = recv_within(&client, SHORT).await;
    assert_eq!(got.as_deref(), Some(&b"hello"[..]));

    server.close().await;
    client.close().await;
}

#[tokio::test]
async fn round_robin_distributes_one_each() {
    let server = Socket::builder().send_mode(SendMode::RoundRobin).build();
    let addr = server.bind("127.0.0.1:0").await.unwrap();

    let c1 = Socket::new();
    let c2 = Socket::new();
    c1.connect(addr).await.unwrap();
    c2.connect(addr).await.unwrap();

    // Let both handshakes complete so the round-robin set is stable.
    tokio::time::sleep(Duration::from_millis(300)).await;

    for i in 0..4u8 {
        server.send(vec![i]).await.unwrap();
    }

    let mut seen: HashSet<u8> = HashSet::new();
    let mut count1 = 0;
    let mut count2 = 0;
    for _ in 0..2 {
        if let Some(m) = recv_within(&c1, SHORT).await {
            seen.insert(m[0]);
            count1 += 1;
        }
        if let Some(m) = recv_within(&c2, SHORT).await {
            seen.insert(m[0]);
            count2 += 1;
        }
    }

    assert_eq!(
        count1, 2,
        "c1 should get exactly 2 of 4 round-robin messages"
    );
    assert_eq!(
        count2, 2,
        "c2 should get exactly 2 of 4 round-robin messages"
    );
    assert_eq!(seen, HashSet::from([0, 1, 2, 3]));

    server.close().await;
    c1.close().await;
    c2.close().await;
}

#[tokio::test]
async fn publish_sends_to_every_peer() {
    let server = Socket::builder().send_mode(SendMode::Publish).build();
    let addr = server.bind("127.0.0.1:0").await.unwrap();

    let c1 = Socket::new();
    let c2 = Socket::new();
    c1.connect(addr).await.unwrap();
    c2.connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    server.send("news").await.unwrap();

    assert_eq!(recv_within(&c1, SHORT).await.as_deref(), Some(&b"news"[..]));
    assert_eq!(recv_within(&c2, SHORT).await.as_deref(), Some(&b"news"[..]));

    server.close().await;
    c1.close().await;
    c2.close().await;
}

#[tokio::test]
async fn identity_routing_targets_one_peer() {
    let server = Socket::new();
    let addr = server.bind("127.0.0.1:0").await.unwrap();

    let id1: [u8; 16] = [1; 16];
    let id2: [u8; 16] = [2; 16];
    let c1 = Socket::builder().identity(id1).build();
    let c2 = Socket::builder().identity(id2).build();
    c1.connect(addr).await.unwrap();
    c2.connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    server.send_to(id1, "for-one").await.unwrap();
    server.send_to(id2, "for-two").await.unwrap();

    assert_eq!(
        recv_within(&c1, SHORT).await.as_deref(),
        Some(&b"for-one"[..])
    );
    assert_eq!(
        recv_within(&c2, SHORT).await.as_deref(),
        Some(&b"for-two"[..])
    );

    server.close().await;
    c1.close().await;
    c2.close().await;
}

#[tokio::test]
async fn at_least_once_delivers_and_acks() {
    // With AT_LEAST_ONCE the sender uses DATA_REQ and the receiver ACKs; the
    // resend timer must be cancelled by the ACK (no duplicate arrives).
    let server = Socket::builder()
        .send_mode(SendMode::RoundRobin)
        .delivery_guarantee(DeliveryGuarantee::AtLeastOnce)
        .build();
    let addr = server.bind("127.0.0.1:0").await.unwrap();

    let client = Socket::new();
    client.connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    server.send("reliable").await.unwrap();

    assert_eq!(
        recv_within(&client, SHORT).await.as_deref(),
        Some(&b"reliable"[..])
    );
    // No duplicate within a window comfortably shorter than the 5s resend timer.
    assert_eq!(recv_within(&client, Duration::from_millis(500)).await, None);

    server.close().await;
    client.close().await;
}

#[tokio::test]
async fn messages_stream_yields_in_order() {
    let server = Socket::builder().send_mode(SendMode::Publish).build();
    let addr = server.bind("127.0.0.1:0").await.unwrap();

    let client = Socket::new();
    client.connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    for i in 0..5u8 {
        server.send(vec![i]).await.unwrap();
    }

    let mut stream = Box::pin(client.messages());
    let mut got = Vec::new();
    for _ in 0..5 {
        let m = timeout(SHORT, stream.next()).await.unwrap().unwrap();
        got.push(m[0]);
    }
    assert_eq!(got, vec![0, 1, 2, 3, 4]);

    server.close().await;
    client.close().await;
}

#[tokio::test]
async fn connect_end_reconnects_after_server_restart() {
    // Bind, connect, confirm delivery, kill the server, bring a new one up on
    // the same port, and confirm the client reconnected and receives again.
    let server1 = Socket::builder().send_mode(SendMode::Publish).build();
    let addr = server1.bind("127.0.0.1:0").await.unwrap();

    let client = Socket::new();
    client.connect(addr).await.unwrap();

    server1.send("first").await.unwrap();
    assert_eq!(
        recv_within(&client, SHORT).await.as_deref(),
        Some(&b"first"[..])
    );

    server1.close().await;

    // New server on the same address; the client should reconnect on its own.
    let server2 = Socket::builder().send_mode(SendMode::Publish).build();
    server2.bind(addr).await.unwrap();

    // Buffered on server2 until the client reconnects (≈ reconnect delay).
    server2.send("second").await.unwrap();
    assert_eq!(
        recv_within(&client, Duration::from_secs(5))
            .await
            .as_deref(),
        Some(&b"second"[..])
    );

    server2.close().await;
    client.close().await;
}
