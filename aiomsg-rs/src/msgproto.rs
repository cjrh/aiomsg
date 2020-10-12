use async_std::io;
// use async_std::net::{TcpListener, TcpStream};
use async_std::io::{BufReader, Read};
use async_std::net::TcpStream;
use async_std::prelude::*;
use futures::io::{AsyncRead, AsyncWrite, ReadExact};
// use futures::stream::Stream;
use log::{error, info, trace, warn};
use std::pin::Pin;

const HEADER_SIZE: usize = 4;
// TODO: change the i32
const MAX_MESSAGE_SIZE: i32 = 5_000_000;

pub async fn read_msg(reader: &TcpStream) -> Option<Vec<u8>> {
    let mut size_bytes: [u8; 4] = [0; HEADER_SIZE];
    // This hack seems to be necessary to activate the read/write methods on
    // "&TcpStream"
    let mut x = reader;
    if let Err(e) = x.read_exact(&mut size_bytes).await {
        trace!("Error reading size: {}", e);
        return None;
    };
    let size = i32::from_be_bytes(size_bytes);
    if size > MAX_MESSAGE_SIZE {
        warn!("Message is too big, discarding. Size was {}", &size);
        return None;
    }

    let mut buf = vec![0; size as usize];
    match x.read_exact(&mut buf).await {
        Ok(_n) => Some(buf),
        Err(_) => None,
    }
}

pub async fn read_string(reader: &TcpStream) -> Option<String> {
    match read_msg(reader).await {
        Some(bytes) => Some(String::from_utf8_lossy(&bytes).to_string()),
        None => None,
    }
}

pub async fn send_msg(writer: &TcpStream, data: &[u8]) -> io::Result<()> {
    let size_bytes = (data.len() as i32).to_be_bytes();
    trace!("Size as a u32: {:?}", &size_bytes);
    let mut x = writer;
    x.write_all(&size_bytes).await?;
    x.write_all(data).await?;
    Ok(())
}

// Using `impl AsyncWrite + Unpin` in place of TcpStream, trying to use
// things that only depend on futures, and not async-std
pub async fn send_string(writer: &TcpStream, string: &str) -> io::Result<()> {
    let msg_bytes = string.as_bytes();
    send_msg(writer, msg_bytes).await?;
    Ok(())
}

// TODO: read_json and send_json

#[cfg(test)]
mod tests {
    extern crate pretty_env_logger;
    use crate::test_utils;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    // use crate::test_utils::get_addr;
    use async_std::future;
    use async_std::net::{TcpListener, TcpStream};
    use async_std::{io, task};

    #[test]
    fn it_works() {
        std::env::set_var("RUST_LOG", "info");
        pretty_env_logger::init();
        assert_eq!(2 + 2, 4);
    }

    async fn server(addr: String) -> io::Result<Vec<String>> {
        async fn connection(stream: &TcpStream) -> io::Result<Vec<String>> {
            let mut received_messages: Vec<String> = vec![];
            while let Some(bytes) = read_msg(stream).await {
                let s = String::from_utf8_lossy(&bytes).to_string();
                received_messages.push(s);
            }
            Ok(received_messages)
        };

        let listener = TcpListener::bind(addr).await?;
        let (stream, _addr) = listener.accept().await?;
        // Single connection only - for tests. We don't accept any more
        // connections.
        let received_messages = connection(&stream).await?;
        info!("Leaving server...");
        Ok(received_messages)
    }

    async fn string_server(addr: String) -> io::Result<Vec<String>> {
        async fn connection(stream: &TcpStream) -> io::Result<Vec<String>> {
            let mut received_messages: Vec<String> = vec![];
            while let Some(s) = read_string(stream).await {
                received_messages.push(s);
            }
            Ok(received_messages)
        };

        let listener = TcpListener::bind(addr).await?;
        let fut = future::timeout(Duration::from_secs(1), listener.accept());
        let (stream, _addr) = match fut.await {
            Ok(t) => t?,
            Err(_) => {
                error!("Got a timeout waiting for a connection");
                panic!("Timed out");
            }
        };
        // Single connection only - for tests. We don't accept any more
        // connections.
        let received_messages = connection(&stream).await?;
        info!("Leaving server...");
        Ok(received_messages)
    }

    async fn client(addr: String) -> io::Result<()> {
        // Small pause just to make sure the server is up.
        task::sleep(Duration::from_millis(50)).await;
        let stream = match future::timeout(
            Duration::from_secs(1),
            TcpStream::connect(addr),
        )
        .await
        {
            Ok(s) => s?,
            Err(_) => panic!("Timed out"),
        };

        let msg = String::from("aiomsg-heartbeat");
        let msg_bytes = msg.as_bytes();
        let size_bytes = (msg_bytes.len() as i32).to_be_bytes();
        assert_eq!(size_bytes, [0x00, 0x00, 0x00, 0x10]);

        (&stream).write_all(&size_bytes).await?;
        (&stream).write_all(msg_bytes).await?;
        drop(stream); // TODO very likely unnecessary
        info!("Leaving client...");
        Ok(())
    }

    async fn string_client(
        addr: String,
        msgs: Arc<Vec<String>>,
    ) -> io::Result<()> {
        let stream = test_utils::give_client(addr, None).await?;
        for msg in &msgs[..] {
            send_string(&stream, &msg).await?;
        }
        info!("Leaving client...");
        Ok(())
    }

    #[async_std::test]
    async fn test_read_msg() {
        let addr = test_utils::get_addr();
        let server_task = task::spawn(server(addr.clone()));
        let client_task = task::spawn(client(addr.clone()));
        let result = server_task.await.unwrap(); // actual test
        assert_eq!(&result[0], "aiomsg-heartbeat");
        client_task.await.unwrap(); // cleanup
    }

    #[async_std::test]
    async fn test_read_msg2() {
        let addr = test_utils::get_addr();
        let server_task = task::spawn(server(addr.clone()));
        task::spawn(client(addr.clone()));
        let result = server_task.await.unwrap(); // actual test
        assert_eq!(&result[0], "aiomsg-heartbeat");
    }

    #[async_std::test]
    async fn test_send_msg() {
        let msgs = vec!["hello there".to_string()];
        let a = Arc::new(msgs);

        let addr = test_utils::get_addr();
        let server_task = task::spawn(string_server(addr.clone()));
        task::spawn(string_client(addr.clone(), a.clone()));
        let received_strings = server_task.await.unwrap(); // actual test
        assert_eq!(received_strings, vec!["hello there"]);
        warn!("Leaving test_send_msg");
    }

    #[async_std::test]
    async fn test_send_multiple_messages() {
        let m = vec!["msg1", "msg2", "msg3"];
        let msgs = m.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        let a = Arc::new(msgs);

        let addr = test_utils::get_addr();
        let server_task = task::spawn(string_server(addr.clone()));
        task::spawn(string_client(addr.clone(), a.clone()));
        let received_strings = server_task.await.unwrap(); // actual test
        assert_eq!(received_strings, &a[..]);
        warn!("leaving multiple messages test");
    }

    #[async_std::test]
    async fn test_send_big_message() {
        let big_string = "abc".repeat(1_000_000); // 3 MB
        let a = Arc::new(vec![big_string]);

        let addr = test_utils::get_addr();
        let server_task = task::spawn(string_server(addr.clone()));
        task::spawn(string_client(addr.clone(), a.clone()));
        let received_strings = server_task.await.unwrap(); // actual test

        let expected = &a[..];
        assert_eq!(received_strings, expected);
    }
}
