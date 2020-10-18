use aiomsg_rs::Socket;
use anyhow::Result;
use std::time::Duration;

#[async_std::main]
async fn main() -> Result<()> {
    println!("Hello, world!");
    let sock = Socket::new();
    sock.connect("127.0.0.1", 61111, None).await?;
    for _i in 0..10 {
        sock.send(b"test").await?;
        async_std::task::sleep(Duration::from_millis(1000)).await;
    }
    println!("Done");
    Ok(())
}
