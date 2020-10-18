use async_std::future;
use async_std::{io, net::TcpStream};
use std::time::Duration;

/// Picks an unused port and returns strings like "127.0.0.1:61876"
pub fn get_addr() -> String {
    format!(
        "127.0.0.1:{}",
        portpicker::pick_unused_port().expect("No free ports")
    )
}

pub async fn give_client(addr: String, wait: Option<u64>) -> io::Result<TcpStream> {
    match future::timeout(
        Duration::from_secs(wait.unwrap_or(1)),
        TcpStream::connect(addr),
    )
    .await
    {
        Ok(s) => s,
        Err(_) => panic!("Timed out"),
    }
}
