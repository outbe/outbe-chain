//! Trusted local receipt delivery. No secret material or signing messages cross this socket.

use crate::{
    endpoint::ChainIdentity,
    manager::{FinalizedBlock, FinalizedPublicationReceipt, RepositoryReceiptReader},
};
use alloy_primitives::B256;
use outbe_primitives::radicle_publication::{PublicationBody, PUBLICATION_BODY_LEN};
use outbe_radicleregistry::publication::PublicationReceipt;
use std::{
    fs::{self, File, OpenOptions},
    future::Future,
    io,
    os::unix::fs::{FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    net::{UnixListener, UnixStream},
    sync::Semaphore,
    task::JoinSet,
};

const MAGIC: &[u8; 4] = b"ORL1";
const MAX_FRAME: usize = 5 + 8 + 32 + 8 + 32 + PUBLICATION_BODY_LEN;
const MAX_CONNECTIONS: usize = 32;
const MAX_READERS: usize = 4;
const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("local receipt socket is not private or has an unexpected owner")]
    Untrusted,
    #[error("invalid local receipt frame")]
    Protocol,
    #[error("local finalized receipt is unavailable")]
    Unavailable,
    #[error("local receipt deadline exceeded")]
    Deadline,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Separate from the native Heartwood control socket; both processes know that path.
pub fn receipt_socket(control_socket: &Path) -> PathBuf {
    let mut path = control_socket.as_os_str().to_os_string();
    path.push(".outbe");
    path.into()
}

fn private_parent(path: &Path) -> Result<(), Error> {
    let parent = path.parent().ok_or(Error::Untrusted)?;
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(Error::Untrusted);
    }
    Ok(())
}

fn private_socket(path: &Path) -> Result<fs::Metadata, Error> {
    private_parent(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(Error::Untrusted);
    }
    Ok(metadata)
}

fn peer(stream: &UnixStream) -> Result<(), Error> {
    if stream.peer_cred()?.uid() != rustix::process::geteuid().as_raw() {
        return Err(Error::Untrusted);
    }
    Ok(())
}

struct SocketLease {
    path: PathBuf,
    device: u64,
    inode: u64,
    _lock: File,
}

impl Drop for SocketLease {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| metadata.dev() == self.device && metadata.ino() == self.inode)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub struct LocalReceiptServer {
    listener: UnixListener,
    reader: Arc<dyn RepositoryReceiptReader>,
    _lease: SocketLease,
}

impl LocalReceiptServer {
    /// Bind under the existing private native control directory, without replacing a live owner.
    pub fn bind(
        control_socket: &Path,
        reader: Arc<dyn RepositoryReceiptReader>,
    ) -> Result<Self, Error> {
        let path = receipt_socket(control_socket);
        private_parent(&path)?;
        let lock_path = path.with_file_name({
            let mut name = path.file_name().ok_or(Error::Untrusted)?.to_os_string();
            name.push(".lock");
            name
        });
        if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
            if !metadata.is_file()
                || metadata.uid() != rustix::process::geteuid().as_raw()
                || metadata.permissions().mode() & 0o777 != 0o600
            {
                return Err(Error::Untrusted);
            }
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)?;
        lock.try_lock()
            .map_err(|error| io::Error::new(io::ErrorKind::AddrInUse, error))?;
        match private_socket(&path) {
            Ok(_) => match probe(&path) {
                Ok(_) => return Err(io::Error::from(io::ErrorKind::AddrInUse).into()),
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                    fs::remove_file(&path)?;
                }
                Err(error) => return Err(error.into()),
            },
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let metadata = private_socket(&path)?;
        let lease = SocketLease {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            _lock: lock,
        };
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener: UnixListener::from_std(listener)?,
            reader,
            _lease: lease,
        })
    }

    /// Run outside consensus execution; blocking state readers retain their bounded permit.
    pub async fn run(self, stop: impl Future<Output = ()>) -> Result<(), Error> {
        tokio::pin!(stop);
        let readers = Arc::new(Semaphore::new(MAX_READERS));
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut stop => break,
                _ = clients.join_next(), if !clients.is_empty() => {},
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted?;
                    if clients.len() >= MAX_CONNECTIONS || peer(&stream).is_err() {
                        continue;
                    }
                    let reader = self.reader.clone();
                    let readers = readers.clone();
                    clients.spawn(async move {
                        let _ = tokio::time::timeout(DEADLINE, serve(stream, reader, readers)).await;
                    });
                }
            }
        }
        clients.abort_all();
        while clients.join_next().await.is_some() {}
        Ok(())
    }
}

