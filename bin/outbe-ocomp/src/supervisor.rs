//! Durable finalized-job journal owned by the OCOMP supervisor.
//!
//! Network discovery lives in `rpc_discovery`: this module deliberately owns
//! only the restart-safe local record and contains no node transport.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use alloy_primitives::{keccak256, B256};
use outbe_ocomp_protocol::{FinalizedJobSpecV1, ProtocolError, SchemaLimits};
use thiserror::Error;

use crate::control::EndpointIdentity;

const JOURNAL_MAGIC: [u8; 8] = *b"OUTBDIS1";
const JOURNAL_VERSION: u16 = 1;
const JOURNAL_FILE: &str = "discovery.v1";
const JOURNAL_TEMP_FILE: &str = "discovery.v1.tmp";
const JOURNAL_LOCK_FILE: &str = "discovery.lock";
const JOURNAL_FIXED_BYTES: usize = 8 + 2 + 8 + 8 + 4 + 32;
const SCAN_CHECKPOINT_MAGIC: [u8; 8] = *b"OUTBSCN1";
const SCAN_CHECKPOINT_VERSION: u16 = 1;
const SCAN_CHECKPOINT_FILE: &str = "scan-checkpoint.v1";
const SCAN_CHECKPOINT_TEMP_FILE: &str = "scan-checkpoint.v1.tmp";
const SCAN_CHECKPOINT_BYTES: usize = 8 + 2 + 8 + 8 + 32 + 32 + 32 + 32 + 8 + 32 + 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryRecord {
    pub generation: u64,
    pub cursor: u64,
    pub spec: FinalizedJobSpecV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScanCheckpointV1 {
    pub generation: u64,
    pub identity: EndpointIdentity,
    pub fork_id: B256,
    pub scanned_through_height: u64,
    pub scanned_through_block_hash: B256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryOutcome {
    NoNewJob,
    Discovered(Box<DiscoveryRecord>),
}

pub(crate) struct DiscoveryJournal {
    root: PathBuf,
    limits: SchemaLimits,
    record: Option<DiscoveryRecord>,
    scan_checkpoint: Option<ScanCheckpointV1>,
    _lock: JournalLock,
}

impl DiscoveryJournal {
    pub(crate) fn open(
        root: &Path,
        limits: SchemaLimits,
    ) -> Result<Self, SupervisorDiscoveryError> {
        create_private_directory(root)?;
        let lock = JournalLock::acquire(root)?;
        let temp = root.join(JOURNAL_TEMP_FILE);
        if path_exists(&temp)? {
            return Err(SupervisorDiscoveryError::AmbiguousJournal(temp));
        }
        let scan_temp = root.join(SCAN_CHECKPOINT_TEMP_FILE);
        if path_exists(&scan_temp)? {
            return Err(SupervisorDiscoveryError::AmbiguousJournal(scan_temp));
        }
        let path = root.join(JOURNAL_FILE);
        let record = if path_exists(&path)? {
            Some(read_record(&path, &limits)?)
        } else {
            None
        };
        let scan_path = root.join(SCAN_CHECKPOINT_FILE);
        let scan_checkpoint = if path_exists(&scan_path)? {
            Some(read_scan_checkpoint(&scan_path)?)
        } else {
            None
        };
        Ok(Self {
            root: root.to_path_buf(),
            limits,
            record,
            scan_checkpoint,
            _lock: lock,
        })
    }

    pub(crate) const fn record(&self) -> Option<&DiscoveryRecord> {
        self.record.as_ref()
    }

    pub(crate) const fn scan_checkpoint(&self) -> Option<&ScanCheckpointV1> {
        self.scan_checkpoint.as_ref()
    }

    pub(crate) fn persist(
        &mut self,
        spec: FinalizedJobSpecV1,
    ) -> Result<DiscoveryRecord, SupervisorDiscoveryError> {
        if let Some(existing) = &self.record {
            if existing.spec == spec {
                return Ok(existing.clone());
            }
            if spec.summary.cursor <= existing.cursor {
                return Err(SupervisorDiscoveryError::NonMonotonicCursor {
                    before: existing.cursor,
                    after: spec.summary.cursor,
                });
            }
        }
        let generation = self.record.as_ref().map_or(Ok(1), |record| {
            record
                .generation
                .checked_add(1)
                .ok_or(SupervisorDiscoveryError::JournalGenerationOverflow)
        })?;
        let record = DiscoveryRecord {
            generation,
            cursor: spec.summary.cursor,
            spec,
        };
        let encoded = encode_record(&record, &self.limits)?;
        let temp = self.root.join(JOURNAL_TEMP_FILE);
        let final_path = self.root.join(JOURNAL_FILE);
        let result = publish_record(&temp, &final_path, &self.root, &encoded);
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result?;
        self.record = Some(record.clone());
        Ok(record)
    }

    pub(crate) fn persist_scan_checkpoint(
        &mut self,
        identity: EndpointIdentity,
        fork_id: B256,
        scanned_through_height: u64,
        scanned_through_block_hash: B256,
    ) -> Result<ScanCheckpointV1, SupervisorDiscoveryError> {
        if let Some(existing) = self.scan_checkpoint {
            if existing.identity != identity || existing.fork_id != fork_id {
                return Err(SupervisorDiscoveryError::ScanCheckpointIdentityMismatch);
            }
            if scanned_through_height < existing.scanned_through_height {
                return Err(SupervisorDiscoveryError::NonMonotonicScanCheckpoint {
                    before: existing.scanned_through_height,
                    after: scanned_through_height,
                });
            }
            if scanned_through_height == existing.scanned_through_height {
                if scanned_through_block_hash == existing.scanned_through_block_hash {
                    return Ok(existing);
                }
                return Err(SupervisorDiscoveryError::ScanCheckpointConflict {
                    height: scanned_through_height,
                });
            }
        }
        let generation = self.scan_checkpoint.map_or(Ok(1), |checkpoint| {
            checkpoint
                .generation
                .checked_add(1)
                .ok_or(SupervisorDiscoveryError::JournalGenerationOverflow)
        })?;
        let checkpoint = ScanCheckpointV1 {
            generation,
            identity,
            fork_id,
            scanned_through_height,
            scanned_through_block_hash,
        };
        let encoded = encode_scan_checkpoint(checkpoint)?;
        let temp = self.root.join(SCAN_CHECKPOINT_TEMP_FILE);
        let final_path = self.root.join(SCAN_CHECKPOINT_FILE);
        let result = publish_record(&temp, &final_path, &self.root, &encoded);
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result?;
        self.scan_checkpoint = Some(checkpoint);
        Ok(checkpoint)
    }
}

fn encode_scan_checkpoint(
    checkpoint: ScanCheckpointV1,
) -> Result<Vec<u8>, SupervisorDiscoveryError> {
    if checkpoint.generation == 0
        || checkpoint.identity.chain_id == 0
        || checkpoint.identity.genesis_hash.is_zero()
        || checkpoint.identity.boot_nonce.is_zero()
        || checkpoint.identity.protocol_bundle_hash.is_zero()
        || checkpoint.fork_id.is_zero()
        || checkpoint.scanned_through_height == 0
        || checkpoint.scanned_through_block_hash.is_zero()
    {
        return Err(SupervisorDiscoveryError::InvalidScanCheckpoint);
    }
    let mut encoded = Vec::with_capacity(SCAN_CHECKPOINT_BYTES);
    encoded.extend_from_slice(&SCAN_CHECKPOINT_MAGIC);
    encoded.extend_from_slice(&SCAN_CHECKPOINT_VERSION.to_be_bytes());
    encoded.extend_from_slice(&checkpoint.generation.to_be_bytes());
    encoded.extend_from_slice(&checkpoint.identity.chain_id.to_be_bytes());
    encoded.extend_from_slice(checkpoint.identity.genesis_hash.as_slice());
    encoded.extend_from_slice(checkpoint.identity.boot_nonce.as_slice());
    encoded.extend_from_slice(checkpoint.identity.protocol_bundle_hash.as_slice());
    encoded.extend_from_slice(checkpoint.fork_id.as_slice());
    encoded.extend_from_slice(&checkpoint.scanned_through_height.to_be_bytes());
    encoded.extend_from_slice(checkpoint.scanned_through_block_hash.as_slice());
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    debug_assert_eq!(encoded.len(), SCAN_CHECKPOINT_BYTES);
    Ok(encoded)
}

fn read_scan_checkpoint(path: &Path) -> Result<ScanCheckpointV1, SupervisorDiscoveryError> {
    let mut file = open_regular_nofollow(path)?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("stat discovery scan checkpoint", path, source))?;
    if usize::try_from(metadata.len()).ok() != Some(SCAN_CHECKPOINT_BYTES) {
        return Err(SupervisorDiscoveryError::InvalidScanCheckpoint);
    }
    let mut encoded = Vec::with_capacity(SCAN_CHECKPOINT_BYTES);
    file.read_to_end(&mut encoded)
        .map_err(|source| io_error("read discovery scan checkpoint", path, source))?;
    decode_scan_checkpoint(&encoded)
}

fn decode_scan_checkpoint(encoded: &[u8]) -> Result<ScanCheckpointV1, SupervisorDiscoveryError> {
    if encoded.len() != SCAN_CHECKPOINT_BYTES || encoded[..8] != SCAN_CHECKPOINT_MAGIC {
        return Err(SupervisorDiscoveryError::InvalidScanCheckpoint);
    }
    let version = u16::from_be_bytes(
        encoded[8..10]
            .try_into()
            .map_err(|_| SupervisorDiscoveryError::InvalidScanCheckpoint)?,
    );
    if version != SCAN_CHECKPOINT_VERSION {
        return Err(SupervisorDiscoveryError::UnsupportedScanCheckpointVersion(
            version,
        ));
    }
    let checksum_offset = SCAN_CHECKPOINT_BYTES - 32;
    let expected_checksum = B256::from_slice(&encoded[checksum_offset..]);
    if keccak256(&encoded[..checksum_offset]) != expected_checksum {
        return Err(SupervisorDiscoveryError::ScanCheckpointChecksumMismatch);
    }
    let checkpoint = ScanCheckpointV1 {
        generation: read_u64(encoded, 10)?,
        identity: EndpointIdentity {
            chain_id: read_u64(encoded, 18)?,
            genesis_hash: B256::from_slice(&encoded[26..58]),
            boot_nonce: B256::from_slice(&encoded[58..90]),
            protocol_bundle_hash: B256::from_slice(&encoded[90..122]),
        },
        fork_id: B256::from_slice(&encoded[122..154]),
        scanned_through_height: read_u64(encoded, 154)?,
        scanned_through_block_hash: B256::from_slice(&encoded[162..194]),
    };
    if encode_scan_checkpoint(checkpoint)? != encoded {
        return Err(SupervisorDiscoveryError::InvalidScanCheckpoint);
    }
    Ok(checkpoint)
}

fn encode_record(
    record: &DiscoveryRecord,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, SupervisorDiscoveryError> {
    if record.generation == 0 || record.cursor != record.spec.summary.cursor {
        return Err(SupervisorDiscoveryError::InvalidJournal);
    }
    let spec = record.spec.encode_body(limits)?;
    let spec_len =
        u32::try_from(spec.len()).map_err(|_| SupervisorDiscoveryError::JournalTooLarge)?;
    let mut encoded = Vec::with_capacity(JOURNAL_FIXED_BYTES + spec.len());
    encoded.extend_from_slice(&JOURNAL_MAGIC);
    encoded.extend_from_slice(&JOURNAL_VERSION.to_be_bytes());
    encoded.extend_from_slice(&record.generation.to_be_bytes());
    encoded.extend_from_slice(&record.cursor.to_be_bytes());
    encoded.extend_from_slice(&spec_len.to_be_bytes());
    encoded.extend_from_slice(&spec);
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    Ok(encoded)
}

fn read_record(
    path: &Path,
    limits: &SchemaLimits,
) -> Result<DiscoveryRecord, SupervisorDiscoveryError> {
    let mut file = open_regular_nofollow(path)?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("stat discovery journal", path, source))?;
    let max_len = JOURNAL_FIXED_BYTES
        .checked_add(limits.max_control_body_bytes)
        .ok_or(SupervisorDiscoveryError::JournalTooLarge)?;
    let file_len =
        usize::try_from(metadata.len()).map_err(|_| SupervisorDiscoveryError::JournalTooLarge)?;
    if file_len < JOURNAL_FIXED_BYTES || file_len > max_len {
        return Err(SupervisorDiscoveryError::JournalTooLarge);
    }
    let mut encoded = Vec::with_capacity(file_len);
    file.read_to_end(&mut encoded)
        .map_err(|source| io_error("read discovery journal", path, source))?;
    if encoded.len() != file_len {
        return Err(SupervisorDiscoveryError::InvalidJournal);
    }
    decode_record(&encoded, limits)
}

fn decode_record(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<DiscoveryRecord, SupervisorDiscoveryError> {
    if encoded.len() < JOURNAL_FIXED_BYTES || encoded[..8] != JOURNAL_MAGIC {
        return Err(SupervisorDiscoveryError::InvalidJournal);
    }
    let version = u16::from_be_bytes(
        encoded[8..10]
            .try_into()
            .map_err(|_| SupervisorDiscoveryError::InvalidJournal)?,
    );
    if version != JOURNAL_VERSION {
        return Err(SupervisorDiscoveryError::UnsupportedJournalVersion(version));
    }
    let generation = read_u64(encoded, 10)?;
    let cursor = read_u64(encoded, 18)?;
    let spec_len = usize::try_from(read_u32(encoded, 26)?)
        .map_err(|_| SupervisorDiscoveryError::JournalTooLarge)?;
    let spec_end = 30_usize
        .checked_add(spec_len)
        .ok_or(SupervisorDiscoveryError::JournalTooLarge)?;
    let expected_len = spec_end
        .checked_add(32)
        .ok_or(SupervisorDiscoveryError::JournalTooLarge)?;
    if expected_len != encoded.len() || spec_len > limits.max_control_body_bytes {
        return Err(SupervisorDiscoveryError::InvalidJournal);
    }
    let expected_checksum = B256::from_slice(
        encoded
            .get(spec_end..)
            .ok_or(SupervisorDiscoveryError::InvalidJournal)?,
    );
    if keccak256(&encoded[..spec_end]) != expected_checksum {
        return Err(SupervisorDiscoveryError::JournalChecksumMismatch);
    }
    let spec = FinalizedJobSpecV1::decode_body(&encoded[30..spec_end], limits)?;
    if generation == 0 || cursor != spec.summary.cursor {
        return Err(SupervisorDiscoveryError::InvalidJournal);
    }
    Ok(DiscoveryRecord {
        generation,
        cursor,
        spec,
    })
}

fn read_u64(encoded: &[u8], start: usize) -> Result<u64, SupervisorDiscoveryError> {
    Ok(u64::from_be_bytes(
        encoded
            .get(start..start + 8)
            .ok_or(SupervisorDiscoveryError::InvalidJournal)?
            .try_into()
            .map_err(|_| SupervisorDiscoveryError::InvalidJournal)?,
    ))
}

fn read_u32(encoded: &[u8], start: usize) -> Result<u32, SupervisorDiscoveryError> {
    Ok(u32::from_be_bytes(
        encoded
            .get(start..start + 4)
            .ok_or(SupervisorDiscoveryError::InvalidJournal)?
            .try_into()
            .map_err(|_| SupervisorDiscoveryError::InvalidJournal)?,
    ))
}

fn publish_record(
    temp: &Path,
    final_path: &Path,
    root: &Path,
    encoded: &[u8],
) -> Result<(), SupervisorDiscoveryError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(temp)
        .map_err(|source| io_error("create discovery journal temp", temp, source))?;
    file.write_all(encoded)
        .map_err(|source| io_error("write discovery journal temp", temp, source))?;
    file.sync_all()
        .map_err(|source| io_error("fsync discovery journal temp", temp, source))?;
    fs::rename(temp, final_path)
        .map_err(|source| io_error("publish discovery journal", final_path, source))?;
    sync_directory(root)
}

fn create_private_directory(path: &Path) -> Result<(), SupervisorDiscoveryError> {
    fs::create_dir_all(path)
        .map_err(|source| io_error("create discovery journal root", path, source))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("stat discovery journal root", path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(SupervisorDiscoveryError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

fn path_exists(path: &Path) -> Result<bool, SupervisorDiscoveryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            Ok(true)
        }
        Ok(_) => Err(SupervisorDiscoveryError::UnsafePath(path.to_path_buf())),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("stat discovery journal path", path, source)),
    }
}

