use crate::aiomsg_types::{DeliveryGuarantee, Identity, Payload, SendMode};
use crate::msgproto;
use crate::utils::{hexify, hexify_vec, stringify};
use async_std::net::TcpStream;
use async_std::{prelude::*, task};
use futures::channel::mpsc;
use futures::sink::SinkExt;
use futures::StreamExt;
use log::{debug, error, info, trace, warn};
use std::collections::hash_map::{Entry, HashMap};
use std::sync::Arc;

type Sender<T> = mpsc::UnboundedSender<T>;
type Receiver<T> = mpsc::UnboundedReceiver<T>;
type Result<T> =
    std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug)]
pub enum Event {
    NewPeer {
        name: Identity,
        stream: Arc<TcpStream>,
    },
    Message {
        from: Identity,
        // Multiple identities
        to: Vec<Identity>,
        msg: Payload,
    },
}

pub async fn broker_loop(
    mut events: Receiver<Event>,
    send_mode: SendMode,
) -> Result<()> {
    let mut peers: HashMap<Identity, Sender<Payload>> = HashMap::new();

    while let Some(event) = events.next().await {
        trace!("Received event: {:?}", &event);
        match event {
            Event::Message { from, to, msg } => {
                info!(
                    "New msg from {} to {:?}: {:?}",
                    hexify(&from, 4),
                    &to,
                    stringify(&msg, 20),
                );
                match send_mode {
                    SendMode::Publish => {
                        info!("publish to all peers");
                        for (id, mut sender) in &peers {
                            sender.send(msg.clone()).await?;
                        }
                    }
                    SendMode::RoundRobin => {
                        info!("roundrobin {:?}", &peers);
                        // TODO: fix this to actually be roundrobin
                        for (id, mut sender) in &peers {
                            info!("roundrobin send to peer {}", hexify(id, 4));
                            sender.send(msg.clone()).await?;
                        }
                    }
                }
                // for addr in to {
                //     if let Some(peer) = peers.get_mut(&addr) {
                //         // TODO: this could be expensive, copying all the data
                //         info!(
                //             "Sending to peer {}, msg {}",
                //             hexify(&addr, 4),
                //             stringify(&msg, 20)
                //         );
                //         peer.send(msg.clone()).await?
                //     } else {
                //         warn!(
                //             "No peer {} found in the map {:?}, dropping msg",
                //             hexify(&addr, 4),
                //             &peers
                //         );
                //     }
                // }
            }
            Event::NewPeer { name, stream } => match peers.entry(name) {
                Entry::Occupied(..) => (),
                Entry::Vacant(entry) => {
                    info!("Adding new peer {} to map", hexify(&name, 4));
                    let (client_sender, client_receiver) = mpsc::unbounded();
                    entry.insert(client_sender);
                    spawn_and_log_error(connection_writer_loop(
                        client_receiver,
                        stream,
                    ));
                }
            },
        }
    }
    Ok(())
}

async fn connection_writer_loop(
    mut messages: Receiver<Payload>,
    stream: Arc<TcpStream>, // 3
) -> Result<()> {
    let mut stream = &*stream;
    while let Some(msg) = messages.next().await {
        info!("Sending msg to peer: {}", stringify(&msg, 20));
        msgproto::send_msg(stream, &msg).await?;
        // stream.write_all(&msg).await?;
    }
    Ok(())
}

fn spawn_and_log_error<F>(fut: F) -> task::JoinHandle<()>
where
    F: Future<Output = Result<()>> + Send + 'static,
{
    task::spawn(async move {
        if let Err(e) = fut.await {
            eprintln!("{}", e)
        }
    })
}
