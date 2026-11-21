//! Plain local resident storage until hardware sealing is available.

use super::{
    filesystem::write_bytes_once, locked_state::lock_node_host_state, path_exists,
    read_owned_bounded_file, replace_bytes_atomically,
};
use crate::{
    resident::{ResidentOfferMaterial, ResidentTeeKeys, TeeCryptoError},
    TransportError,
};
use alloy_primitives::B256;
use ring::rand::SecureRandom as _;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"OBRESID1";
const MAX_GROUP_SIGNATURE_BYTES: usize = 512;
const MAX_RECORD_BYTES: u64 = 8 + 32 + 32 + 1 + 2 + 512 + 8 + 8;

#[derive(Debug, thiserror::Error)]
pub enum ResidentStateError {
    #[error("resident state file is invalid")]
    InvalidRecord,
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
}

pub struct LoadedResidentState {
    pub keys: ResidentTeeKeys,
    pub initialization_challenge: [u8; 32],
    pub material: Option<ResidentOfferMaterial>,
}

pub struct ResidentStateStore {
    node_data_dir: PathBuf,
}

impl From<TransportError> for ResidentStateError {
    fn from(error: TransportError) -> Self {
        Self::Storage(Box::new(error))
    }
}

struct DecodedRecord {
    seed: Zeroizing<[u8; 32]>,
    challenge: [u8; 32],
    material: Option<(Zeroizing<Vec<u8>>, u64, u64)>,
}

impl ResidentStateStore {
    pub fn load_or_create(
        node_data_dir: &Path,
        chain_id: B256,
    ) -> Result<(Self, LoadedResidentState), ResidentStateError> {
        let store = Self {
            node_data_dir: node_data_dir.to_path_buf(),
        };
        let (paths, _lock) = lock_node_host_state(node_data_dir)?;
        if path_exists(&paths.resident_state)? {
            let bytes = Zeroizing::new(read_owned_bounded_file(
                &paths.resident_state,
                MAX_RECORD_BYTES,
                "resident state",
            )?);
            let decoded = decode(&bytes)?;
            let keys = ResidentTeeKeys::new_with_identity_seed(decoded.seed)?;
            let material = decoded
                .material
                .map(|(signature, key_epoch, tribute_offer_epoch)| {
                    ResidentOfferMaterial::from_group_signature(
                        signature,
                        chain_id,
                        key_epoch,
                        tribute_offer_epoch,
                    )
                })
                .transpose()?;
            return Ok((
                store,
                LoadedResidentState {
                    keys,
                    initialization_challenge: decoded.challenge,
                    material,
                },
            ));
        }

        let keys = ResidentTeeKeys::generate()?;
        let mut initialization_challenge = [0; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut initialization_challenge)
            .map_err(crate::errors::TeeCryptoError::rng_failure)
            .map_err(TeeCryptoError::from)?;
        let bytes = encode(&keys, initialization_challenge, None)?;
        write_bytes_once(&paths.resident_state, &bytes, &paths.root)?;
        Ok((
            store,
            LoadedResidentState {
                keys,
                initialization_challenge,
                material: None,
            },
        ))
    }

    pub fn persist_active(
        &self,
        keys: &ResidentTeeKeys,
        challenge: [u8; 32],
        material: &ResidentOfferMaterial,
    ) -> Result<(), ResidentStateError> {
        let (paths, _lock) = lock_node_host_state(&self.node_data_dir)?;
        let current = Zeroizing::new(read_owned_bounded_file(
            &paths.resident_state,
            MAX_RECORD_BYTES,
            "resident state",
        )?);
        let existing = decode(&current)?;
        if existing.seed.as_ref() != keys.identity_seed.as_ref() || existing.challenge != challenge
        {
            return Err(ResidentStateError::IdentityConflict);
        }
        let bytes = encode(keys, challenge, Some(material))?;
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
        material: &ResidentOfferMaterial,
    ) -> Result<(), ResidentStateError> {
        let (paths, _lock) = lock_node_host_state(node_data_dir)?;
        if !path_exists(&paths.resident_state)? {
            return Err(ResidentStateError::MissingRecord);
        }
        let current = Zeroizing::new(read_owned_bounded_file(
            &paths.resident_state,
            MAX_RECORD_BYTES,
            "resident state",
        )?);
        decode(&current)?;
        let bytes = encode(keys, challenge, Some(material))?;
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

fn encode(
    keys: &ResidentTeeKeys,
    challenge: [u8; 32],
    material: Option<&ResidentOfferMaterial>,
) -> Result<Zeroizing<Vec<u8>>, ResidentStateError> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_RECORD_BYTES as usize));
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(keys.identity_seed.as_ref());
    bytes.extend_from_slice(&challenge);
    if let Some(material) = material {
        let signature = material.group_sig.as_slice();
        if signature.is_empty() || signature.len() > MAX_GROUP_SIGNATURE_BYTES {
            return Err(ResidentStateError::GroupSignatureTooLong);
        }
        bytes.push(1);
        bytes.extend_from_slice(&(signature.len() as u16).to_be_bytes());
        bytes.extend_from_slice(signature);
        bytes.extend_from_slice(&material.key_epoch.to_be_bytes());
        bytes.extend_from_slice(&material.tribute_offer_epoch.to_be_bytes());
    } else {
        bytes.push(0);
    }
    Ok(bytes)
}

