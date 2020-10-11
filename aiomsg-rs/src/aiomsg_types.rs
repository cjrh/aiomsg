use futures::channel::mpsc;

pub type Payload = Vec<u8>;
// pub type Sender = mpsc::Sender<Payload>;
// pub type Receiver = mpsc::Receiver<Payload>;
pub type Identity = [u8; 16];

#[derive(Debug, Copy, Clone)]
pub enum SendMode {
    Publish,
    RoundRobin,
}

#[derive(Debug)]
pub enum ConnectionEnd {
    Binder,
    Connector,
}

#[derive(Debug)]
pub enum DeliveryGuarantee {
    AtMostOnce,
    AtLeastOnce,
}
