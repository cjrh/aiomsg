use crate::aiomsg_types::{DeliveryGuarantee, Identity, Payload, SendMode};
use crate::msgproto;
use crate::utils::{hexify, stringify};
use async_std::net::TcpStream;
use async_std::{prelude::*, task};
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::sink::SinkExt;
use futures::StreamExt;
use log::{debug, error, info, trace, warn};
use std::collections::hash_map::{Entry, HashMap};
use std::sync::Arc;

type Sender<T> = mpsc::UnboundedSender<T>;
type Receiver<T> = mpsc::UnboundedReceiver<T>;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug)]
pub enum Event {
    NewPeer {
        name: Identity,
        stream: Arc<TcpStream>,
    },
    RemovePeer {
        name: Identity,
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
    send_into_broker: Arc<Mutex<Sender<Event>>>,
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
                        for (_id, mut sender) in &peers {
                            sender.send(msg.clone()).await?;
                        }
                    }
                    SendMode::RoundRobin => {
                        info!("roundrobin {:?}", &peers);
                        // TODO: fix this to actually be roundrobin
                        for (id, mut sender) in &peers {
                            info!("roundrobin send to peer {}", hexify(id, 4));
                            match sender.send(msg.clone()).await {
                                Ok(_) => trace!("Sent message into queue"),
                                Err(e) => {
                                    error!("Some kind of problem: {}", &e);
                                    continue;
                                }
                            };
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
                        name,
                        send_into_broker.clone(),
                        stream,
                    ));
                }
            },
            Event::RemovePeer { name } => match peers.entry(name) {
                Entry::Occupied(entry) => {
                    entry.remove_entry();
                }
                Entry::Vacant(..) => trace!("Tried to clear a conn but it was already gone."),
            },
        }
    }
    Ok(())
}

async fn connection_writer_loop(
    mut messages: Receiver<Payload>,
    id: Identity,
    send_into_broker: Arc<Mutex<Sender<Event>>>,
    stream: Arc<TcpStream>, // 3
) -> Result<()> {
    let stream = &*stream;
    while let Some(msg) = messages.next().await {
        info!("Sending msg to peer: {}", stringify(&msg, 20));
        match msgproto::send_msg(stream, &msg).await {
            Ok(_) => trace!("Message sent"),
            Err(e) => {
                info!("Got error: {}", &e);
                // This *should* make the "read" end also disconnect, which will initiate
                // reconnection. Check the code in the connector loop.
                match stream.shutdown(std::net::Shutdown::Read) {
                    Ok(()) => {
                        trace!("Shutdown successful");
                    }
                    Err(e) => {
                        error!(
                            "Problem trying to shut down both sides of a connection: {}",
                            &e
                        );
                        // return Err(Box::new(e));
                    }
                };
                let mut broker = send_into_broker.lock().await;
                broker.send(Event::RemovePeer { name: id }).await?;
                return Ok(());
            }
        };
    }
    Ok(())
}

fn spawn_and_log_error<F>(fut: F) -> task::JoinHandle<()>
where
    F: Future<Output = Result<()>> + Send + 'static,
{
    task::spawn(async move {
        if let Err(e) = fut.await {
            eprintln!("in spawn and log error {}", e)
        }
    })
}
