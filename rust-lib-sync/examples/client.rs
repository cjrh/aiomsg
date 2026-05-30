//! The connect end of the two-process demo: prints whatever the server sends,
//! reconnecting automatically if the server restarts.

use aiomsg::Socket;

fn main() -> aiomsg::Result<()> {
    let sock = Socket::new();
    sock.connect("127.0.0.1:25000")?;

    for msg in sock.messages() {
        println!("received: {}", String::from_utf8_lossy(&msg));
    }
    Ok(())
}
