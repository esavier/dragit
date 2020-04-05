use std::collections::HashSet;
use std::error::Error;

use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;

use libp2p::core::{connection::ConnectionId, Multiaddr, PeerId};
use libp2p::swarm::{
    DialPeerCondition, NetworkBehaviour, NetworkBehaviourAction, NotifyHandler, OneShotHandler,
    OneShotHandlerConfig, PollParameters, ProtocolsHandler, SubstreamProtocol,
};

use crate::p2p::protocol::{FileToSend, ProtocolEvent, TransferOut, TransferPayload};

pub struct TransferBehaviour {
    pub peers: HashSet<PeerId>,
    pub connected_peers: HashSet<PeerId>,
    pub events: Vec<NetworkBehaviourAction<TransferPayload, TransferOut>>,
    payloads: Vec<FileToSend>,
}

impl TransferBehaviour {
    pub fn new() -> Self {
        TransferBehaviour {
            peers: HashSet::new(),
            connected_peers: HashSet::new(),
            events: vec![],
            payloads: vec![],
        }
    }

    pub fn push_file(&mut self, file: FileToSend) -> Result<(), Box<dyn Error>> {
        Ok(self.payloads.push(file))
    }
}

impl NetworkBehaviour for TransferBehaviour {
    type ProtocolsHandler = OneShotHandler<TransferPayload, TransferOut, ProtocolEvent>;
    type OutEvent = TransferPayload;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        let timeout = Duration::from_secs(120);
        let tp = TransferPayload::default();
        let handler_config = OneShotHandlerConfig {
            inactive_timeout: timeout,
            substream_timeout: timeout,
        };
        let proto = SubstreamProtocol::new(tp).with_timeout(timeout);
        Self::ProtocolsHandler::new(proto, handler_config)
    }

    fn addresses_of_peer(&mut self, _peer_id: &PeerId) -> Vec<Multiaddr> {
        Vec::new()
    }

    fn inject_connected(&mut self, peer: &PeerId) {
        self.connected_peers.insert(peer.to_owned());
    }

    fn inject_dial_failure(&mut self, peer: &PeerId) {
        println!("Dial failure {:?}", peer);
        self.connected_peers.remove(peer);
    }

    fn inject_disconnected(&mut self, peer: &PeerId) {
        println!("Disconnected: {:?}", peer);
        self.connected_peers.remove(peer);
        self.peers.remove(peer);
    }

    fn inject_event(&mut self, peer: PeerId, c: ConnectionId, event: ProtocolEvent) {
        println!("Inject event: {:?}", event);
        match event {
            ProtocolEvent::Received(data) => {
                self.events.push(NetworkBehaviourAction::NotifyHandler {
                    handler: NotifyHandler::One(c),
                    peer_id: peer,
                    event: data,
                });
            }
            ProtocolEvent::Sent => return,
        };
    }

    fn poll(
        &mut self,
        _: &mut Context,
        _: &mut impl PollParameters,
    ) -> Poll<
        NetworkBehaviourAction<
            <Self::ProtocolsHandler as ProtocolsHandler>::InEvent,
            Self::OutEvent,
        >,
    > {
        if let Some(event) = self.events.pop() {
            println!("Got event from the queue: {:?}", event);
            match event {
                NetworkBehaviourAction::NotifyHandler {
                    peer_id: _,
                    handler: _,
                    event: send_event,
                } => {
                    let tp = TransferPayload {
                        name: send_event.name,
                        path: send_event.path,
                        hash: send_event.hash,
                        size_bytes: send_event.size_bytes,
                    };
                    return Poll::Ready(NetworkBehaviourAction::GenerateEvent(tp));
                }
                NetworkBehaviourAction::GenerateEvent(e) => {
                    println!("GenerateEvent event {:?}", e);
                }
                _ => {
                    println!("Another event");
                }
            }
        };

        if self.connected_peers.len() > 0 {
            let peer = self.connected_peers.iter().nth(0).unwrap();
            match self.payloads.pop() {
                Some(message) => {
                    let event = TransferOut {
                        name: message.name,
                        path: message.path,
                    };
                    return Poll::Ready(NetworkBehaviourAction::NotifyHandler {
                        handler: NotifyHandler::Any,
                        peer_id: peer.to_owned(),
                        event,
                    });
                }
                None => return Poll::Pending,
            }
        } else {
            for peer in self.peers.iter() {
                if !self.connected_peers.contains(peer) {
                    println!("Will try to dial: {:?}", peer);
                    let millis = Duration::from_millis(100);
                    thread::sleep(millis);
                    return Poll::Ready(NetworkBehaviourAction::DialPeer {
                        condition: DialPeerCondition::NotDialing,
                        peer_id: peer.to_owned(),
                    });
                }
            }
        }
        Poll::Pending
    }
}
