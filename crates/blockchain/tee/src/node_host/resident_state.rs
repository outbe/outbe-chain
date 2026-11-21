//! Plain local resident storage until hardware sealing is available.

use super::{
    filesystem::write_bytes_once, locked_state::lock_node_host_state, path_exists,
    replace_bytes_atomically,
};
use crate::{
    in_process::admission::VerifiedAdmissionAnchorV1,
    resident::{ResidentOfferMaterial, ResidentTeeKeys, TeeCryptoError},
    TransportError,
};
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{
    CodecError, EnclaveInitializationManifestV1, NodeHostAuthorizationWitnessV1,
};
use ring::rand::SecureRandom as _;
use std::{
    fs::OpenOptions,
    io::Read as _,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"OBRESID2";
const HEADER_BYTES: usize = 8 + 32 + 32 + 32 + 1;
const MAX_GROUP_SIGNATURE_BYTES: usize = 512;
const ANCHOR_BYTES: usize = 8 + 32 + 32 + 8;
const MAX_RECORD_BYTES: u64 =
    (HEADER_BYTES + 2 + MAX_GROUP_SIGNATURE_BYTES + 16 + ANCHOR_BYTES) as u64;

#[derive(Debug, thiserror::Error)]
pub enum ResidentStateError {
    #[error("resident state file is invalid")]
    InvalidRecord,
    #[error("resident state file path is unsafe")]
    UnsafePath,
    #[error("resident state file does not match the local initialization manifest")]
    BindingMismatch,
    #[error("resident state file belongs to a different in-process identity")]
    IdentityConflict,
    #[error("resident group signature exceeds the local record bound")]
    GroupSignatureTooLong,
    #[error("resident state file is missing during promotion")]
    MissingRecord,
    #[error("resident state file operation failed")]
    Storage(Box<TransportError>),
    #[error("resident key reconstruction failed")]
    Crypto(#[from] TeeCryptoError),
    #[error("resident manifest encoding failed")]
    Codec(#[from] CodecError),
}

pub struct LoadedResidentState {
    pub keys: ResidentTeeKeys,
    pub initialization_challenge: [u8; 32],
    pub material: Option<ResidentOfferMaterial>,
    pub anchor: Option<VerifiedAdmissionAnchorV1>,
}

pub struct ResidentStateStore {
    node_data_dir: PathBuf,
}

impl From<TransportError> for ResidentStateError {
    fn from(error: TransportError) -> Self {
        Self::Storage(Box::new(error))
    }
}

enum DecodedState {
    Unkeyed,
    FoundingDkg {
        signature: Zeroizing<Vec<u8>>,
        key_epoch: u64,
        tribute_offer_epoch: u64,
    },
    Anchored {
        signature: Zeroizing<Vec<u8>>,
        key_epoch: u64,
        tribute_offer_epoch: u64,
        anchor: VerifiedAdmissionAnchorV1,
    },
}

struct DecodedRecord {
    seed: Zeroizing<[u8; 32]>,
    challenge: [u8; 32],
    manifest_authorization_hash: B256,
    state: DecodedState,
}

impl ResidentStateStore {
    pub fn load_or_create(
        node_data_dir: &Path,
        authorization: &NodeHostAuthorizationWitnessV1,
    ) -> Result<(Self, LoadedResidentState), ResidentStateError> {
        let store = Self {
            node_data_dir: node_data_dir.to_path_buf(),
        };
        let (paths, _lock) = lock_node_host_state(node_data_dir)?;
        if path_exists(&paths.resident_state)? {
            let bytes = read_resident_file(&paths.resident_state)?;
            let decoded = decode(&bytes)?;
            let keys = ResidentTeeKeys::new_with_identity_seed(decoded.seed)?;
            let expected_hash =
                manifest_authorization_hash(authorization, &keys, decoded.challenge)?;
            if expected_hash != decoded.manifest_authorization_hash {
                return Err(ResidentStateError::BindingMismatch);
            }
            let chain_id = B256::from(authorization.chain_id);
            let (material, anchor) = match decoded.state {
                DecodedState::Unkeyed => (None, None),
                DecodedState::FoundingDkg {
                    signature,
                    key_epoch,
                    tribute_offer_epoch,
                } => (
                    Some(ResidentOfferMaterial::from_group_signature(
                        signature,
                        chain_id,
                        key_epoch,
                        tribute_offer_epoch,
                    )?),
                    None,
                ),
                DecodedState::Anchored {
                    signature,
                    key_epoch,
                    tribute_offer_epoch,
                    anchor,
                } => (
                    Some(ResidentOfferMaterial::from_group_signature(
                        signature,
                        chain_id,
                        key_epoch,
                        tribute_offer_epoch,
                    )?),
                    Some(anchor),
                ),
            };
            return Ok((
                store,
                LoadedResidentState {
                    keys,
                    initialization_challenge: decoded.challenge,
                    material,
                    anchor,
                },
            ));
        }

        let keys = ResidentTeeKeys::generate()?;
        let mut initialization_challenge = [0; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut initialization_challenge)
            .map_err(crate::errors::TeeCryptoError::rng_failure)
            .map_err(TeeCryptoError::from)?;
        let manifest_hash =
            manifest_authorization_hash(authorization, &keys, initialization_challenge)?;
        let bytes = encode(&keys, initialization_challenge, manifest_hash, None)?;
        write_bytes_once(&paths.resident_state, &bytes, &paths.root)?;
        Ok((
            store,
            LoadedResidentState {
                keys,
                initialization_challenge,
                material: None,
                anchor: None,
            },
        ))
    }

    pub fn persist_active(
        &self,
        keys: &ResidentTeeKeys,
        challenge: [u8; 32],
        manifest_authorization_hash: B256,
        material: &ResidentOfferMaterial,
        anchor: Option<VerifiedAdmissionAnchorV1>,
    ) -> Result<(), ResidentStateError> {
        let (paths, _lock) = lock_node_host_state(&self.node_data_dir)?;
        let current = read_resident_file(&paths.resident_state)?;
        let existing = decode(&current)?;
        if existing.seed.as_ref() != keys.identity_seed.as_ref() || existing.challenge != challenge
        {
            return Err(ResidentStateError::IdentityConflict);
        }
        if existing.manifest_authorization_hash != manifest_authorization_hash {
            return Err(ResidentStateError::BindingMismatch);
        }
        let bytes = encode(
            keys,
            challenge,
            manifest_authorization_hash,
            Some((material, anchor)),
        )?;
        if current.as_slice() != bytes.as_slice() {
            replace_bytes_atomically(
                &paths.resident_state,
                &paths.resident_state_next,
                &paths.resident_state_scratch,
                &bytes,
                &paths.root,
            )?;
        }
        Ok(())
    }

    pub fn replace_promoted(
        node_data_dir: &Path,
        keys: &ResidentTeeKeys,
        challenge: [u8; 32],
        manifest_authorization_hash: B256,
        material: &ResidentOfferMaterial,
        anchor: Option<VerifiedAdmissionAnchorV1>,
    ) -> Result<(), ResidentStateError> {
        let (paths, _lock) = lock_node_host_state(node_data_dir)?;
        if !path_exists(&paths.resident_state)? {
            return Err(ResidentStateError::MissingRecord);
        }
        let current = read_resident_file(&paths.resident_state)?;
        decode(&current)?;
        let bytes = encode(
            keys,
            challenge,
            manifest_authorization_hash,
            Some((material, anchor)),
        )?;
        replace_bytes_atomically(
            &paths.resident_state,
            &paths.resident_state_next,
            &paths.resident_state_scratch,
            &bytes,
            &paths.root,
        )?;
        Ok(())
    }
}

fn manifest_authorization_hash(
    authorization: &NodeHostAuthorizationWitnessV1,
    keys: &ResidentTeeKeys,
    initialization_challenge: [u8; 32],
) -> Result<B256, ResidentStateError> {
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: authorization.chain_id,
        genesis_hash: authorization.genesis_hash,
        attestation_mode: authorization.attestation_mode,
        node_id: authorization.node_id.clone(),
        initialization_challenge,
        node_host_noise_x25519: authorization.node_host_noise_x25519,
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    Ok(manifest.authorization_hash()?)
}

fn read_resident_file(path: &Path) -> Result<Zeroizing<Vec<u8>>, ResidentStateError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                ResidentStateError::UnsafePath
            } else {
                TransportError::from(error).into()
            }
        })?;
    let metadata = file.metadata().map_err(TransportError::from)?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.nlink() != 1
    {
        return Err(ResidentStateError::UnsafePath);
    }
    if metadata.len() > MAX_RECORD_BYTES {
        return Err(ResidentStateError::InvalidRecord);
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(metadata.len() as usize));
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(TransportError::from)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(ResidentStateError::InvalidRecord);
    }
    Ok(bytes)
}

