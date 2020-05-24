use libp2p::{Multiaddr, PeerId};

#[derive(Debug, Clone)]
pub enum PeerEvent {
    PeersUpdated(CurrentPeers),
    TransferProgress((usize, usize)),
    TransferError,
    FileCorrect(String, String),
    FileIncorrect,
    FileIncoming(String, String),
}

pub type CurrentPeers = Vec<Peer>;

#[derive(Debug, Eq, Hash, Clone)]
pub struct Peer {
    pub name: String,
    pub address: Multiaddr,
    pub peer_id: PeerId,
}

impl PartialEq for Peer {
    fn eq(&self, other: &Self) -> bool {
        self.peer_id == other.peer_id
    }
}
