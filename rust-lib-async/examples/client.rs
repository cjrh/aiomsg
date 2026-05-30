//! The connect end of the two-process demo: prints whatever the server sends.
//! Reconnects automatically if the server restarts.

use aiomsg::Socket;

#[tokio::main]
async fn main() -> aiomsg::Result<()> {
    let sock = Socket::new();
    sock.connect("127.0.0.1:25000".parse::<std::net::SocketAddr>().unwrap())
        .await?;

    while let Some(msg) = sock.recv().await {
        println!("received: {}", String::from_utf8_lossy(&msg));
    }
    Ok(())
}
