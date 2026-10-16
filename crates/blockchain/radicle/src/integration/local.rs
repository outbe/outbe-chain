//! Trusted local finalized context and fixed native jobs. Secret shares never cross this socket.

mod bootstrap;
mod material;
mod preparation;
mod publication;
mod state;

pub use bootstrap::BootstrapSigningContext;
pub use preparation::{publication_store_path, PatchPreparationContext};

use super::publication::{store::ReviewPackageStore, PublicationManifest};
use super::{network::EndpointNetworkResolver, signing::RepositorySigningMaterial};

use crate::{
    endpoint::ChainIdentity,
    manager::{
        FinalizedBlock, FinalizedPatchSigningState, FinalizedPublicationReceipt,
        RepositoryAuthorityReader, RepositoryReceiptReader,
    },
};
use alloy_primitives::{B256, U256};
use outbe_primitives::radicle_publication::{PublicationBody, PUBLICATION_BODY_LEN};
use outbe_radicleregistry::publication::PublicationReceipt;
use std::{
    fs::{self, File, OpenOptions},
    future::Future,
    io,
    os::unix::fs::{FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    net::{UnixListener, UnixStream},
    sync::Semaphore,
    task::JoinSet,
};

const MAGIC: &[u8; 4] = b"ORL1";
const RECEIPT_FRAME_LEN: usize = 5 + 8 + 32 + 8 + 32 + PUBLICATION_BODY_LEN;
const MAX_REQUEST: usize = publication::MAX_REQUEST;
const MAX_FRAME: usize = if preparation::MAX_FRAME > MAX_REQUEST {
    preparation::MAX_FRAME
} else {
    MAX_REQUEST
};
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
    authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    material: Option<EndpointNetworkResolver>,
    publications: Option<Arc<Mutex<ReviewPackageStore>>>,
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
            authority: None,
            material: None,
            publications: None,
            _lease: lease,
        })
    }

    pub fn with_authority_reader(mut self, reader: Arc<dyn RepositoryAuthorityReader>) -> Self {
        self.authority = Some(reader);
        self
    }

    pub fn with_material_resolver(mut self, resolver: EndpointNetworkResolver) -> Self {
        self.material = Some(resolver);
        self
    }

    pub fn with_publication_store(mut self, store: ReviewPackageStore) -> Self {
        self.publications = Some(Arc::new(Mutex::new(store)));
        self
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
                    let authority = self.authority.clone();
                    let material = self.material.clone();
                    let publications = self.publications.clone();
                    let readers = readers.clone();
                    clients.spawn(async move {
                        let _ = tokio::time::timeout(DEADLINE, serve(stream, reader, authority, material, publications, readers)).await;
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
    use rustix::net::{connect, socket, AddressFamily, SocketAddrUnix, SocketType};
    let socket = socket(AddressFamily::UNIX, SocketType::STREAM, None)?;
    let socket: std::os::unix::net::UnixStream = socket.into();
    socket.set_nonblocking(true)?;
    rustix::io::fcntl_setfd(&socket, rustix::io::FdFlags::CLOEXEC)?;
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
        let request = [MAGIC.as_slice(), &sequence.to_be_bytes()].concat();
        decode_response(
            &self.exchange(&request, RECEIPT_FRAME_LEN).await?,
            sequence,
            self.chain,
        )
    }

    /// Ask the node to verify, durably select, then attest a complete native bootstrap package.
    pub async fn attest_bootstrap_publication(
        &self,
        context: &BootstrapSigningContext,
        repo: &radicle::storage::git::Repository,
        content_manifest: radicle::git::Oid,
        package: &super::bootstrap::VerifiedBootstrapPublication,
        witness: &super::signing::witness::RepositoryWitnessBundle,
    ) -> Result<outbe_primitives::radicle_publication::PublicationAttestation, Error> {
        use outbe_primitives::radicle_publication::PublicationBatch;
        if package.body().chain_id != self.chain.chain_id
            || package.body().genesis_hash != self.chain.genesis_hash
        {
            return Err(Error::Protocol);
        }
        let request = publication::request(
            publication::ATTEST,
            repo,
            content_manifest,
            package,
            witness,
        )?;
        let response = self.exchange(&request, publication::ATTEST_FRAME).await?;
        if response == [publication::ATTEST.as_slice(), &[0]].concat() {
            return Err(Error::Unavailable);
        }
        if response.get(..5) != Some([publication::ATTEST.as_slice(), &[2]].concat().as_slice()) {
            return Err(Error::Protocol);
        }
        let mut batch =
            PublicationBatch::from_bytes(&response[5..]).map_err(|_| Error::Protocol)?;
        if batch.body != *package.body() || batch.attestations.len() != 1 {
            return Err(Error::Protocol);
        }
        let attestation = batch.attestations.pop().ok_or(Error::Protocol)?;
        context
            .material
            .verify_publication_attestation(&batch.body, &attestation)
            .map_err(|_| Error::Protocol)?;
        Ok(attestation)
    }

    pub async fn queue_bootstrap_publication(
        &self,
        repo: &radicle::storage::git::Repository,
        content_manifest: radicle::git::Oid,
        package: &super::bootstrap::VerifiedBootstrapPublication,
        witness: &super::signing::witness::RepositoryWitnessBundle,
        batch: &outbe_primitives::radicle_publication::PublicationBatch,
    ) -> Result<(), Error> {
        if batch.body != *package.body()
            || batch.body.chain_id != self.chain.chain_id
            || batch.body.genesis_hash != self.chain.genesis_hash
        {
            return Err(Error::Protocol);
        }
        let mut request =
            publication::request(publication::QUEUE, repo, content_manifest, package, witness)?;
        request.extend(batch.to_bytes().map_err(|_| Error::Protocol)?);
        let response = self.exchange(&request, 5).await?;
        if response == [publication::QUEUE.as_slice(), &[2]].concat() {
            Ok(())
        } else if response == [publication::QUEUE.as_slice(), &[0]].concat() {
            Err(Error::Unavailable)
        } else {
            Err(Error::Protocol)
        }
    }

    /// Ask the node to reconstruct and sign its own current genesis document, never caller bytes.
    pub async fn sign_bootstrap_document(
        &self,
        context: &BootstrapSigningContext,
    ) -> Result<super::signing::RepositoryPartial, Error> {
        if context.state.authority().chain_id() != self.chain.chain_id
            || context.state.authority().genesis_hash() != self.chain.genesis_hash
        {
            return Err(Error::Protocol);
        }
        let job = super::bootstrap::prepare_bootstrap_document(
            context.state.descriptor(),
            &context.material,
        )
        .map_err(|_| Error::Protocol)?;
        let request = [
            bootstrap::SIGN_DOCUMENT.as_slice(),
            job.body().receipt_hash().as_slice(),
        ]
        .concat();
        let response = self.exchange(&request, bootstrap::SIGN_FRAME).await?;
        let (partial, sender) =
            bootstrap::decode_partial(&response, bootstrap::SIGN_DOCUMENT, context)?;
        context
            .material
            .verify_bootstrap_document_partial(&job, &context.state, &sender, &partial)
            .map_err(|_| Error::Protocol)?;
        Ok(partial)
    }

    pub async fn sign_bootstrap_identity(
        &self,
        context: &BootstrapSigningContext,
        job: &super::bootstrap::PreparedBootstrapIdentity,
        repo: &radicle::storage::git::Repository,
        content_manifest: radicle::git::Oid,
    ) -> Result<super::signing::RepositoryPartial, Error> {
        if context.state.authority().chain_id() != self.chain.chain_id
            || context.state.authority().genesis_hash() != self.chain.genesis_hash
        {
            return Err(Error::Protocol);
        }
        let request = bootstrap::native_request(
            bootstrap::SIGN_IDENTITY,
            job.body(),
            repo,
            content_manifest,
            &[job.document_signature()],
        )?;
        let response = self.exchange(&request, bootstrap::SIGN_FRAME).await?;
        let (partial, sender) =
            bootstrap::decode_partial(&response, bootstrap::SIGN_IDENTITY, context)?;
        context
            .material
            .verify_bootstrap_identity_partial(job, &context.state, &sender, &partial)
            .map_err(|_| Error::Protocol)?;
        Ok(partial)
    }

    pub async fn sign_bootstrap_refs(
        &self,
        context: &BootstrapSigningContext,
        job: &super::bootstrap::PreparedBootstrapRefs,
        repo: &radicle::storage::git::Repository,
        content_manifest: radicle::git::Oid,
    ) -> Result<super::signing::RepositoryPartial, Error> {
        if context.state.authority().chain_id() != self.chain.chain_id
            || context.state.authority().genesis_hash() != self.chain.genesis_hash
        {
            return Err(Error::Protocol);
        }
        let request = bootstrap::native_request(
            bootstrap::SIGN_REFS,
            job.body(),
            repo,
            content_manifest,
            &job.signatures(),
        )?;
        let response = self.exchange(&request, bootstrap::SIGN_FRAME).await?;
        let (partial, sender) =
            bootstrap::decode_partial(&response, bootstrap::SIGN_REFS, context)?;
        context
            .material
            .verify_bootstrap_refs_partial(job, &context.state, &sender, &partial)
            .map_err(|_| Error::Protocol)?;
        Ok(partial)
    }

    /// Public genesis descriptor and current DKG Output; no local Share crosses the socket.
    pub async fn read_bootstrap_signing_context(&self) -> Result<BootstrapSigningContext, Error> {
        bootstrap::decode(
            &self
                .exchange(bootstrap::MAGIC, bootstrap::MAX_FRAME)
                .await?,
            self.chain,
        )
    }

    /// A fresh local finalized snapshot for this exact approved proposal, never a cached fallback.
    pub async fn read_patch_signing_state(
        &self,
        proposal_id: U256,
    ) -> Result<FinalizedPatchSigningState, Error> {
        let request = [state::MAGIC.as_slice(), &proposal_id.to_be_bytes::<32>()].concat();
        state::decode(
            &self.exchange(&request, state::MAX_FRAME).await?,
            proposal_id,
            self.chain,
        )
    }

    /// Public Output and patch state must describe the same current finalized authority.
    pub async fn read_patch_signing_context(
        &self,
        proposal_id: U256,
    ) -> Result<(FinalizedPatchSigningState, RepositorySigningMaterial), Error> {
        let request = [material::MAGIC.as_slice(), &proposal_id.to_be_bytes::<32>()].concat();
        material::decode(
            &self.exchange(&request, material::MAX_FRAME).await?,
            proposal_id,
            self.chain,
        )
    }

    /// Cache an exact manifest only after the node confirms its immutable accepted receipt.
    pub async fn cache_checkpoint_manifest(
        &self,
        checkpoint: &FinalizedPublicationReceipt,
        manifest: &PublicationManifest,
    ) -> Result<(), Error> {
        let receipt = checkpoint.receipt();
        if receipt.body.chain_id != self.chain.chain_id
            || receipt.body.genesis_hash != self.chain.genesis_hash
        {
            return Err(Error::Protocol);
        }
        let request = [
            preparation::CACHE.as_slice(),
            &receipt.body.sequence.to_be_bytes(),
            receipt.receipt_hash.as_slice(),
            &manifest.to_bytes().map_err(|_| Error::Protocol)?,
        ]
        .concat();
        let response = self.exchange(&request, 5).await?;
        if response == [preparation::CACHE.as_slice(), &[2]].concat() {
            Ok(())
        } else if response == [preparation::CACHE.as_slice(), &[0]].concat() {
            Err(Error::Unavailable)
        } else {
            Err(Error::Protocol)
        }
    }

    /// Preparation inputs only; subsequent signing still rechecks the current chain state.
    pub async fn read_patch_preparation_context(
        &self,
        proposal_id: U256,
    ) -> Result<PatchPreparationContext, Error> {
        let request = [
            preparation::MAGIC.as_slice(),
            &proposal_id.to_be_bytes::<32>(),
        ]
        .concat();
        preparation::decode(
            &self.exchange(&request, preparation::MAX_FRAME).await?,
            proposal_id,
            self.chain,
        )
    }

    async fn exchange(&self, request: &[u8], maximum: usize) -> Result<Vec<u8>, Error> {
        tokio::time::timeout(DEADLINE, async {
            private_socket(&self.socket)?;
            let mut stream = UnixStream::connect(&self.socket).await?;
            peer(&stream)?;
            write_frame(&mut stream, request).await?;
            read_frame(&mut stream, maximum).await
        })
        .await
        .map_err(|_| Error::Deadline)?
    }
}

async fn serve(
    mut stream: UnixStream,
    reader: Arc<dyn RepositoryReceiptReader>,
    authority: Option<Arc<dyn RepositoryAuthorityReader>>,
    material: Option<EndpointNetworkResolver>,
    publications: Option<Arc<Mutex<ReviewPackageStore>>>,
    readers: Arc<Semaphore>,
) -> Result<(), Error> {
    let request = read_frame(&mut stream, MAX_REQUEST).await?;
    let magic = match (request.get(..4), request.len()) {
        (Some(magic), 12) if magic == MAGIC => MAGIC,
        (Some(magic), length) if magic == publication::ATTEST && length > 4 => publication::ATTEST,
        (Some(magic), length) if magic == publication::QUEUE && length > 4 => publication::QUEUE,
        (Some(magic), 4) if magic == bootstrap::MAGIC => bootstrap::MAGIC,
        (Some(magic), 36) if magic == bootstrap::SIGN_DOCUMENT => bootstrap::SIGN_DOCUMENT,
        (Some(magic), length) if magic == bootstrap::SIGN_IDENTITY && length > 36 => {
            bootstrap::SIGN_IDENTITY
        }
        (Some(magic), length) if magic == bootstrap::SIGN_REFS && length > 36 => {
            bootstrap::SIGN_REFS
        }
        (Some(magic), 36) if magic == state::MAGIC => state::MAGIC,
        (Some(magic), 36) if magic == material::MAGIC => material::MAGIC,
        (Some(magic), 36) if magic == preparation::MAGIC => preparation::MAGIC,
        (Some(magic), length) if magic == preparation::CACHE && length > 44 => preparation::CACHE,
        _ => return Err(Error::Protocol),
    };
    let permit = match readers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return write_frame(&mut stream, &[magic.as_slice(), &[0]].concat()).await,
    };
    if magic == publication::ATTEST || magic == publication::QUEUE {
        let response =
            publication::publish(authority, material, publications, readers, permit, request)
                .await
                .unwrap_or_else(|_| [magic.as_slice(), &[0]].concat());
        return write_frame(&mut stream, &response).await;
    }
    if magic == bootstrap::SIGN_IDENTITY || magic == bootstrap::SIGN_REFS {
        let response = bootstrap::sign_native(authority, material, readers, permit, request)
            .await
            .unwrap_or_else(|_| [magic.as_slice(), &[0]].concat());
        return write_frame(&mut stream, &response).await;
    }
    if magic == bootstrap::SIGN_DOCUMENT {
        let response =
            bootstrap::sign_document(authority, material, permit, B256::from_slice(&request[4..]))
                .await
                .unwrap_or_else(|_| [magic.as_slice(), &[0]].concat());
        return write_frame(&mut stream, &response).await;
    }
    if magic == bootstrap::MAGIC {
        let response = bootstrap::read(authority, material, permit)
            .await
            .unwrap_or_else(|_| [magic.as_slice(), &[0]].concat());
        return write_frame(&mut stream, &response).await;
    }
    if magic == preparation::CACHE {
        let result = preparation::cache(reader, publications, permit, request).await;
        let response = [magic.as_slice(), &[if result.is_ok() { 2 } else { 0 }]].concat();
        return write_frame(&mut stream, &response).await;
    }
    if magic == preparation::MAGIC {
        let result = preparation::read(
            reader,
            authority,
            material,
            publications,
            readers,
            permit,
            U256::from_be_slice(&request[4..]),
        )
        .await;
        let response = result.unwrap_or_else(|_| [magic.as_slice(), &[0]].concat());
        return write_frame(&mut stream, &response).await;
    }
    if magic == material::MAGIC {
        let result = material::read(
            authority,
            material,
            permit,
            U256::from_be_slice(&request[4..]),
        )
        .await;
        let response = result.unwrap_or_else(|_| [magic.as_slice(), &[0]].concat());
        return write_frame(&mut stream, &response).await;
    }
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if magic == state::MAGIC {
            let proposal_id = U256::from_be_slice(&request[4..]);
            let current = authority
                .ok_or(Error::Unavailable)?
                .read_current_patch_signing_state(proposal_id)
                .map_err(|_| Error::Unavailable)?;
            state::encode(&current)
        } else {
            let sequence =
                u64::from_be_bytes(request[4..].try_into().map_err(|_| Error::Protocol)?);
            let receipt = reader
                .read_current_publication_receipt(sequence)
                .map_err(|_| Error::Unavailable)?;
            Ok(encode_receipt(receipt.as_ref()))
        }
    })
    .await
    .map_err(|_| Error::Unavailable)?;
    let response = result.unwrap_or_else(|_| [magic.as_slice(), &[0]].concat());
    write_frame(&mut stream, &response).await
}

fn encode_receipt(receipt: Option<&FinalizedPublicationReceipt>) -> Vec<u8> {
    let mut response = MAGIC.to_vec();
    match receipt {
        None => response.push(1),
        Some(receipt) => {
            response.push(2);
            response.extend(receipt.block().number.to_be_bytes());
            response.extend(receipt.block().hash.0);
            response.extend(receipt.receipt().accepted_height.to_be_bytes());
            response.extend(receipt.receipt().receipt_hash.0);
            response.extend(receipt.receipt().body.to_bytes());
        }
    }
    response
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
        (2, RECEIPT_FRAME_LEN) => {
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

async fn read_frame(
    stream: &mut (impl AsyncRead + Unpin),
    maximum: usize,
) -> Result<Vec<u8>, Error> {
    let length = stream.read_u32().await? as usize;
    if length > maximum {
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
