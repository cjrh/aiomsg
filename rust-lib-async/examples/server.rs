//! The bind end of the two-process demo: publishes the time once a second.
//!
//! Run this, then run the `client` example in another terminal:
//!
//! ```text
//! cargo run --example server
//! cargo run --example client
//! ```

use std::time::Duration;

use aiomsg::{SendMode, Socket};

#[tokio::main]
async fn main() -> aiomsg::Result<()> {
    let sock = Socket::builder().send_mode(SendMode::Publish).build();
    let addr = sock.bind("127.0.0.1:25000").await?;
    println!("bound to {addr}, publishing the time...");

    loop {
        // Encoding is the application's choice; here we send a plain string.
        let now = format!("{:?}", std::time::SystemTime::now());
        sock.send(now).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
