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

#[cfg(test)]
mod tests {
    use super::{
        load_live_join_admission_anchor_v1, persist_live_join_admission_anchor_v1,
        LiveJoinAnchorError, Store, NODE_HOST_DIRECTORY_V1, NODE_HOST_IDENTITY_KEY_V1,
    };
    use crate::in_process::admission::VerifiedAdmissionAnchorV1;
    use crate::in_process::{InProcessTeeContext, ProofSourceV1};
    use crate::node_host::NodeHostIdentityKey;
    use crate::resident::{ResidentOfferMaterial, ResidentTeeKeys};
    use crate::FinalizedJoinAdmissionAnchorV1;
    use crate::NodeHostIdentityV1;
    use alloy_primitives::B256;
    use outbe_primitives::tee_attestation_v1::{
        AttestationMode, EnclaveInitializationManifestV1, NetworkBindingV1, NodeIdV1,
        TrustedNetworkDescriptorV1,
    };
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    fn resident() -> InProcessTeeContext {
        let keys = ResidentTeeKeys::from_seed([7; 32]).unwrap();
        let node_key = k256::ecdsa::SigningKey::from_slice(&[7; 32]).unwrap();
        let reth_p2p_public = node_key
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap();
        let binding = NetworkBindingV1 {
            chain_id: alloy_primitives::U256::from(676).to_be_bytes(),
            genesis_hash: B256::repeat_byte(2),
            attestation_mode: AttestationMode::NoAttestationDev,
        };
        let manifest = EnclaveInitializationManifestV1 {
            chain_id: binding.chain_id,
            genesis_hash: binding.genesis_hash,
            attestation_mode: binding.attestation_mode,
            node_id: NodeIdV1 { reth_p2p_public },
            initialization_challenge: [3; 32],
            node_host_noise_x25519: NodeHostIdentityKey::from_private([4; 32]).public(),
            recipient_x25519: keys.tribute_offer_public(),
            attestation_ed25519: keys.attestation_pub(),
            noise_responder_x25519: keys.noise_public(),
        };
        InProcessTeeContext::new(
            keys,
            NodeHostIdentityV1 {
                network_binding: binding,
                reth_p2p_public,
            },
            manifest,
            TrustedNetworkDescriptorV1 {
                network_binding: binding,
                genesis_consensus_keys: Vec::new(),
            },
            ProofSourceV1::NoHardware,
        )
        .unwrap()
    }

    fn write_host_key(dir: &Path) {
        let host = dir.join(NODE_HOST_DIRECTORY_V1);
        std::fs::create_dir_all(&host).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(host.join(NODE_HOST_IDENTITY_KEY_V1))
            .unwrap();
        file.write_all(&[4; 32]).unwrap();
    }

    fn anchor(
        live: &InProcessTeeContext,
        height: u64,
        mark: u8,
        timestamp: u64,
    ) -> FinalizedJoinAdmissionAnchorV1 {
        let manifest = live.manifest();
        FinalizedJoinAdmissionAnchorV1 {
            chain_id: manifest.chain_id,
            genesis_hash: manifest.genesis_hash,
            node_id_hash: manifest.node_id.node_id_hash().unwrap(),
            enclave_id: manifest.enclave_id().unwrap(),
            intent_hash: B256::repeat_byte(9),
            finalized_height: height,
            finalized_hash: B256::repeat_byte(mark),
            finalized_state_root: B256::repeat_byte(mark.wrapping_add(1)),
            finalized_consensus_timestamp: timestamp,
        }
    }

    fn verified(anchor: FinalizedJoinAdmissionAnchorV1) -> VerifiedAdmissionAnchorV1 {
        VerifiedAdmissionAnchorV1 {
            block_number: anchor.finalized_height,
            block_hash: anchor.finalized_hash,
            state_root: anchor.finalized_state_root,
            consensus_timestamp: anchor.finalized_consensus_timestamp,
        }
    }

