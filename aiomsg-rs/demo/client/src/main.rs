use aiomsg_rs::{PeerConfig, Socket};
use anyhow::Result;
use std::time::Duration;
use async_std::task;


async fn test1() -> Result<()> {
    let sock = Socket::new();
    println!("sock: {:?}", &sock);
    sock.connect(&PeerConfig::new("127.0.0.1", 61111)).await?;
    for _i in 0..100 {
        sock.send(b"test").await?;
        async_std::task::sleep(Duration::from_millis(1000)).await;
    }
    Ok(())
}

#[async_std::main]
async fn main() -> Result<()> {
    std::env::set_var("RUST_LOG", "info");
    pretty_env_logger::init_timed();
    test1().await?;
    task::spawn(test1());
    Ok(())
}
