//! The bind end of the two-process demo: publishes the time once a second.
//!
//! ```text
//! cargo run --example server
//! cargo run --example client   # in another terminal
//! ```

use std::time::Duration;

use aiomsg::{SendMode, Socket};

fn main() -> aiomsg::Result<()> {
    let sock = Socket::builder().send_mode(SendMode::Publish).build();
    let addr = sock.bind("127.0.0.1:25000")?;
    println!("bound to {addr}, publishing the time...");

    loop {
        let now = format!("{:?}", std::time::SystemTime::now());
        sock.send(now)?;
        std::thread::sleep(Duration::from_secs(1));
    }
}
