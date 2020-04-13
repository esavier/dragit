use std::error::Error;
use std::fs::{metadata, File};
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::Instant;
use std::{io, iter, pin::Pin};

use async_std::fs::File as AsyncFile;
use async_std::io as asyncio;
use futures::channel::mpsc::Sender;
use futures::prelude::*;
use libp2p::core::{InboundUpgrade, OutboundUpgrade, PeerId, UpgradeInfo};

use super::peer::PeerEvent;
use super::util::{add_row, check_size, get_target_path, hash_contents};

const CHUNK_SIZE: usize = 4096;

#[derive(Clone, Debug)]
pub struct FileToSend {
    pub name: String,
    pub path: String,
    pub peer: PeerId,
}

impl FileToSend {
    pub fn new(path: &str, peer: &PeerId) -> Result<Self, Box<dyn Error>> {
        metadata(path)?;
        let name = Self::extract_name(path)?;
        Ok(FileToSend {
            name,
            path: path.to_string(),
            peer: peer.to_owned(),
        })
    }

    fn extract_name(path: &str) -> Result<String, Box<dyn Error>> {
        let path = Path::new(path).canonicalize()?;
        let name = path
            .file_name()
            .expect("There is no file name")
            .to_str()
            .expect("Expected a name")
            .to_string();
        Ok(name)
    }
}

#[derive(Clone, Debug)]
pub enum ProtocolEvent {
    Received(TransferPayload),
    Sent,
}

#[derive(Clone, Debug, Default)]
pub struct TransferOut {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct TransferPayload {
    pub name: String,
    pub path: String,
    pub hash: String,
    pub size_bytes: usize,
    pub sender_queue: Sender<PeerEvent>,
}

impl TransferPayload {
    pub fn check_file(&self) -> Result<(), io::Error> {
        let mut contents = vec![];
        let mut file = BufReader::new(File::open(&self.path)?);
        file.read_to_end(&mut contents).expect("Cannot read file");
        let hash_from_disk = hash_contents(&mut contents);

        if hash_from_disk != self.hash {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "File corrupted!",
            ))
        } else {
            Ok(())
        }
    }

    fn notify_progress(&self, counter: usize, total_size: usize) {
        let event = PeerEvent::TransferProgress((counter, total_size));
        if let Err(e) = self.sender_queue.to_owned().try_send(event) {
            eprintln!("{:?}", e);
        };
    }

    async fn read_socket(
        &self,
        socket: impl AsyncRead + AsyncWrite + Send + Unpin,
    ) -> Result<TransferPayload, io::Error> {
        let mut reader = asyncio::BufReader::new(socket);
        let mut payloads: Vec<u8> = vec![];

        let mut name: String = "".into();
        let mut hash: String = "".into();
        let mut size_b: String = "".into();
        reader.read_line(&mut name).await?;
        reader.read_line(&mut hash).await?;
        reader.read_line(&mut size_b).await?;

        let (name, hash, size) = (
            name.trim(),
            hash.trim(),
            size_b.trim().parse::<usize>().unwrap(),
        );
        println!("Name: {}, Hash: {}, Size: {}", name, hash, size);

        let path = get_target_path(&name)?;
        let mut file = asyncio::BufWriter::new(AsyncFile::create(&path).await?);

        let mut counter: usize = 0;
        let mut res: usize = 0;
        loop {
            let mut buff = vec![0u8; CHUNK_SIZE];
            match reader.read(&mut buff).await {
                Ok(n) => {
                    if n > 0 {
                        payloads.extend(&buff[..n]);
                        counter += n;
                        res += n;

                        if payloads.len() >= (CHUNK_SIZE * 256) {
                            file.write_all(&payloads).await?;
                            file.flush().await?;
                            payloads.clear();

                            if res >= (CHUNK_SIZE * 256 * 50) {
                                self.notify_progress(counter, size);
                                res = 0;
                            }
                        }
                    } else {
                        file.write_all(&payloads).await?;
                        file.flush().await?;
                        payloads.clear();
                        self.notify_progress(counter, size);
                        break;
                    }
                }
                Err(e) => panic!("Failed reading the socket {:?}", e),
            }
        }

        let event = TransferPayload {
            name: name.to_string(),
            path: path.to_string(),
            hash: hash.to_string(),
            size_bytes: counter,
            sender_queue: self.sender_queue.clone(),
        };

        println!("Name: {}, Read {:?} bytes", name, counter);
        Ok(event)
    }
}

impl UpgradeInfo for TransferPayload {
    type Info = &'static str;
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once("/transfer/1.0")
    }
}

impl UpgradeInfo for TransferOut {
    type Info = &'static str;
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once("/transfer/1.0")
    }
}

impl<TSocket> InboundUpgrade<TSocket> for TransferPayload
where
    TSocket: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Output = TransferPayload;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_inbound(self, socket: TSocket, _: Self::Info) -> Self::Future {
        Box::pin(async move {
            println!("Upgrade inbound");
            let start = Instant::now();
            let event = self.read_socket(socket).await?;

            println!("Finished {:?} ms", start.elapsed().as_millis());
            Ok(event)
        })
    }
}

impl<TSocket> OutboundUpgrade<TSocket> for TransferOut
where
    TSocket: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Output = ();
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_outbound(self, mut socket: TSocket, _: Self::Info) -> Self::Future {
        Box::pin(async move {
            println!("Upgrade outbound");
            let start = Instant::now();
            let path = self.path.clone();

            println!("Name: {:?}, Path: {:?}", self.name, self.path);

            let file = AsyncFile::open(self.path).await.expect("File missing");
            let mut buff = asyncio::BufReader::new(&file);
            let mut contents = vec![];
            buff.read_to_end(&mut contents)
                .await
                .expect("Cannot read file");

            let hash = hash_contents(&contents);
            let name = add_row(&self.name);
            let size = check_size(&path)?;
            let size_b = add_row(&size);
            let checksum = add_row(&hash);

            socket.write(&name).await?;
            socket.write(&checksum).await?;
            socket.write(&size_b).await?;
            socket.write_all(&contents).await.expect("Writing failed");
            socket.close().await.expect("Failed to close socket");

            println!("Finished {:?} ms", start.elapsed().as_millis());
            Ok(())
        })
    }
}

impl From<()> for ProtocolEvent {
    fn from(_: ()) -> Self {
        ProtocolEvent::Sent
    }
}

impl From<TransferPayload> for ProtocolEvent {
    fn from(transfer: TransferPayload) -> Self {
        ProtocolEvent::Received(transfer)
    }
}
