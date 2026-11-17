//! Resident-aware admission evidence. Loading disk state never activates resident keys.
use crate::journal_storage::{
    private_file_options, sync_directory, JournalPaths, JournalStorageError,
};
use crate::node_host::{NODE_HOST_DIRECTORY_V1, NODE_HOST_IDENTITY_KEY_V1};
use crate::{
    in_process::InProcessTeeContext, node_host::NodeHostIdentityKey, FinalizedJoinAdmissionAnchorV1,
};
use alloy_primitives::B256;
use std::{
    fs::{self, DirBuilder, File},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::Path,
};

const ANCHOR_BYTES: usize = 241;
const DIRECTORY: &str = "tee-live-join-admission-v1";

#[derive(Debug, thiserror::Error)]
pub enum LiveJoinAnchorError {
    #[error("admission anchor I/O failed")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Storage(#[from] JournalStorageError),
    #[error("persistent NodeHost key rejected")]
    NodeHost(#[source] crate::TransportError),
    #[error("live identity codec rejected")]
    Codec(#[from] outbe_primitives::tee_attestation_v1::CodecError),
    #[error(transparent)]
    Resident(#[from] crate::in_process::InProcessError),
    #[error("admission anchor path must be private and owned")]
    UnsafePath,
    #[error("admission anchor framing rejected")]
    InvalidRecord,
    #[error("admission anchor does not match live process or stable NodeHost")]
    IdentityMismatch,
    #[error("admission anchor is not strictly newer for the same chain and NodeHost")]
    ConflictingAnchor,
}

struct Store {
    paths: JournalPaths,
    _lock: File,
}
impl Store {
    fn open(dir: &Path) -> Result<Self, LiveJoinAnchorError> {
        let mut paths = JournalPaths::new(dir, DIRECTORY);
        paths.journal = paths.root.join("anchor.bin");
        match DirBuilder::new().mode(0o700).create(&paths.root) {
            Ok(()) => sync_directory(dir)?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let metadata = fs::symlink_metadata(&paths.root)?;
        if !metadata.is_dir() || !owned(&metadata, 0o700) {
            return Err(LiveJoinAnchorError::UnsafePath);
        }
        let lock = private_file_options(true).open(&paths.lock)?;
        validate_file(&lock)?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(std::io::Error::from)?;
        // The committed file is authoritative; an interrupted scratch is never loaded.
        match fs::symlink_metadata(&paths.next) {
            Ok(m) if m.is_file() && owned(&m, 0o600) && m.nlink() == 1 => {
                fs::remove_file(&paths.next)?;
                sync_directory(&paths.root)?;
            }
            Ok(_) => return Err(LiveJoinAnchorError::UnsafePath),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(Self { paths, _lock: lock })
    }
    fn read(&self) -> Result<Option<FinalizedJoinAdmissionAnchorV1>, LiveJoinAnchorError> {
        let file = match private_file_options(false).open(&self.paths.journal) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        validate_file(&file)?;
        let mut bytes = Vec::new();
        file.take((ANCHOR_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        decode(&bytes).map(Some)
    }
    fn write(&self, anchor: FinalizedJoinAdmissionAnchorV1) -> Result<(), LiveJoinAnchorError> {
        let mut options = private_file_options(true);
        options.create_new(true);
        let mut file = options.open(&self.paths.next)?;
        file.write_all(&encode(anchor))?;
        file.sync_all()?;
        fs::rename(&self.paths.next, &self.paths.journal)?;
        sync_directory(&self.paths.root)?;
        Ok(())
    }
}

/// Save only after resident BLS/MPT admission; the stable Noise key must already exist.
pub fn persist_live_join_admission_anchor_v1(
    node_data_dir: &Path,
    live: &InProcessTeeContext,
    anchor: FinalizedJoinAdmissionAnchorV1,
) -> Result<FinalizedJoinAdmissionAnchorV1, LiveJoinAnchorError> {
    decode(&encode(anchor))?;
    validate_live(node_data_dir, live, anchor)?;
    let verified = live
        .admission_anchor()?
        .ok_or(LiveJoinAnchorError::IdentityMismatch)?;
    if (
        anchor.finalized_height,
        anchor.finalized_hash,
        anchor.finalized_state_root,
        anchor.finalized_consensus_timestamp,
    ) != (
        verified.block_number,
        verified.block_hash,
        verified.state_root,
        verified.consensus_timestamp,
    ) {
        return Err(LiveJoinAnchorError::IdentityMismatch);
    }
    let store = Store::open(node_data_dir)?;
    if let Some(old) = store.read()? {
        let same_host = (old.chain_id, old.genesis_hash, old.node_id_hash)
            == (anchor.chain_id, anchor.genesis_hash, anchor.node_id_hash);
        if !same_host
            || (old != anchor
                && (anchor.finalized_height <= old.finalized_height
                    || anchor.finalized_consensus_timestamp < old.finalized_consensus_timestamp))
        {
            return Err(LiveJoinAnchorError::ConflictingAnchor);
        }
        if old == anchor {
            return Ok(anchor);
        }
    }
    store.write(anchor)?;
    Ok(anchor)
}

/// A prior process's anchor is rejected even when its permanent NodeHost is unchanged.
pub fn load_live_join_admission_anchor_v1(
    node_data_dir: &Path,
    live: &InProcessTeeContext,
) -> Result<Option<FinalizedJoinAdmissionAnchorV1>, LiveJoinAnchorError> {
    let store = Store::open(node_data_dir)?;
    let Some(anchor) = store.read()? else {
        return Ok(None);
    };
    validate_live(node_data_dir, live, anchor)?;
    Ok(Some(anchor))
}

fn validate_live(
    dir: &Path,
    live: &InProcessTeeContext,
    a: FinalizedJoinAdmissionAnchorV1,
) -> Result<(), LiveJoinAnchorError> {
    let manifest = live.manifest();
    let key = NodeHostIdentityKey::load(
        &dir.join(NODE_HOST_DIRECTORY_V1)
            .join(NODE_HOST_IDENTITY_KEY_V1),
    )
    .map_err(LiveJoinAnchorError::NodeHost)?;
    let chain_host = (a.chain_id, a.genesis_hash, a.node_id_hash, a.enclave_id)
        == (
            manifest.chain_id,
            manifest.genesis_hash,
            manifest.node_id.node_id_hash()?,
            manifest.enclave_id()?,
        );
    if !chain_host
        || key.public() != manifest.node_host_noise_x25519
        || live.identity().network_binding != manifest.network_binding()
    {
        return Err(LiveJoinAnchorError::IdentityMismatch);
    }
    Ok(())
}
fn owned(metadata: &fs::Metadata, mode: u32) -> bool {
    metadata.uid() == rustix::process::geteuid().as_raw()
        && metadata.permissions().mode() & 0o777 == mode
}
fn validate_file(file: &File) -> Result<(), LiveJoinAnchorError> {
    let m = file.metadata()?;
    if !m.is_file() || !owned(&m, 0o600) || m.nlink() != 1 {
        return Err(LiveJoinAnchorError::UnsafePath);
    }
    Ok(())
}
fn encode(a: FinalizedJoinAdmissionAnchorV1) -> Vec<u8> {
    let mut bytes = vec![1];
    for field in [
        a.chain_id,
        a.genesis_hash.0,
        a.node_id_hash.0,
        a.enclave_id.0,
        a.intent_hash.0,
    ] {
        bytes.extend_from_slice(&field)
    }
    bytes.extend_from_slice(&a.finalized_height.to_be_bytes());
    bytes.extend_from_slice(a.finalized_hash.as_slice());
    bytes.extend_from_slice(a.finalized_state_root.as_slice());
    bytes.extend_from_slice(&a.finalized_consensus_timestamp.to_be_bytes());
    bytes
}
fn take<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N], LiveJoinAnchorError> {
    let (field, rest) = bytes
        .split_at_checked(N)
        .ok_or(LiveJoinAnchorError::InvalidRecord)?;
    *bytes = rest;
    field
        .try_into()
        .map_err(|_| LiveJoinAnchorError::InvalidRecord)
}
fn decode(mut bytes: &[u8]) -> Result<FinalizedJoinAdmissionAnchorV1, LiveJoinAnchorError> {
    if bytes.len() != ANCHOR_BYTES || take::<1>(&mut bytes)? != [1] {
        return Err(LiveJoinAnchorError::InvalidRecord);
    }
    let a = FinalizedJoinAdmissionAnchorV1 {
        chain_id: take(&mut bytes)?,
        genesis_hash: B256::from(take::<32>(&mut bytes)?),
        node_id_hash: B256::from(take::<32>(&mut bytes)?),
        enclave_id: B256::from(take::<32>(&mut bytes)?),
        intent_hash: B256::from(take::<32>(&mut bytes)?),
        finalized_height: u64::from_be_bytes(take(&mut bytes)?),
        finalized_hash: B256::from(take::<32>(&mut bytes)?),
        finalized_state_root: B256::from(take::<32>(&mut bytes)?),
        finalized_consensus_timestamp: u64::from_be_bytes(take(&mut bytes)?),
    };
    if [
        a.genesis_hash,
        a.node_id_hash,
        a.enclave_id,
        a.intent_hash,
        a.finalized_hash,
        a.finalized_state_root,
    ]
    .iter()
    .any(B256::is_zero)
        || a.chain_id == [0; 32]
        || a.finalized_height == 0
    {
        return Err(LiveJoinAnchorError::InvalidRecord);
    }
    Ok(a)
}