fn validate_anchor(anchor: &VerifiedAdmissionAnchorV1) -> Result<(), ResidentStateError> {
    if anchor.block_number == 0 || anchor.block_hash.is_zero() || anchor.state_root.is_zero() {
        return Err(ResidentStateError::InvalidRecord);
    }
    Ok(())
}

fn encode(
    keys: &ResidentTeeKeys,
    challenge: [u8; 32],
    manifest_authorization_hash: B256,
    active: Option<(&ResidentOfferMaterial, Option<VerifiedAdmissionAnchorV1>)>,
) -> Result<Zeroizing<Vec<u8>>, ResidentStateError> {
    if manifest_authorization_hash.is_zero() {
        return Err(ResidentStateError::InvalidRecord);
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_RECORD_BYTES as usize));
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(keys.identity_seed.as_ref());
    bytes.extend_from_slice(&challenge);
    bytes.extend_from_slice(manifest_authorization_hash.as_slice());
    match active {
        None => bytes.push(0),
        Some((material, anchor)) => {
            let signature = material.group_sig.as_slice();
            if signature.is_empty() || signature.len() > MAX_GROUP_SIGNATURE_BYTES {
                return Err(ResidentStateError::GroupSignatureTooLong);
            }
            if let Some(anchor) = &anchor {
                validate_anchor(anchor)?;
                bytes.push(2);
            } else {
                bytes.push(1);
            }
            bytes.extend_from_slice(&(signature.len() as u16).to_be_bytes());
            bytes.extend_from_slice(signature);
            bytes.extend_from_slice(&material.key_epoch.to_be_bytes());
            bytes.extend_from_slice(&material.tribute_offer_epoch.to_be_bytes());
            if let Some(anchor) = anchor {
                bytes.extend_from_slice(&anchor.block_number.to_be_bytes());
                bytes.extend_from_slice(anchor.block_hash.as_slice());
                bytes.extend_from_slice(anchor.state_root.as_slice());
                bytes.extend_from_slice(&anchor.consensus_timestamp.to_be_bytes());
            }
        }
    }
    Ok(bytes)
}

