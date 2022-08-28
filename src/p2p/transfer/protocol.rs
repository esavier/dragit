use std::fmt;
use async_std::fs::File;
use std::fs::remove_file;
use std::io::ErrorKind;
use std::sync::mpsc::sync_channel;
use std::sync::Arc;

use std::task::{Context, Poll};
use std::time::Instant;
use std::{io, iter, pin::Pin};

use async_std::channel::{Receiver, Sender};
use async_std::io::BufReader;
use async_std::sync::Mutex;
use async_std::task;

use futures::future;
use futures::io as futio;
use futures::prelude::*;
use libp2p::core::{InboundUpgrade, OutboundUpgrade, UpgradeInfo};

use crate::p2p::commands::TransferCommand;
use crate::p2p::peer::{Direction, PeerEvent};
use crate::p2p::transfer::file::{get_hash_from_payload, FileToSend, Payload, StreamOption};
use crate::p2p::transfer::jobs;
use crate::p2p::transfer::metadata::{Answer, Metadata};
use crate::p2p::util::{self, TSocketAlias, CHUNK_SIZE};
use crate::user_data;

#[derive(Clone, Debug)]
pub enum ProtocolEvent {
    Received(TransferPayload),
    Sent,
}

// Outgoing transfer to remote peer
#[derive(Clone, Debug)]
pub struct TransferOut {
    pub file: FileToSend,
    pub sender_queue: Sender<PeerEvent>,
}

// Incoming transfer to current host
#[derive(Clone, Debug)]
pub struct TransferPayload {
    pub name: String,
    pub payload: Payload,
    pub hash: String,
    pub size_bytes: usize,
    pub sender_queue: Sender<PeerEvent>,
    pub receiver: Arc<Mutex<Receiver<TransferCommand>>>,
    pub target_path: Option<String>,
}

impl TransferPayload {
    pub fn check_file(&self) -> Result<(), io::Error> {
        let (hash_from_disk, _) =
            task::block_on(async { get_hash_from_payload(&self.payload).await })?;

        if hash_from_disk != self.hash {
            Err(io::Error::new(ErrorKind::InvalidData, "File corrupted!"))
        } else {
            Ok(())
        }
    }

    pub fn cleanup(&self) -> Result<(), io::Error> {
        if let Payload::Text(_) = self.payload {
            return match &self.target_path {
                Some(target_path) => Ok(remove_file(target_path)?),
                None => {
                    warn!("Cannot remove payload, because it has no path yet.");
                    Ok(())
                }
            };
        }
        Ok(())
    }

    async fn notify_incoming_file_event(&self, meta: &Metadata) {
        let name = meta.name.to_string();
        let hash = meta.hash.to_string();
        let size = meta.size;
        let transfer_type = meta.transfer_type;
        let event = PeerEvent::FileIncoming(name, hash, size, transfer_type);
        util::notify(&self.sender_queue, event).await;
    }

    async fn block_for_answer(
        &self,
        receiver: Arc<Mutex<Receiver<TransferCommand>>>,
    ) -> TransferCommand {
        let mut r = receiver.lock().await;
        // Wait for the user to confirm the incoming file
        task::block_on(future::poll_fn(
            move |context: &mut Context| match Receiver::poll_next_unpin(&mut r, context) {
                Poll::Ready(Some(choice)) => {
                    info!("Got the choice: {:?}", choice);
                    Poll::Ready(choice)
                }
                Poll::Ready(None) => {
                    info!("Nothing to handle now");
                    Poll::Pending
                }
                Poll::Pending => Poll::Pending,
            },
        ))
    }

    async fn read_file_payload(
        &mut self,
        socket: impl TSocketAlias,
        meta: &Metadata,
        size: usize,
        direction: &Direction,
    ) -> Result<(usize, String), io::Error> {
        let mut reader = BufReader::new(socket);

        let path =
            user_data::get_target_path(&meta.get_safe_file_name(), self.target_path.as_ref())?;

        // TODO: reader is already AsyncRead. Use Compat for directories

        let mut counter: usize = 0;
        let mut current_size: usize = 0;
        let mut file = File::create(&path).await?;
        loop {
            let mut buff = vec![0u8; CHUNK_SIZE];
            match reader.read(&mut buff).await {
                Ok(n) => {
                    if n > 0 {
                        counter += n;
                        current_size += n;

                        file.write(&buff[..n]).await?;

                        if util::time_to_notify(current_size, size) {
                            util::notify_progress(
                                &self.sender_queue,
                                counter,
                                size,
                                &direction,
                            )
                            .await;
                            current_size = 0;
                        }
                        // }
                    } else {
                        file.close().await?;
                        util::notify_progress(&self.sender_queue, counter, size, &direction).await;
                        break;
                    }
                }
                Err(e) => return Err(e),
            }
        }

        Ok((counter, path))
    }

