use std::convert::{TryFrom, TryInto};

use log::{info, trace, warn, error};
use async_std::prelude::*;
use async_std::net::{TcpStream, TcpListener};
use async_std::{io, task};

async fn read_msg(reader: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut size_bytes: [u8; 4] = [0; 4];
    reader.read_exact(&mut size_bytes).await?;
    let size = i32::from_be_bytes(size_bytes);

    let mut buf = vec![0; size as usize];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn send_msg(writer: &mut TcpStream, data: &[u8]) -> io::Result<()> {
    let size_bytes = (data.len() as i32).to_be_bytes();
    trace!("Size as a u32: {:?}", &size_bytes);

    writer.write_all(&size_bytes).await?;
    writer.write_all(data).await?;
    Ok(())
}

async fn send_string(writer: &mut TcpStream, string: &str) -> io::Result<()> { 
    let msg_bytes = string.as_bytes();
    send_msg(writer, msg_bytes).await?;
    Ok(())
}


#[cfg(test)]
mod tests {
    extern crate pretty_env_logger;
    use std::time::Duration;

    use super::*;
    use async_std::prelude::*;
    use async_std::net::{TcpStream, TcpListener};
    use async_std::{io, task};
    use async_std::future;

    #[test]
    fn it_works() {
        std::env::set_var("RUST_LOG", "info");
        pretty_env_logger::init();
        assert_eq!(2 + 2, 4);
    }

    #[test]
    fn test_read_msg() {
        async_std::task::block_on(async {
            async fn server() -> io::Result<()> {
                let listener = TcpListener::bind("127.0.0.1:27001").await?;
                let (mut stream, addr) = listener.accept().await?;
                let received = read_msg(&mut stream).await?;
                trace!("{:?}", String::from_utf8_lossy(&received));
                assert_eq!(
                    String::from_utf8_lossy(&received),
                    "aiomsg-heartbeat"
                );
                Ok(())
            }

            async fn client() -> io::Result<()> {
                let mut stream = match future::timeout(
                    Duration::from_secs(1),
                    TcpStream::connect("127.0.0.1:27001"),
                ).await {
                    Ok(s) => s?,
                    TimeoutError => panic!("Timed out")
                };

                let msg = String::from("aiomsg-heartbeat");
                let msg_bytes = msg.as_bytes();
                let size_bytes = (msg_bytes.len() as i32).to_be_bytes();
                info!("Size as a u32: {:?}", &size_bytes);
                assert_eq!(size_bytes, [0x00, 0x00, 0x00, 0x10]);

                stream.write_all(&size_bytes).await?;
                stream.write_all(msg_bytes).await?;
                Ok(())
            }

            let server_task = task::spawn(server());
            let client_task = task::spawn(client());
            server_task.await.unwrap(); // actual test
            client_task.await.unwrap(); // cleanup
        })
    }

    #[test]
    fn test_send_msg() {
        async_std::task::block_on(async {
            async fn server() -> io::Result<()> {
                let listener = TcpListener::bind("127.0.0.1:27002").await?;
                let (mut stream, addr) = listener.accept().await?;
                let received = read_msg(&mut stream).await?;
                trace!("{:?}", String::from_utf8_lossy(&received));
                assert_eq!(
                    String::from_utf8_lossy(&received),
                    "hello there"
                );
                Ok(())
            }

            async fn client() -> io::Result<()> {
                let mut stream = match future::timeout(
                    Duration::from_secs(1),
                    TcpStream::connect("127.0.0.1:27002"),
                ).await {
                    Ok(s) => s?,
                    TimeoutError => panic!("Timed out")
                };

                send_string(&mut stream, "hello there").await?;
                Ok(())
            }

            let server_task = task::spawn(server());
            let client_task = task::spawn(client());
            server_task.await.unwrap(); // actual test
            client_task.await.unwrap(); // cleanup
        })
    }
}

