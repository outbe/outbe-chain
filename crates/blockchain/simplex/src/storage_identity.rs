use crate::errors::StorageIdentityError;
use alloy_primitives::B256;
use outbe_primitives::chain::MAINNET_CHAIN_ID;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    path::Path,
};

pub const CONSENSUS_IDENTITY_FILE: &str = "outbe-consensus-identity-v1.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConsensusStorageIdentityRecordV1 {
    version: u8,
    chain_id: u64,
    genesis_hash: B256,
}

pub fn bind_consensus_storage_identity(
    storage_dir: &Path,
    chain_id: u64,
    genesis_hash: B256,
) -> eyre::Result<()> {
    if chain_id != MAINNET_CHAIN_ID {
        return Ok(());
    }

    ensure_real_directory(storage_dir)?;
    let marker_path = storage_dir.join(CONSENSUS_IDENTITY_FILE);
    if marker_path.exists() {
        return validate_existing_marker(&marker_path, chain_id, genesis_hash);
    }
    if fs::read_dir(storage_dir)?.next().transpose()?.is_some() {
        return Err(StorageIdentityError::NonemptyWithoutMarker {
            storage_dir: storage_dir.display().to_string(),
            marker_file: CONSENSUS_IDENTITY_FILE,
        }
        .into());
    }

    let identity = ConsensusStorageIdentityRecordV1 {
        version: 1,
        chain_id,
        genesis_hash,
    };
    let mut encoded = serde_json::to_vec(&identity)?;
    encoded.push(b'\n');
    let temporary_path = storage_dir.join(format!(".{CONSENSUS_IDENTITY_FILE}.tmp"));
    let mut temporary = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_path)?;
    if let Err(error) = temporary
        .write_all(&encoded)
        .and_then(|()| temporary.sync_all())
        .and_then(|()| fs::rename(&temporary_path, &marker_path))
    {
        let _ = fs::remove_file(&temporary_path);
        return Err(error.into());
    }
    validate_existing_marker(&marker_path, chain_id, genesis_hash)
}

fn ensure_real_directory(storage_dir: &Path) -> eyre::Result<()> {
    match fs::symlink_metadata(storage_dir) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(())
        }
        Ok(_) => {
            Err(StorageIdentityError::NotRealDirectory(storage_dir.display().to_string()).into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(storage_dir)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn validate_existing_marker(
    marker_path: &Path,
    chain_id: u64,
    genesis_hash: B256,
) -> eyre::Result<()> {
    let metadata = fs::symlink_metadata(marker_path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(
            StorageIdentityError::MarkerNotRegularFile(marker_path.display().to_string()).into(),
        );
    }
    let identity: ConsensusStorageIdentityRecordV1 =
        serde_json::from_slice(&fs::read(marker_path)?)?;
    let expected = ConsensusStorageIdentityRecordV1 {
        version: 1,
        chain_id,
        genesis_hash,
    };
    if identity != expected {
        return Err(StorageIdentityError::IdentityMismatch {
            marker_path: marker_path.display().to_string(),
            expected_chain_id: expected.chain_id,
            expected_genesis_hash: expected.genesis_hash,
            actual_chain_id: identity.chain_id,
            actual_genesis_hash: identity.genesis_hash,
        }
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const GENESIS: B256 = B256::repeat_byte(0x11);

    #[test]
    fn mainnet_fresh_storage_is_bound_and_reopens_exactly() {
        let dir = tempfile::tempdir().unwrap();
        bind_consensus_storage_identity(dir.path(), MAINNET_CHAIN_ID, GENESIS).unwrap();

        let marker = fs::read_to_string(dir.path().join(CONSENSUS_IDENTITY_FILE)).unwrap();
        assert_eq!(
            serde_json::from_str::<ConsensusStorageIdentityRecordV1>(&marker).unwrap(),
            ConsensusStorageIdentityRecordV1 {
                version: 1,
                chain_id: MAINNET_CHAIN_ID,
                genesis_hash: GENESIS,
            }
        );
        bind_consensus_storage_identity(dir.path(), MAINNET_CHAIN_ID, GENESIS).unwrap();
    }

    #[test]
    fn mainnet_rejects_wrong_or_malformed_storage_identity() {
        for marker in [
            r#"{"version":1,"chainId":54322345,"genesisHash":"0x1111111111111111111111111111111111111111111111111111111111111111"}"#,
            r#"{"version":1,"chainId":676,"genesisHash":"0x2222222222222222222222222222222222222222222222222222222222222222"}"#,
            "not-json",
        ] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join(CONSENSUS_IDENTITY_FILE), marker).unwrap();
            assert!(
                bind_consensus_storage_identity(dir.path(), MAINNET_CHAIN_ID, GENESIS).is_err()
            );
        }
    }

    #[test]
    fn mainnet_rejects_nonempty_storage_without_an_identity_marker() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("outbe-simplex-0"), b"legacy").unwrap();
        assert!(bind_consensus_storage_identity(dir.path(), MAINNET_CHAIN_ID, GENESIS).is_err());
    }

    #[test]
    fn devnet_and_testnet_storage_behavior_is_unchanged() {
        for chain_id in [
            outbe_primitives::chain::DEVNET_CHAIN_ID,
            outbe_primitives::chain::TESTNET_CHAIN_ID,
        ] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("existing"), b"legacy").unwrap();
            bind_consensus_storage_identity(dir.path(), chain_id, GENESIS).unwrap();
            assert!(!dir.path().join(CONSENSUS_IDENTITY_FILE).exists());
        }
    }
}