    async fn read_socket(&mut self, socket: impl TSocketAlias) -> Result<(), io::Error> {
        let direction = Direction::Incoming;
        let (meta, mut socket) = Metadata::read(socket).await?;
        info!("Meta received! \n{}", meta);

        self.notify_incoming_file_event(&meta).await;
        let rec_cp = Arc::clone(&self.receiver);

        match self.block_for_answer(rec_cp).await {
            TransferCommand::Accept(hash) if hash == meta.hash => {
                Answer::write(&mut socket, true, hash).await?;

                util::notify_progress(&self.sender_queue, 0, meta.size, &direction).await;

                let (counter, path) = match self
                    .read_file_payload(socket, &meta, meta.size, &direction)
                    .await
                {
                    Ok((counter, path)) => (counter, path),
                    Err(err) => {
                        error!("Reading payload failed: {:?}", err);
                        util::notify_error(&self.sender_queue, "Reading payload failed").await;
                        return Err(err);
                    }
                };

                self.name = meta.name;
                self.hash = meta.hash;
                self.payload = Payload::new(meta.transfer_type, path.clone())?;
                self.size_bytes = counter;

                // TransferPayload needs to know where is the actual file after successful transfer.
                self.target_path = Some(path);

                Ok(())
            }
            TransferCommand::Accept(hash) => {
                warn!("Accepted hash does not match: {} {}", hash, meta.hash);
                Answer::write(&mut socket, false, hash).await?;
                Err(io::Error::new(
                    ErrorKind::PermissionDenied,
                    "Hash does not match",
                ))
            }
            TransferCommand::Deny(hash) => {
                warn!("Denied hash: {}", hash);
                Answer::write(&mut socket, false, hash).await?;
                Err(io::Error::new(ErrorKind::PermissionDenied, "Rejected"))
            }
        }
    }
}

impl UpgradeInfo for TransferPayload {
    type Info = &'static str;
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once("/transfer/1.1")
    }
}

impl TransferOut {
    async fn write_socket(&self, socket: impl TSocketAlias) -> Result<(), io::Error> {
        let direction = Direction::Outgoing;
        info!("File to send: {}", self.file);

        util::notify_waiting(&self.sender_queue).await;

        let (size, socket) = Metadata::write(&self.file, socket).await?;

        // Check if remote is willing to accept our file
        let (accepted, socket) = Answer::read(socket).await?;
        info!("File accepted? {:?}", accepted);

        if accepted {
            match self.file.get_file_stream().await? {
                StreamOption::File(file) => {
                    self.stream_data(socket, file, size, direction).await?;
                    Ok(())
                }
                StreamOption::Zip(file, task_handle) => {
                    self.stream_data(socket, file, size, direction).await?;
                    if let Some(handle) = task_handle {
                        handle.await?;
                    }
                    Ok(())
                }
            }
        } else {
            util::notify_rejected(&self.sender_queue).await;
            Ok(())
        }
    }

    async fn stream_data(
        &self,
        socket: impl TSocketAlias,
        mut file: impl AsyncRead + Unpin,
        size: usize,
        direction: Direction,
    ) -> Result<(), io::Error> {
        let mut writer = futio::BufWriter::new(socket);
        util::notify_progress(&self.sender_queue, 0, size, &direction).await;
        let mut counter: usize = 0;
        let mut current_size: usize = 0;
        loop {
            let mut buff = vec![0u8; 1024];
            match file.read(&mut buff).await {
                Ok(n) if n > 0 => {
                    writer.write_all(&buff[..n]).await?;
                    counter += buff.len();
                    current_size += buff.len();

                    if util::time_to_notify(current_size, size) {
                        util::notify_progress(&self.sender_queue, counter, size, &direction).await;
                        current_size = 0;
                    }
                }
                Ok(_) => {
                    writer.close().await?;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        util::notify_completed(&self.sender_queue).await;
        Ok(())
    }
}

impl UpgradeInfo for TransferOut {
    type Info = &'static str;
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once("/transfer/1.1")
    }
}

impl<TSocket> InboundUpgrade<TSocket> for TransferPayload
where
    TSocket: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Output = TransferPayload;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_inbound(mut self, socket: TSocket, _: Self::Info) -> Self::Future {
        Box::pin(async move {
            info!("Upgrade inbound");
            let start = Instant::now();
            self.read_socket(socket).await?;

            info!("Finished {:?} ms", start.elapsed().as_millis());
            Ok(self)
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

    fn upgrade_outbound(self, socket: TSocket, _: Self::Info) -> Self::Future {
        Box::pin(async move {
            info!("Upgrade outbound");
            let start = Instant::now();

            self.write_socket(socket).await?;

            info!("Finished {:?} ms", start.elapsed().as_millis());
            Ok(())
        })
    }
}

impl fmt::Display for TransferOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[TransferOut] {}", self.file)
    }
}

impl fmt::Display for TransferPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TransferPayload name: {}, payload: {}, hash: {}, size: {} bytes",
            self.name, self.payload, self.hash, self.size_bytes
        )
    }
}

impl fmt::Display for ProtocolEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolEvent::Received(e) => write!(f, "Received {}", e),
            ProtocolEvent::Sent => write!(f, "Sent"),
        }
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