    fn material(live: &InProcessTeeContext) -> ResidentOfferMaterial {
        ResidentOfferMaterial::for_test(vec![5; 96], B256::from(live.manifest().chain_id), 0, 0)
            .unwrap()
    }

    fn plant(dir: &Path, anchor: FinalizedJoinAdmissionAnchorV1) {
        let store = Store::open(dir).unwrap();
        store.write(anchor).unwrap();
    }

    #[test]
    fn same_node_host_anchor_rewrite_is_idempotent_and_strictly_monotonic() {
        let live = resident();
        let current = anchor(&live, 10, 0x11, 1_000);
        let conflict = anchor(&live, 10, 0x22, 1_000);
        let later = anchor(&live, 11, 0x33, 1_001);
        let rollback_disk = anchor(&live, 12, 0x44, 2_000);

        let conflict_dir = tempfile::tempdir().unwrap();
        write_host_key(conflict_dir.path());
        plant(conflict_dir.path(), current);
        live.activate_for_test_with_anchor(material(&live), verified(conflict))
            .unwrap();
        assert!(matches!(
            persist_live_join_admission_anchor_v1(conflict_dir.path(), &live, conflict),
            Err(LiveJoinAnchorError::ConflictingAnchor)
        ));

        let dir = tempfile::tempdir().unwrap();
        write_host_key(dir.path());
        assert_eq!(
            persist_live_join_admission_anchor_v1(dir.path(), &live, conflict).unwrap(),
            conflict
        );
        assert_eq!(
            persist_live_join_admission_anchor_v1(dir.path(), &live, conflict).unwrap(),
            conflict
        );

        live.activate_for_test_with_anchor(material(&live), verified(later))
            .unwrap();
        assert_eq!(
            persist_live_join_admission_anchor_v1(dir.path(), &live, later).unwrap(),
            later
        );
        assert_eq!(
            load_live_join_admission_anchor_v1(dir.path(), &live).unwrap(),
            Some(later)
        );

        let rollback_dir = tempfile::tempdir().unwrap();
        write_host_key(rollback_dir.path());
        plant(rollback_dir.path(), rollback_disk);
        assert!(matches!(
            persist_live_join_admission_anchor_v1(rollback_dir.path(), &live, later),
            Err(LiveJoinAnchorError::ConflictingAnchor)
        ));
    }

    #[test]
    fn corrupt_registration_anchor_is_rejected_and_retained() {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::open(dir.path()).unwrap());
        let path = dir.path().join("tee-live-join-admission-v1/anchor.bin");
        let corrupt = [0xff, 0x00];
        crate::journal_storage::private_file_options(true)
            .open(&path)
            .unwrap()
            .write_all(&corrupt)
            .unwrap();
        assert!(matches!(
            load_live_join_admission_anchor_v1(dir.path(), &resident()),
            Err(LiveJoinAnchorError::InvalidRecord)
        ));
        assert!(path.exists());
        assert_eq!(std::fs::read(path).unwrap(), corrupt);
    }

    #[test]
    fn restart_discards_anchor_scratch_and_keeps_the_committed_anchor() {
        let live = resident();
        let dir = tempfile::tempdir().unwrap();
        write_host_key(dir.path());
        let saved = anchor(&live, 10, 0x11, 1_000);
        plant(dir.path(), saved);
        let scratch = dir.path().join("tee-live-join-admission-v1/journal.next");
        crate::journal_storage::private_file_options(true)
            .create_new(true)
            .open(&scratch)
            .unwrap()
            .write_all(b"torn")
            .unwrap();
        assert_eq!(
            load_live_join_admission_anchor_v1(dir.path(), &live).unwrap(),
            Some(saved)
        );
        assert!(!scratch.exists());
        assert!(dir
            .path()
            .join("tee-live-join-admission-v1/anchor.bin")
            .exists());
    }
}