fn decode(bytes: &[u8]) -> Result<DecodedRecord, ResidentStateError> {
    if bytes.len() < HEADER_BYTES || &bytes[..8] != MAGIC {
        return Err(ResidentStateError::InvalidRecord);
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    seed.copy_from_slice(&bytes[8..40]);
    let mut challenge = [0u8; 32];
    challenge.copy_from_slice(&bytes[40..72]);
    let manifest_authorization_hash = B256::from_slice(&bytes[72..104]);
    if manifest_authorization_hash.is_zero() {
        return Err(ResidentStateError::InvalidRecord);
    }
    let state = match bytes[104] {
        0 if bytes.len() == HEADER_BYTES => DecodedState::Unkeyed,
        tag @ (1 | 2) => {
            let anchor_len = if tag == 2 { ANCHOR_BYTES } else { 0 };
            if bytes.len() < HEADER_BYTES + 2 + 16 + anchor_len {
                return Err(ResidentStateError::InvalidRecord);
            }
            let length = u16::from_be_bytes([bytes[105], bytes[106]]) as usize;
            let sig_start = HEADER_BYTES + 2;
            let epochs_start = sig_start
                .checked_add(length)
                .ok_or(ResidentStateError::InvalidRecord)?;
            let anchor_start = epochs_start
                .checked_add(16)
                .ok_or(ResidentStateError::InvalidRecord)?;
            let expected = anchor_start
                .checked_add(anchor_len)
                .ok_or(ResidentStateError::InvalidRecord)?;
            if length == 0 || length > MAX_GROUP_SIGNATURE_BYTES || bytes.len() != expected {
                return Err(ResidentStateError::InvalidRecord);
            }
            let signature = Zeroizing::new(bytes[sig_start..epochs_start].to_vec());
            let key_epoch = u64::from_be_bytes(
                bytes[epochs_start..epochs_start + 8]
                    .try_into()
                    .map_err(|_| ResidentStateError::InvalidRecord)?,
            );
            let tribute_offer_epoch = u64::from_be_bytes(
                bytes[epochs_start + 8..anchor_start]
                    .try_into()
                    .map_err(|_| ResidentStateError::InvalidRecord)?,
            );
            if tag == 1 {
                DecodedState::FoundingDkg {
                    signature,
                    key_epoch,
                    tribute_offer_epoch,
                }
            } else {
                let block_number = u64::from_be_bytes(
                    bytes[anchor_start..anchor_start + 8]
                        .try_into()
                        .map_err(|_| ResidentStateError::InvalidRecord)?,
                );
                let block_hash = B256::from_slice(&bytes[anchor_start + 8..anchor_start + 40]);
                let state_root = B256::from_slice(&bytes[anchor_start + 40..anchor_start + 72]);
                let consensus_timestamp = u64::from_be_bytes(
                    bytes[anchor_start + 72..anchor_start + 80]
                        .try_into()
                        .map_err(|_| ResidentStateError::InvalidRecord)?,
                );
                let anchor = VerifiedAdmissionAnchorV1 {
                    block_number,
                    block_hash,
                    state_root,
                    consensus_timestamp,
                };
                validate_anchor(&anchor)?;
                DecodedState::Anchored {
                    signature,
                    key_epoch,
                    tribute_offer_epoch,
                    anchor,
                }
            }
        }
        _ => return Err(ResidentStateError::InvalidRecord),
    };
    Ok(DecodedRecord {
        seed,
        challenge,
        manifest_authorization_hash,
        state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NodeIdV1};
    use std::{fs, os::unix::fs::symlink};

    fn test_p2p_public(seed: u8) -> [u8; 33] {
        k256::ecdsa::SigningKey::from_slice(&[seed; 32])
            .unwrap()
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap()
    }

    fn test_authorization(seed: u8) -> NodeHostAuthorizationWitnessV1 {
        NodeHostAuthorizationWitnessV1 {
            chain_id: [3; 32],
            genesis_hash: B256::repeat_byte(4),
            attestation_mode: AttestationMode::NoAttestationDev,
            node_id: NodeIdV1 {
                reth_p2p_public: test_p2p_public(seed),
            },
            node_host_noise_x25519: [5; 32],
        }
    }

    #[test]
    fn resident_identity_and_active_offer_survive_restart_for_tag1_and_tag2() {
        let directory = tempfile::tempdir().unwrap();
        let auth = test_authorization(11);
        let chain_id = B256::from(auth.chain_id);
        let (store, first) = ResidentStateStore::load_or_create(directory.path(), &auth).unwrap();
        let paths = super::super::NodeHostPaths::new(directory.path());
        assert_eq!(
            fs::metadata(&paths.resident_state)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(first.material.is_none());
        assert!(first.anchor.is_none());
        let initial_public = first.keys.noise_public();
        let challenge = first.initialization_challenge;
        let manifest_hash = manifest_authorization_hash(&auth, &first.keys, challenge).unwrap();
        let material = ResidentOfferMaterial::for_test(vec![7; 96], chain_id, 2, 4).unwrap();
        let offer_public = material.public();

        // Tag 1 (FoundingDkg, anchor: None)
        store
            .persist_active(&first.keys, challenge, manifest_hash, &material, None)
            .unwrap();
        let (_, restored_tag1) =
            ResidentStateStore::load_or_create(directory.path(), &auth).unwrap();
        assert_eq!(restored_tag1.keys.noise_public(), initial_public);
        assert_eq!(
            restored_tag1.keys.attestation_pub(),
            first.keys.attestation_pub()
        );
        assert_eq!(
            restored_tag1.keys.tribute_offer_public(),
            first.keys.tribute_offer_public()
        );
        assert_eq!(restored_tag1.initialization_challenge, challenge);
        assert!(restored_tag1.anchor.is_none());
        let restored_offer = restored_tag1.material.unwrap();
        assert_eq!(restored_offer.public(), offer_public);
        assert_eq!(restored_offer.key_epoch(), 2);
        assert_eq!(restored_offer.tribute_offer_epoch(), 4);

        // Tag 2 (Anchored)
        let anchor = VerifiedAdmissionAnchorV1 {
            block_number: 42,
            block_hash: B256::repeat_byte(0xAA),
            state_root: B256::repeat_byte(0xBB),
            consensus_timestamp: 1_700_000_042,
        };
        store
            .persist_active(
                &first.keys,
                challenge,
                manifest_hash,
                &material,
                Some(anchor),
            )
            .unwrap();
        let (_, restored_tag2) =
            ResidentStateStore::load_or_create(directory.path(), &auth).unwrap();
        assert_eq!(restored_tag2.anchor, Some(anchor));
        assert_eq!(restored_tag2.material.unwrap().public(), offer_public);
    }

    #[test]
    fn resident_file_rejects_obresid1_corruption_unsafe_paths_and_binding_mismatch_without_overwrite(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let auth = test_authorization(11);
        ResidentStateStore::load_or_create(directory.path(), &auth).unwrap();
        let paths = super::super::NodeHostPaths::new(directory.path());
        let original_bytes = fs::read(&paths.resident_state).unwrap();

        // Changed chain_id, genesis_hash, attestation_mode, P2P key, NodeHost key -> BindingMismatch.
        let mut wrong_chain = auth.clone();
        wrong_chain.chain_id = [9; 32];
        let mut wrong_genesis = auth.clone();
        wrong_genesis.genesis_hash = B256::repeat_byte(9);
        let mut wrong_mode = auth.clone();
        wrong_mode.attestation_mode = AttestationMode::DcapRequired;
        let mut wrong_p2p = auth.clone();
        wrong_p2p.node_id = NodeIdV1 {
            reth_p2p_public: test_p2p_public(22),
        };
        let mut wrong_host = auth.clone();
        wrong_host.node_host_noise_x25519 = [6; 32];
        for bad_auth in [
            wrong_chain,
            wrong_genesis,
            wrong_mode,
            wrong_p2p,
            wrong_host,
        ] {
            assert!(matches!(
                ResidentStateStore::load_or_create(directory.path(), &bad_auth),
                Err(ResidentStateError::BindingMismatch)
            ));
            assert_eq!(fs::read(&paths.resident_state).unwrap(), original_bytes);
        }

        // Unsafe mode 0644 -> UnsafePath without overwrite.
        fs::set_permissions(&paths.resident_state, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            ResidentStateStore::load_or_create(directory.path(), &auth),
            Err(ResidentStateError::UnsafePath)
        ));
        fs::set_permissions(&paths.resident_state, fs::Permissions::from_mode(0o600)).unwrap();

        // Hardlink (nlink == 2) -> UnsafePath without overwrite.
        let hardlink = directory.path().join("resident-hardlink");
        fs::hard_link(&paths.resident_state, &hardlink).unwrap();
        assert!(matches!(
            ResidentStateStore::load_or_create(directory.path(), &auth),
            Err(ResidentStateError::UnsafePath)
        ));
        fs::remove_file(&hardlink).unwrap();

        // Leftover OBRESID1 record at same path -> InvalidRecord without overwrite.
        let mut obresid1 = Vec::new();
        obresid1.extend_from_slice(b"OBRESID1");
        obresid1.extend_from_slice(&[1; 32]);
        obresid1.extend_from_slice(&[2; 32]);
        obresid1.push(0);
        fs::write(&paths.resident_state, &obresid1).unwrap();
        assert!(matches!(
            ResidentStateStore::load_or_create(directory.path(), &auth),
            Err(ResidentStateError::InvalidRecord)
        ));
        assert_eq!(fs::read(&paths.resident_state).unwrap(), obresid1);

        // Corrupt bytes -> InvalidRecord without overwrite.
        fs::write(&paths.resident_state, b"broken").unwrap();
        assert!(matches!(
            ResidentStateStore::load_or_create(directory.path(), &auth),
            Err(ResidentStateError::InvalidRecord)
        ));
        assert_eq!(fs::read(&paths.resident_state).unwrap(), b"broken");

        // Symlink -> UnsafePath.
        fs::remove_file(&paths.resident_state).unwrap();
        symlink(directory.path().join("absent"), &paths.resident_state).unwrap();
        assert!(matches!(
            ResidentStateStore::load_or_create(directory.path(), &auth),
            Err(ResidentStateError::UnsafePath)
        ));

        // FIFO -> UnsafePath without blocking on open.
        fs::remove_file(&paths.resident_state).unwrap();
        let status = std::process::Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(&paths.resident_state)
            .status()
            .unwrap();
        assert!(status.success(), "mkfifo failed: {status}");
        assert!(matches!(
            ResidentStateStore::load_or_create(directory.path(), &auth),
            Err(ResidentStateError::UnsafePath)
        ));
    }

    #[test]
    fn finalized_successor_replaces_resident_file_with_tag2_anchor() {
        let directory = tempfile::tempdir().unwrap();
        let auth = test_authorization(11);
        let chain_id = B256::from(auth.chain_id);
        let (_, original) = ResidentStateStore::load_or_create(directory.path(), &auth).unwrap();
        let successor = ResidentTeeKeys::generate().unwrap();
        let successor_public = successor.noise_public();
        let challenge = [5; 32];
        let manifest_hash = manifest_authorization_hash(&auth, &successor, challenge).unwrap();
        let material = ResidentOfferMaterial::for_test(vec![9; 96], chain_id, 5, 6).unwrap();
        let offer_public = material.public();
        let anchor = VerifiedAdmissionAnchorV1 {
            block_number: 100,
            block_hash: B256::repeat_byte(0x12),
            state_root: B256::repeat_byte(0x34),
            consensus_timestamp: 1_700_000_100,
        };
        ResidentStateStore::replace_promoted(
            directory.path(),
            &successor,
            challenge,
            manifest_hash,
            &material,
            Some(anchor),
        )
        .unwrap();

        let (_, restored) = ResidentStateStore::load_or_create(directory.path(), &auth).unwrap();
        assert_ne!(restored.keys.noise_public(), original.keys.noise_public());
        assert_eq!(restored.keys.noise_public(), successor_public);
        assert_eq!(restored.initialization_challenge, challenge);
        assert_eq!(restored.material.unwrap().public(), offer_public);
        assert_eq!(restored.anchor, Some(anchor));
    }
}
