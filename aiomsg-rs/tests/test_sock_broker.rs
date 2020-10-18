mod test_utils;

use aiomsg_rs::{PeerConfig, Result, Socket};
use async_std::sync::Arc;
use async_std::{io, task};
use log::info;
use std::time::Duration;

#[test]
fn setup() {
    std::env::set_var("RUST_LOG", "info");
    pretty_env_logger::init();
}

#[test]
fn blah() {
    assert_eq!(4, 2 + 2);
}

#[test]
fn crate_socket_lyfe() {
    let sock = Socket::new();
    info!("{:?}", &sock);
}

#[async_std::test]
async fn crate_socket_run_async_method() {
    let sock = Socket::new();
    sock.clone().test_call().await;
}

#[async_std::test]
async fn crate_sock_broker() -> io::Result<()> {
    info!("Running sock_broker");
    let _addr = test_utils::get_addr();
    let peer = Arc::new(PeerConfig::new("127.0.0.1", 27005));

    async fn client(peer: Arc<PeerConfig>) -> Result<()> {
        async_std::task::sleep(Duration::from_secs(1)).await;
        let sock = Socket::new();
        sock.connect(&peer).await?;
        async_std::task::sleep(Duration::from_secs(1)).await;
        sock.send(b"blah1").await?;
        sock.send(b"blah2").await?;
        sock.send(b"blah3").await?;
        info!("Leaving client...");
        Ok(())
    }

    async fn server(peer: Arc<PeerConfig>) -> io::Result<Vec<String>> {
        let mut result = vec![];
        let sock = Socket::new();
        // TODO: change the sig for bind too
        // sock.bind(peer).await?;
        sock.bind("127.0.0.1", 27005, None).await?;
        let rng: std::ops::Range<u32> = 0..3;
        info!("Waiting for messages...");
        for _i in rng {
            match sock.recv().await? {
                Some(msg) => {
                    info!("Received: {:?}", &msg);
                    result.push(String::from_utf8_lossy(&msg[..]).to_string());
                }
                None => break,
            }
        }
        info!("Leaving server");
        Ok(result)
    }

    info!("Spawning client");
    task::spawn(client(peer.clone()));
    let received = server(peer.clone()).await?;
    info!("This was received: {:?}", &received);
    assert_eq!(received, vec!["blah1", "blah2", "blah3"]);
    Ok(())
}
