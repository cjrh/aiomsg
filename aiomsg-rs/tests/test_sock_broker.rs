mod test_utils;

use aiomsg_rs::{Result, Socket};
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

    async fn client() -> Result<()> {
        async_std::task::sleep(Duration::from_secs(3)).await;
        let sock = Socket::new();
        sock.connect("127.0.0.1", 27005, None).await?;
        sock.send(b"blah1").await?;
        sock.send(b"blah2").await?;
        sock.send(b"blah3").await?;
        Ok(())
    }

    async fn server() -> io::Result<Vec<String>> {
        let mut result = vec![];
        let sock = Socket::new();
        sock.bind("127.0.0.1", 27005, None).await?;
        let rng: std::ops::Range<u32> = 0..3;
        for _i in rng {
            match sock.recv().await? {
                Some(msg) => {
                    info!("Received: {:?}", &msg);
                    result.push(String::from_utf8_lossy(&msg[..]).to_string());
                }
                None => break,
            }
        }
        Ok(result)
    }

    task::spawn(client());
    let received = server().await?;
    info!("This was received: {:?}", &received);
    assert_eq!(received, vec!["blah1", "blah2", "blah3"]);
    Ok(())
}