fn probe(path: &Path) -> io::Result<()> {
    use rustix::net::{
        connect, socket_with, AddressFamily, SocketAddrUnix, SocketFlags, SocketType,
    };
    let socket = socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )?;
    connect(&socket, &SocketAddrUnix::new(path)?).map_err(Into::into)
}

#[derive(Clone)]
pub struct LocalReceiptClient {
    socket: PathBuf,
    chain: ChainIdentity,
}

impl LocalReceiptClient {
    pub fn new(control_socket: &Path, chain: ChainIdentity) -> Self {
        Self {
            socket: receipt_socket(control_socket),
            chain,
        }
    }

    pub async fn read(&self, sequence: u64) -> Result<Option<FinalizedPublicationReceipt>, Error> {
        tokio::time::timeout(DEADLINE, async {
            private_socket(&self.socket)?;
            let mut stream = UnixStream::connect(&self.socket).await?;
            peer(&stream)?;
            let request = [MAGIC.as_slice(), &sequence.to_be_bytes()].concat();
            write_frame(&mut stream, &request).await?;
            let response = read_frame(&mut stream).await?;
            decode_response(&response, sequence, self.chain)
        })
        .await
        .map_err(|_| Error::Deadline)?
    }
}

async fn serve(
    mut stream: UnixStream,
    reader: Arc<dyn RepositoryReceiptReader>,
    readers: Arc<Semaphore>,
) -> Result<(), Error> {
    let request = read_frame(&mut stream).await?;
    if request.len() != 12 || request[..4] != *MAGIC {
        return Err(Error::Protocol);
    }
    let sequence = u64::from_be_bytes(request[4..].try_into().map_err(|_| Error::Protocol)?);
    let permit = match readers.try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return write_frame(&mut stream, &[MAGIC.as_slice(), &[0]].concat()).await,
    };
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        reader.read_current_publication_receipt(sequence)
    })
    .await
    .map_err(|_| Error::Unavailable)?;
    let mut response = MAGIC.to_vec();
    match result {
        Err(_) => response.push(0),
        Ok(None) => response.push(1),
        Ok(Some(receipt)) => {
            response.push(2);
            response.extend(receipt.block().number.to_be_bytes());
            response.extend(receipt.block().hash.0);
            response.extend(receipt.receipt().accepted_height.to_be_bytes());
            response.extend(receipt.receipt().receipt_hash.0);
            response.extend(receipt.receipt().body.to_bytes());
        }
    }
    write_frame(&mut stream, &response).await
}

fn decode_response(
    response: &[u8],
    sequence: u64,
    chain: ChainIdentity,
) -> Result<Option<FinalizedPublicationReceipt>, Error> {
    if response.len() < 5 || response[..4] != *MAGIC {
        return Err(Error::Protocol);
    }
    match (response[4], response.len()) {
        (0, 5) => Err(Error::Unavailable),
        (1, 5) => Ok(None),
        (2, MAX_FRAME) => {
            let number =
                u64::from_be_bytes(response[5..13].try_into().map_err(|_| Error::Protocol)?);
            let hash = B256::from_slice(&response[13..45]);
            let accepted_height =
                u64::from_be_bytes(response[45..53].try_into().map_err(|_| Error::Protocol)?);
            let receipt_hash = B256::from_slice(&response[53..85]);
            let body = PublicationBody::from_bytes(&response[85..]).map_err(|_| Error::Protocol)?;
            if body.sequence != sequence
                || body.chain_id != chain.chain_id
                || body.genesis_hash != chain.genesis_hash
                || receipt_hash != body.receipt_hash()
                || accepted_height == 0
                || accepted_height > number
                || hash.is_zero()
            {
                return Err(Error::Protocol);
            }
            Ok(Some(FinalizedPublicationReceipt {
                block: FinalizedBlock { number, hash },
                receipt: PublicationReceipt {
                    body,
                    accepted_height,
                    receipt_hash,
                },
            }))
        }
        _ => Err(Error::Protocol),
    }
}

async fn read_frame(stream: &mut (impl AsyncRead + Unpin)) -> Result<Vec<u8>, Error> {
    let length = stream.read_u32().await? as usize;
    if length > MAX_FRAME {
        return Err(Error::Protocol);
    }
    let mut frame = vec![0; length];
    stream.read_exact(&mut frame).await?;
    Ok(frame)
}

async fn write_frame(stream: &mut (impl AsyncWrite + Unpin), frame: &[u8]) -> Result<(), Error> {
    if frame.len() > MAX_FRAME {
        return Err(Error::Protocol);
    }
    stream.write_u32(frame.len() as u32).await?;
    stream.write_all(frame).await?;
    Ok(())
}

#[cfg(test)]
mod tests;