fn decode(bytes: &[u8]) -> Result<DecodedRecord, ResidentStateError> {
    if bytes.len() < 73 || &bytes[..8] != MAGIC {
        return Err(ResidentStateError::InvalidRecord);
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    seed.copy_from_slice(&bytes[8..40]);
    let mut challenge = [0u8; 32];
    challenge.copy_from_slice(&bytes[40..72]);
    let material = match bytes[72] {
        0 if bytes.len() == 73 => None,
        1 if bytes.len() >= 91 => {
            let length = u16::from_be_bytes([bytes[73], bytes[74]]) as usize;
            let expected = 75usize
                .checked_add(length)
                .and_then(|value| value.checked_add(16))
                .ok_or(ResidentStateError::InvalidRecord)?;
            if length == 0 || length > MAX_GROUP_SIGNATURE_BYTES || bytes.len() != expected {
                return Err(ResidentStateError::InvalidRecord);
            }
            let signature = Zeroizing::new(bytes[75..75 + length].to_vec());
            let key_epoch = u64::from_be_bytes(
                bytes[75 + length..83 + length]
                    .try_into()
                    .map_err(|_| ResidentStateError::InvalidRecord)?,
            );
            let tribute_offer_epoch = u64::from_be_bytes(
                bytes[83 + length..91 + length]
                    .try_into()
                    .map_err(|_| ResidentStateError::InvalidRecord)?,
            );
            Some((signature, key_epoch, tribute_offer_epoch))
        }
        _ => return Err(ResidentStateError::InvalidRecord),
    };
    Ok(DecodedRecord {
        seed,
        challenge,
        material,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink, os::unix::fs::PermissionsExt as _};

    #[test]
    fn resident_identity_and_active_offer_survive_restart() {
        let directory = tempfile::tempdir().unwrap();
        let chain_id = B256::repeat_byte(3);
        let (store, first) =
            ResidentStateStore::load_or_create(directory.path(), chain_id).unwrap();
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
        let initial_public = first.keys.noise_public();
        let challenge = first.initialization_challenge;
        let material = ResidentOfferMaterial::for_test(vec![7; 96], chain_id, 2, 4).unwrap();
        let offer_public = material.public();
        store
            .persist_active(&first.keys, challenge, &material)
            .unwrap();

        let (_, restored) = ResidentStateStore::load_or_create(directory.path(), chain_id).unwrap();
        assert_eq!(restored.keys.noise_public(), initial_public);
        assert_eq!(
            restored.keys.attestation_pub(),
            first.keys.attestation_pub()
        );
        assert_eq!(
            restored.keys.tribute_offer_public(),
            first.keys.tribute_offer_public()
        );
        assert_eq!(restored.initialization_challenge, challenge);
        let restored_offer = restored.material.unwrap();
        assert_eq!(restored_offer.public(), offer_public);
        assert_eq!(
            restored_offer.group_sig.as_slice(),
            material.group_sig.as_slice()
        );
        assert_eq!(restored_offer.key_epoch(), 2);
        assert_eq!(restored_offer.tribute_offer_epoch(), 4);
    }

    #[test]
    fn resident_file_rejects_corruption_and_public_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let chain_id = B256::repeat_byte(3);
        ResidentStateStore::load_or_create(directory.path(), chain_id).unwrap();
        let paths = super::super::NodeHostPaths::new(directory.path());
        fs::set_permissions(&paths.resident_state, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ResidentStateStore::load_or_create(directory.path(), chain_id).is_err());
        fs::set_permissions(&paths.resident_state, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&paths.resident_state, b"broken").unwrap();
        assert!(matches!(
            ResidentStateStore::load_or_create(directory.path(), chain_id),
            Err(ResidentStateError::InvalidRecord)
        ));
        fs::remove_file(&paths.resident_state).unwrap();
        symlink(directory.path().join("absent"), &paths.resident_state).unwrap();
        assert!(ResidentStateStore::load_or_create(directory.path(), chain_id).is_err());
    }

    #[test]
    fn finalized_successor_replaces_resident_file() {
        let directory = tempfile::tempdir().unwrap();
        let chain_id = B256::repeat_byte(3);
        let (_, original) = ResidentStateStore::load_or_create(directory.path(), chain_id).unwrap();
        let successor = ResidentTeeKeys::generate().unwrap();
        let successor_public = successor.noise_public();
        let challenge = [5; 32];
        let material = ResidentOfferMaterial::for_test(vec![9; 96], chain_id, 5, 6).unwrap();
        let offer_public = material.public();
        ResidentStateStore::replace_promoted(directory.path(), &successor, challenge, &material)
            .unwrap();

        let (_, restored) = ResidentStateStore::load_or_create(directory.path(), chain_id).unwrap();
        assert_ne!(restored.keys.noise_public(), original.keys.noise_public());
        assert_eq!(restored.keys.noise_public(), successor_public);
        assert_eq!(restored.initialization_challenge, challenge);
        assert_eq!(restored.material.unwrap().public(), offer_public);
    }
}
