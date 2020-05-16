use std::sync::Arc;

use async_std::sync::Mutex;
use async_std::task;
use futures::{
    channel::mpsc::{Receiver, Sender},
    executor, future, pin_mut,
    stream::StreamExt,
};
use libp2p::{
    core::muxing,
    core::transport::timeout::TransportTimeout,
    core::transport::Transport,
    core::upgrade,
    dns, identity,
    mdns::{Mdns, MdnsEvent},
    mplex, secio,
    swarm::NetworkBehaviourEventProcess,
    tcp, websocket, NetworkBehaviour, PeerId, Swarm,
};

use std::{
    error::Error,
    task::{Context, Poll},
    time::Duration,
};

pub mod behaviour;
pub mod commands;
pub mod handler;
pub mod peer;
pub mod protocol;
pub mod util;

use behaviour::TransferBehaviour;
use protocol::{TransferOut, TransferPayload};

pub use commands::TransferCommand;
pub use peer::{CurrentPeers, Peer, PeerEvent};
pub use protocol::FileToSend;

#[derive(NetworkBehaviour)]
pub struct MyBehaviour {
    pub mdns: Mdns,
    pub transfer_behaviour: TransferBehaviour,
}

impl NetworkBehaviourEventProcess<MdnsEvent> for MyBehaviour {
    fn inject_event(&mut self, event: MdnsEvent) {
        match event {
            MdnsEvent::Discovered(list) => {
                for (peer_id, addr) in list {
                    match self.transfer_behaviour.add_peer(peer_id, addr) {
                        Ok(_) => (),
                        Err(e) => eprintln!("{:?}", e),
                    };
                }
            }
            MdnsEvent::Expired(list) => {
                for (peer_id, _addr) in list {
                    println!("Expired: {:?}", peer_id);
                    match self.transfer_behaviour.remove_peer(&peer_id) {
                        Ok(_) => (),
                        Err(e) => eprintln!("{:?}", e),
                    }
                }
            }
        }
    }
}

impl NetworkBehaviourEventProcess<TransferPayload> for MyBehaviour {
    fn inject_event(&mut self, mut event: TransferPayload) {
        println!("Injected {}", event);
        match event.check_file() {
            Ok(_) => {
                println!("File correct");
                if let Err(e) = event
                    .sender_queue
                    .try_send(PeerEvent::FileCorrect(event.name))
                {
                    eprintln!("{:?}", e);
                }
            }
            Err(e) => {
                println!("Not correct: {:?}", e);
                if let Err(e) = event.sender_queue.try_send(PeerEvent::FileIncorrect) {
                    eprintln!("{:?}", e);
                }
            }
        }
    }
}

impl NetworkBehaviourEventProcess<TransferOut> for MyBehaviour {
    fn inject_event(&mut self, event: TransferOut) {
        println!("TransferOut event: {:?}", event);
    }
}

async fn execute_swarm(
    sender: Sender<PeerEvent>,
    receiver: Receiver<FileToSend>,
    command_receiver: Receiver<TransferCommand>,
) {
    let local_keys = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_keys.public());
    println!("\nI am Peer: {:?} \n\n", local_peer_id);

    let command_rec = Arc::new(Mutex::new(command_receiver));
    let command_receiver_c = Arc::clone(&command_rec);

    let mut swarm = {
        let mdns = Mdns::new().unwrap();
        let transfer_behaviour = TransferBehaviour::new(sender, command_receiver_c);
        let behaviour = MyBehaviour {
            mdns,
            transfer_behaviour,
        };
        let timeout = Duration::from_secs(60);

        let transport = {
            let tcp = tcp::TcpConfig::new().nodelay(true);
            let transport = dns::DnsConfig::new(tcp).unwrap();
            let trans_clone = transport.clone();
            transport.or_transport(websocket::WsConfig::new(trans_clone))
        };
        let mut mplex_config = mplex::MplexConfig::new();

        // TODO: test different Mplex frame sizes
        let mp = mplex_config
            .max_buffer_len(40960)
            .split_send_size(1024 * 512);

        let transport = TransportTimeout::with_outgoing_timeout(
            transport
                .upgrade(upgrade::Version::V1)
                .authenticate(secio::SecioConfig::new(local_keys.clone()))
                .multiplex(mp.clone())
                .map(|(peer, muxer), _| (peer, muxing::StreamMuxerBox::new(muxer)))
                .timeout(timeout),
            timeout,
        );

        Swarm::new(transport, behaviour, local_peer_id)
    };

    Swarm::listen_on(
        &mut swarm,
        "/ip4/0.0.0.0/tcp/0"
            .parse()
            .expect("Failed to parse address"),
    )
    .expect("Failed to listen");

    let mut listening = false;

    pin_mut!(receiver);
    task::block_on(future::poll_fn(move |context: &mut Context| {
        loop {
            match Receiver::poll_next_unpin(&mut receiver, context) {
                Poll::Ready(Some(event)) => {
                    match swarm.transfer_behaviour.push_file(event) {
                        Ok(_) => {}
                        Err(e) => eprintln!("{:?}", e),
                    };
                }
                Poll::Ready(None) => println!("nothing in queue"),
                Poll::Pending => break,
            };
        }

        loop {
            match swarm.poll_next_unpin(context) {
                Poll::Ready(Some(event)) => println!("Some event main: {:?}", event),
                Poll::Ready(None) => {
                    return {
                        println!("Ready");
                        Poll::Ready("aaa")
                    }
                }
                Poll::Pending => {
                    if !listening {
                        for addr in Swarm::listeners(&swarm) {
                            println!("Listening on {:?}", addr);
                            listening = true;
                        }
                    }

                    break;
                }
            }
        }
        Poll::Pending
    }));
}

pub fn run_server(
    sender: Sender<PeerEvent>,
    file_receiver: Receiver<FileToSend>,
    command_receiver: Receiver<TransferCommand>,
) -> Result<(), Box<dyn Error>> {
    let future = execute_swarm(sender, file_receiver, command_receiver);
    executor::block_on(future);
    Ok(())
}