fn open_regular_nofollow(path: &Path) -> Result<File, SupervisorDiscoveryError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| io_error("open discovery journal", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("stat discovery journal", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(SupervisorDiscoveryError::UnsafePath(path.to_path_buf()));
    }
    Ok(file)
}

fn sync_directory(path: &Path) -> Result<(), SupervisorDiscoveryError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("fsync discovery journal root", path, source))
}

struct JournalLock {
    file: File,
}

impl JournalLock {
    #[allow(unsafe_code)]
    fn acquire(root: &Path) -> Result<Self, SupervisorDiscoveryError> {
        let path = root.join(JOURNAL_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|source| io_error("open discovery journal lock", &path, source))?;
        // SAFETY: `file` owns a live descriptor for the complete flock call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io_error(
                "lock discovery journal",
                &path,
                std::io::Error::last_os_error(),
            ));
        }
        Ok(Self { file })
    }
}

impl Drop for JournalLock {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `self.file` is still open for the complete flock call.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn io_error(
    operation: &'static str,
    path: &Path,
    source: std::io::Error,
) -> SupervisorDiscoveryError {
    SupervisorDiscoveryError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Error)]
pub enum SupervisorDiscoveryError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("supervisor discovery I/O while trying to {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("non-monotonic finalized cursor {after} after {before}")]
    NonMonotonicCursor { before: u64, after: u64 },
    #[error("non-monotonic finalized scan checkpoint {after} after {before}")]
    NonMonotonicScanCheckpoint { before: u64, after: u64 },
    #[error("finalized scan checkpoint conflicts at height {height}")]
    ScanCheckpointConflict { height: u64 },
    #[error("finalized scan checkpoint identity differs from the active endpoint")]
    ScanCheckpointIdentityMismatch,
    #[error("supervisor discovery journal has an ambiguous temporary file at {0}")]
    AmbiguousJournal(PathBuf),
    #[error("supervisor discovery journal path is not a safe regular file/directory: {0}")]
    UnsafePath(PathBuf),
    #[error("supervisor discovery journal is malformed")]
    InvalidJournal,
    #[error("supervisor discovery journal checksum does not match its contents")]
    JournalChecksumMismatch,
    #[error("supervisor discovery scan checkpoint is malformed")]
    InvalidScanCheckpoint,
    #[error("supervisor discovery scan checkpoint checksum does not match its contents")]
    ScanCheckpointChecksumMismatch,
    #[error("unsupported supervisor discovery journal version {0}")]
    UnsupportedJournalVersion(u16),
    #[error("unsupported supervisor discovery scan checkpoint version {0}")]
    UnsupportedScanCheckpointVersion(u16),
    #[error("supervisor discovery journal exceeds the protocol bound")]
    JournalTooLarge,
    #[error("supervisor discovery journal generation overflow")]
    JournalGenerationOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_ocomp_protocol::profile::poc_schema_limits;

    fn identity(marker: u8) -> EndpointIdentity {
        EndpointIdentity {
            chain_id: u64::from(marker),
            genesis_hash: B256::repeat_byte(marker),
            boot_nonce: B256::repeat_byte(marker.wrapping_add(1)),
            protocol_bundle_hash: B256::repeat_byte(marker.wrapping_add(2)),
        }
    }

    #[test]
    fn scan_checkpoint_is_atomic_monotonic_and_restart_safe() {
        let root = tempfile::tempdir().unwrap();
        let expected = ScanCheckpointV1 {
            generation: 1,
            identity: identity(41),
            fork_id: B256::repeat_byte(44),
            scanned_through_height: 100,
            scanned_through_block_hash: B256::repeat_byte(45),
        };
        {
            let mut journal = DiscoveryJournal::open(root.path(), poc_schema_limits()).unwrap();
            assert!(journal.scan_checkpoint().is_none());
            assert_eq!(
                journal
                    .persist_scan_checkpoint(
                        expected.identity,
                        expected.fork_id,
                        expected.scanned_through_height,
                        expected.scanned_through_block_hash,
                    )
                    .unwrap(),
                expected
            );
            assert_eq!(journal.scan_checkpoint(), Some(&expected));
        }
        let journal = DiscoveryJournal::open(root.path(), poc_schema_limits()).unwrap();
        assert_eq!(journal.scan_checkpoint(), Some(&expected));
        assert!(!root.path().join(SCAN_CHECKPOINT_TEMP_FILE).exists());
    }

    #[test]
    fn scan_checkpoint_rejects_regression_conflict_and_identity_change() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = DiscoveryJournal::open(root.path(), poc_schema_limits()).unwrap();
        let endpoint = identity(41);
        let fork_id = B256::repeat_byte(44);
        journal
            .persist_scan_checkpoint(endpoint, fork_id, 100, B256::repeat_byte(45))
            .unwrap();
        assert!(matches!(
            journal.persist_scan_checkpoint(endpoint, fork_id, 99, B256::repeat_byte(46)),
            Err(SupervisorDiscoveryError::NonMonotonicScanCheckpoint { .. })
        ));
        assert!(matches!(
            journal.persist_scan_checkpoint(endpoint, fork_id, 100, B256::repeat_byte(46)),
            Err(SupervisorDiscoveryError::ScanCheckpointConflict { height: 100 })
        ));
        assert!(matches!(
            journal.persist_scan_checkpoint(identity(51), fork_id, 101, B256::repeat_byte(47)),
            Err(SupervisorDiscoveryError::ScanCheckpointIdentityMismatch)
        ));
    }

    #[test]
    fn scan_checkpoint_rejects_corruption_and_ambiguous_temp() {
        let root = tempfile::tempdir().unwrap();
        {
            let mut journal = DiscoveryJournal::open(root.path(), poc_schema_limits()).unwrap();
            journal
                .persist_scan_checkpoint(
                    identity(41),
                    B256::repeat_byte(44),
                    100,
                    B256::repeat_byte(45),
                )
                .unwrap();
        }
        let path = root.path().join(SCAN_CHECKPOINT_FILE);
        let mut bytes = fs::read(&path).unwrap();
        bytes[26] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            DiscoveryJournal::open(root.path(), poc_schema_limits()),
            Err(SupervisorDiscoveryError::ScanCheckpointChecksumMismatch)
        ));

        fs::remove_file(path).unwrap();
        fs::write(root.path().join(SCAN_CHECKPOINT_TEMP_FILE), b"partial").unwrap();
        assert!(matches!(
            DiscoveryJournal::open(root.path(), poc_schema_limits()),
            Err(SupervisorDiscoveryError::AmbiguousJournal(_))
        ));
    }
}
