//! One durable contiguous watermark for the embedded OCOMP ExEx.
//!
//! This is intentionally not a job journal. Per-job work is reconstructed from
//! canonical notifications and the existing retention/CAS records.

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
};

use alloy_primitives::B256;
use thiserror::Error;

const MAGIC: [u8; 8] = *b"OUTBEXX1";
const VERSION: u16 = 1;
const FILE_NAME: &str = "checkpoint-v1.bin";
const PENDING_NAME: &str = "checkpoint-v1.pending";
const RECORD_BYTES: usize = 8 + 2 + 8 + 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OcompExExCheckpointV1 {
    pub block_number: u64,
    pub block_hash: B256,
}

pub struct OcompExExCheckpointStoreV1 {
    root: PathBuf,
    current: Option<OcompExExCheckpointV1>,
}

impl OcompExExCheckpointStoreV1 {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, OcompExExCheckpointErrorV1> {
        let root = root.as_ref().to_path_buf();
        match fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            }
            Ok(_) => return Err(OcompExExCheckpointErrorV1::UnsafeRoot),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = DirBuilder::new();
                builder
                    .mode(0o700)
                    .recursive(true)
                    .create(&root)
                    .map_err(|source| OcompExExCheckpointErrorV1::Io {
                        operation: "create checkpoint root",
                        path: root.clone(),
                        source,
                    })?;
                sync_directory(&root)?;
            }
            Err(source) => {
                return Err(OcompExExCheckpointErrorV1::Io {
                    operation: "stat checkpoint root",
                    path: root,
                    source,
                });
            }
        }
        let pending = root.join(PENDING_NAME);
        if pending.exists() {
            return Err(OcompExExCheckpointErrorV1::AmbiguousPending);
        }
        let path = root.join(FILE_NAME);
        let current = if path.exists() {
            Some(decode(&read_exact_record(&path)?)?)
        } else {
            None
        };
        Ok(Self { root, current })
    }

    #[must_use]
    pub const fn load(&self) -> Option<OcompExExCheckpointV1> {
        self.current
    }

    pub fn persist(
        &mut self,
        checkpoint: OcompExExCheckpointV1,
    ) -> Result<(), OcompExExCheckpointErrorV1> {
        if checkpoint.block_hash.is_zero() {
            return Err(OcompExExCheckpointErrorV1::InvalidCheckpoint);
        }
        if let Some(current) = self.current {
            if current == checkpoint {
                return Ok(());
            }
            if checkpoint.block_number <= current.block_number {
                return Err(OcompExExCheckpointErrorV1::NonMonotonic);
            }
        }
        let pending = self.root.join(PENDING_NAME);
        let final_path = self.root.join(FILE_NAME);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&pending)
            .map_err(|source| OcompExExCheckpointErrorV1::Io {
                operation: "create pending checkpoint",
                path: pending.clone(),
                source,
            })?;
        file.write_all(&encode(checkpoint))
            .map_err(|source| OcompExExCheckpointErrorV1::Io {
                operation: "write pending checkpoint",
                path: pending.clone(),
                source,
            })?;
        file.sync_all()
            .map_err(|source| OcompExExCheckpointErrorV1::Io {
                operation: "sync pending checkpoint",
                path: pending.clone(),
                source,
            })?;
        drop(file);
        fs::rename(&pending, &final_path).map_err(|source| OcompExExCheckpointErrorV1::Io {
            operation: "install checkpoint",
            path: final_path,
            source,
        })?;
        sync_directory(&self.root)?;
        self.current = Some(checkpoint);
        Ok(())
    }
}

fn encode(checkpoint: OcompExExCheckpointV1) -> [u8; RECORD_BYTES] {
    let mut bytes = [0_u8; RECORD_BYTES];
    bytes[..8].copy_from_slice(&MAGIC);
    bytes[8..10].copy_from_slice(&VERSION.to_be_bytes());
    bytes[10..18].copy_from_slice(&checkpoint.block_number.to_be_bytes());
    bytes[18..].copy_from_slice(checkpoint.block_hash.as_slice());
    bytes
}

fn decode(bytes: &[u8; RECORD_BYTES]) -> Result<OcompExExCheckpointV1, OcompExExCheckpointErrorV1> {
    let mut version = [0_u8; 2];
    version.copy_from_slice(&bytes[8..10]);
    if bytes[..8] != MAGIC || u16::from_be_bytes(version) != VERSION {
        return Err(OcompExExCheckpointErrorV1::Malformed);
    }
    let mut block_number = [0_u8; 8];
    block_number.copy_from_slice(&bytes[10..18]);
    let checkpoint = OcompExExCheckpointV1 {
        block_number: u64::from_be_bytes(block_number),
        block_hash: B256::from_slice(&bytes[18..]),
    };
    if checkpoint.block_hash.is_zero() {
        return Err(OcompExExCheckpointErrorV1::Malformed);
    }
    Ok(checkpoint)
}

fn read_exact_record(path: &Path) -> Result<[u8; RECORD_BYTES], OcompExExCheckpointErrorV1> {
    let metadata = fs::symlink_metadata(path).map_err(|source| OcompExExCheckpointErrorV1::Io {
        operation: "stat checkpoint",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() || metadata.len() != RECORD_BYTES as u64 {
        return Err(OcompExExCheckpointErrorV1::Malformed);
    }
    let mut bytes = [0_u8; RECORD_BYTES];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|source| OcompExExCheckpointErrorV1::Io {
            operation: "read checkpoint",
            path: path.to_path_buf(),
            source,
        })?;
    Ok(bytes)
}

fn sync_directory(path: &Path) -> Result<(), OcompExExCheckpointErrorV1> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| OcompExExCheckpointErrorV1::Io {
            operation: "sync checkpoint directory",
            path: path.to_path_buf(),
            source,
        })
}

#[derive(Debug, Error)]
pub enum OcompExExCheckpointErrorV1 {
    #[error("OCOMP ExEx checkpoint root is unsafe")]
    UnsafeRoot,
    #[error("OCOMP ExEx checkpoint has an ambiguous pending write")]
    AmbiguousPending,
    #[error("OCOMP ExEx checkpoint is malformed")]
    Malformed,
    #[error("OCOMP ExEx checkpoint is invalid")]
    InvalidCheckpoint,
    #[error("OCOMP ExEx checkpoint cannot regress or change hash at one height")]
    NonMonotonic,
    #[error("OCOMP ExEx checkpoint I/O failed during {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
