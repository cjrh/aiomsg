use std::convert::{TryFrom, TryInto};
use async_std::net::{TcpStream, TcpListener};
use async_std::prelude::*;
use async_std::{io, task};

async fn read_msg(reader: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut size_bytes: [u8; 4] = [0; 4];
    reader.read_exact(&mut size_bytes).await?;
    let size = i32::from_be_bytes(size_bytes);

    let mut buf = vec![0; size as usize];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}


#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use async_std::prelude::*;
    use async_std::net::{TcpStream, TcpListener};
    use async_std::{io, task};
    use async_std::future;

    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }

    #[test]
    fn test_read_msg() {
        async_std::task::block_on(async {
            async fn server() -> io::Result<()> {
                let listener = TcpListener::bind("127.0.0.1:27001").await?;
                let (mut stream, addr) = listener.accept().await?;
                let received = read_msg(&mut stream).await?;
                println!("{:?}", String::from_utf8_lossy(&received));
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
                println!("Size as a u32: {:?}", &size_bytes);
                assert_eq!(size_bytes, [0x00, 0x00, 0x00, 0x10]);

                stream.write_all(&size_bytes).await?;
                stream.write_all(msg_bytes).await?;
                println!("client: sent bytes, exiting.");
                Ok(())
            }

            let server_task = task::spawn(server());
            let client_task = task::spawn(client());
            server_task.await.unwrap(); // actual test
            client_task.await.unwrap(); // cleanup
        })
    }
}

