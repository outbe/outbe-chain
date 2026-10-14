use std::{
    fs::{self, File, OpenOptions},
    os::{fd::AsRawFd as _, unix::fs::OpenOptionsExt as _},
    path::{Path, PathBuf},
};

use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    control::{FinalizedJobSpecV1, SnapshotExportCommittedV1},
    profile::ProtocolBundleV1,
    ProtocolError, SchemaLimits,
};
use outbe_primitives::projection::ProjectionCheckpoint;
use thiserror::Error;

use crate::discovery_control::{DiscoveryAckRefV1, DiscoveryControlError, DiscoveryOfferRefV1};
use crate::export_receipt::VerifiedExportReceipt;

mod checkpoint;
mod records;

#[cfg(test)]
use checkpoint::read_spool_bytes;
use checkpoint::{
    create_private_directory, inspect_private_directory, inspect_private_file,
    persist_immutable_atomic, recover_crash_temps, regular_file_exists,
};
pub use checkpoint::{
    inspect_closure_checkpoint, CheckpointAdvanceOutcomeV1, ClosureCheckpointInspectionV1,
    ContiguousCheckpointStoreV1,
};
pub use records::DiscoveryPendingCursorV1;
use records::{
    encode_ack_record, encode_offer_record, io_error, parse_category_observation,
    read_quarantine_record_at, verify_pending_marker_at,
};

const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const LOCK_FILE: &str = ".lock";
const TEMP_SUFFIX: &str = ".tmp";
const OFFER_SUFFIX: &str = ".offer";
const ACK_SUFFIX: &str = ".ack";
const PENDING_SUFFIX: &str = ".pending";
const QUARANTINE_SUFFIX: &str = ".quarantine";
const RETIREMENT_SUFFIX: &str = ".retirement";
const OFFER_RECORD_MAGIC: [u8; 8] = *b"OUTBDSO1";
const ACK_RECORD_MAGIC: [u8; 8] = *b"OUTBDSA2";
const PENDING_RECORD_MAGIC: [u8; 8] = *b"OUTBDSP1";
const QUARANTINE_MAGIC: [u8; 8] = *b"OUTBDSQ1";
const RETIREMENT_MAGIC: [u8; 8] = *b"OUTBDSR1";
const RECORD_VERSION: u16 = 1;
const RETIREMENT_FIXED_BYTES: usize = 8 + 2 + 8 + DiscoveryOfferRefV1::FIXED_BYTES + 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PutOutcomeV1 {
    Inserted,
    ExactDuplicate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AckPutOutcomeV1 {
    Committed {
        reference: DiscoveryAckRefV1,
        outcome: PutOutcomeV1,
    },
    Retired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingDiscoveryV1 {
    pub reference: DiscoveryOfferRefV1,
    pub spec: FinalizedJobSpecV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredDiscoveryAckV1 {
    pub reference: DiscoveryAckRefV1,
    pub committed: SnapshotExportCommittedV1,
    pub lease_generation: u64,
    pub manifest_hash: B256,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetirementReportV1 {
    pub completed: u64,
    pub waiting_for_checkpoint: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RetirementIntentV1 {
    closed_through: u64,
    reference: DiscoveryOfferRefV1,
}

/// One existing public spool record. Presence is a local lifecycle observation,
/// not proof that a canonical job still requires this historical record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoverySpoolRecordV1 {
    Offer(PendingDiscoveryV1),
    Ack(StoredDiscoveryAckV1),
    Pending {
        observation: B256,
    },
    Quarantine {
        observation: B256,
        reason_hash: B256,
    },
    Retirement {
        reference: DiscoveryOfferRefV1,
        closed_through: u64,
    },
}

/// Nonmutating inspection of an existing, stopped discovery spool.
/// The reader never opens the private codec owner through its runtime constructor
/// and never exposes it to callers. The reader invokes no locking, recovery or
/// mutation methods.
pub struct DiscoverySpoolReaderV1 {
    codec: DiscoverySpoolV1,
}

impl DiscoverySpoolReaderV1 {
    pub fn open_existing(
        root: impl AsRef<Path>,
        chain_id: u64,
        genesis_hash: B256,
        limits: SchemaLimits,
    ) -> Result<Self, DiscoverySpoolError> {
        if chain_id == 0 || genesis_hash.is_zero() {
            return Err(DiscoverySpoolError::InvalidIdentity);
        }
        let root = root.as_ref().to_path_buf();
        inspect_private_directory(&root)?;
        let codec = DiscoverySpoolV1 {
            offers: root.join("offers"),
            acks: root.join("acks"),
            pending: root.join("pending"),
            quarantine: root.join("quarantine"),
            retirements: root.join("retirements"),
            root,
            chain_id,
            genesis_hash,
            limits,
        };
        for directory in [
            &codec.offers,
            &codec.acks,
            &codec.pending,
            &codec.quarantine,
            &codec.retirements,
        ] {
            inspect_private_directory(directory)?;
        }
        let reader = Self { codec };
        reader.inspect_root()?;
        Ok(reader)
    }

    /// Streams complete bounded native records without collecting the spool.
    /// Directory order is unspecified. Callback failures stop the traversal.
    /// Observations are provisional until traversal succeeds. The traversal emits
    /// quarantine and retirement statuses without repairing or deleting anything.
    pub fn visit_records(
        &self,
        visitor: &mut impl FnMut(DiscoverySpoolRecordV1) -> Result<(), DiscoverySpoolError>,
    ) -> Result<(), DiscoverySpoolError> {
        self.inspect_root()?;
        for (directory, suffix) in [
            (&self.codec.offers, OFFER_SUFFIX),
            (&self.codec.acks, ACK_SUFFIX),
            (&self.codec.pending, PENDING_SUFFIX),
            (&self.codec.quarantine, QUARANTINE_SUFFIX),
            (&self.codec.retirements, RETIREMENT_SUFFIX),
        ] {
            self.visit_category(directory, suffix, visitor)?;
        }
        Ok(())
    }

    fn visit_category(
        &self,
        directory: &Path,
        suffix: &str,
        visitor: &mut impl FnMut(DiscoverySpoolRecordV1) -> Result<(), DiscoverySpoolError>,
    ) -> Result<(), DiscoverySpoolError> {
        inspect_private_directory(directory)?;
        let entries = fs::read_dir(directory)
            .map_err(|source| io_error("list read-only spool records", directory, source))?;
        for entry in entries {
            let path = entry
                .map_err(|source| io_error("read spool directory entry", directory, source))?
                .path();
            let observation = parse_category_observation(&path, suffix)?;
            visitor(self.decode_category_record(suffix, &path, observation)?)?;
        }
        Ok(())
    }

    fn decode_category_record(
        &self,
        suffix: &str,
        path: &Path,
        observation: B256,
    ) -> Result<DiscoverySpoolRecordV1, DiscoverySpoolError> {
        match suffix {
            OFFER_SUFFIX => Ok(DiscoverySpoolRecordV1::Offer(self.read_offer(observation)?)),
            ACK_SUFFIX => {
                let offer = self.read_offer(observation)?;
                Ok(DiscoverySpoolRecordV1::Ack(
                    self.codec.read_ack_for_offer(path, &offer)?,
                ))
            }
            PENDING_SUFFIX => {
                verify_pending_marker_at(path, observation)?;
                self.read_offer(observation)?;
                Ok(DiscoverySpoolRecordV1::Pending { observation })
            }
            QUARANTINE_SUFFIX => read_quarantine_record_at(path, observation),
            RETIREMENT_SUFFIX => self.read_retirement_record_at(path, observation),
            _ => unreachable!("fixed native record categories"),
        }
    }

    fn read_retirement_record_at(
        &self,
        path: &Path,
        observation: B256,
    ) -> Result<DiscoverySpoolRecordV1, DiscoverySpoolError> {
        let intent = self.codec.read_retirement_at(path)?;
        if intent.reference.observation_id != observation {
            return Err(DiscoverySpoolError::CorruptRecord {
                path: path.to_path_buf(),
            });
        }
        // Native retirement unlinks the offer before its tombstone.
        if regular_file_exists(&self.codec.offer_path(&observation))?
            && self.read_offer(observation)?.reference != intent.reference
        {
            return Err(DiscoverySpoolError::CorruptRecord {
                path: path.to_path_buf(),
            });
        }
        Ok(DiscoverySpoolRecordV1::Retirement {
            reference: intent.reference,
            closed_through: intent.closed_through,
        })
    }

    fn read_offer(&self, observation: B256) -> Result<PendingDiscoveryV1, DiscoverySpoolError> {
        let path = self.codec.offer_path(&observation);
        if !regular_file_exists(&path)? {
            return Err(DiscoverySpoolError::MissingOffer(observation));
        }
        let offer = self.codec.read_offer_at(&path)?;
        if offer.reference.observation_id != observation {
            return Err(DiscoverySpoolError::CorruptRecord { path });
        }
        Ok(offer)
    }

    fn inspect_root(&self) -> Result<(), DiscoverySpoolError> {
        inspect_private_directory(&self.codec.root)?;
        let entries = fs::read_dir(&self.codec.root)
            .map_err(|source| io_error("list read-only spool root", &self.codec.root, source))?;
        for entry in entries {
            let path = entry
                .map_err(|source| io_error("read spool root entry", &self.codec.root, source))?
                .path();
            match path.file_name().and_then(|name| name.to_str()) {
                Some("offers" | "acks" | "pending" | "quarantine" | "retirements") => {
                    inspect_private_directory(&path)?
                }
                Some(LOCK_FILE) => inspect_private_file(&path)?,
                _ => return Err(DiscoverySpoolError::UnexpectedEntry(path)),
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct DiscoverySpoolV1 {
    root: PathBuf,
    offers: PathBuf,
    acks: PathBuf,
    pending: PathBuf,
    quarantine: PathBuf,
    retirements: PathBuf,
    chain_id: u64,
    genesis_hash: B256,
    limits: SchemaLimits,
}

impl DiscoverySpoolV1 {
    pub fn open(
        root: impl AsRef<Path>,
        chain_id: u64,
        genesis_hash: B256,
        limits: SchemaLimits,
    ) -> Result<Self, DiscoverySpoolError> {
        if chain_id == 0 || genesis_hash.is_zero() {
            return Err(DiscoverySpoolError::InvalidIdentity);
        }
        let root = root.as_ref().to_path_buf();
        create_private_directory(&root)?;
        let offers = root.join("offers");
        let acks = root.join("acks");
        let pending = root.join("pending");
        let quarantine = root.join("quarantine");
        let retirements = root.join("retirements");
        create_private_directory(&offers)?;
        create_private_directory(&acks)?;
        create_private_directory(&pending)?;
        create_private_directory(&quarantine)?;
        create_private_directory(&retirements)?;
        let spool = Self {
            root,
            offers,
            acks,
            pending,
            quarantine,
            retirements,
            chain_id,
            genesis_hash,
            limits,
        };
        let _lock = FileLock::acquire(&spool.root, LOCK_FILE)?;
        recover_crash_temps(&spool.offers)?;
        recover_crash_temps(&spool.acks)?;
        recover_crash_temps(&spool.pending)?;
        recover_crash_temps(&spool.quarantine)?;
        recover_crash_temps(&spool.retirements)?;
        Ok(spool)
    }

    pub fn put_offer(
        &self,
        generation: u64,
        spec: &FinalizedJobSpecV1,
    ) -> Result<(DiscoveryOfferRefV1, PutOutcomeV1), DiscoverySpoolError> {
        let reference = DiscoveryOfferRefV1::from_spec(
            self.chain_id,
            self.genesis_hash,
            generation,
            spec,
            &self.limits,
        )?;
        let outcome = self.put_offer_exact(&reference, spec)?;
        Ok((reference, outcome))
    }

    pub fn put_offer_exact(
        &self,
        reference: &DiscoveryOfferRefV1,
        spec: &FinalizedJobSpecV1,
    ) -> Result<PutOutcomeV1, DiscoverySpoolError> {
        let _lock = FileLock::acquire(&self.root, LOCK_FILE)?;
        self.require_not_quarantined(reference.observation_id)?;
        let canonical = spec.encode_body(&self.limits)?;
        let expected = DiscoveryOfferRefV1::from_spec(
            self.chain_id,
            self.genesis_hash,
            reference.generation,
            spec,
            &self.limits,
        )?;
        if reference != &expected {
            return self.latch_conflict(reference.observation_id, "offer authority");
        }
        let path = self.offer_path(&reference.observation_id);
        if regular_file_exists(&path)? {
            let stored = self.read_offer_at(&path)?;
            if stored.reference == *reference && stored.spec == *spec {
                if !regular_file_exists(&self.ack_path(&reference.observation_id))? {
                    self.ensure_pending_marker_unlocked(reference.observation_id)?;
                }
                return Ok(PutOutcomeV1::ExactDuplicate);
            }
            return self.latch_conflict(reference.observation_id, "offer replay");
        }
        if regular_file_exists(&self.ack_path(&reference.observation_id))? {
            return self.latch_conflict(reference.observation_id, "ack without offer");
        }
        let encoded = encode_offer_record(reference, &canonical)?;
        persist_immutable_atomic(&self.offers, &path, &encoded)?;
        self.ensure_pending_marker_unlocked(reference.observation_id)?;
        Ok(PutOutcomeV1::Inserted)
    }

    pub fn put_ack(
        &self,
        offer: &DiscoveryOfferRefV1,
        receipt: &VerifiedExportReceipt,
        bundle: &ProtocolBundleV1,
    ) -> Result<AckPutOutcomeV1, DiscoverySpoolError> {
        let _lock = FileLock::acquire(&self.root, LOCK_FILE)?;
        self.require_not_quarantined(offer.observation_id)?;
        let Some(stored_offer) = self.resolve_offer_for_ack(offer)? else {
            return Ok(AckPutOutcomeV1::Retired);
        };
        let committed = receipt.committed();
        if !self.ack_matches_stored_offer(offer, &stored_offer, receipt, &committed, bundle) {
            return self.latch_conflict(offer.observation_id, "ack authority");
        }
        let reference = DiscoveryAckRefV1::from_committed(
            offer,
            &committed,
            receipt.receipt_ref().transport_digest,
            &self.limits,
        )?;
        self.commit_ack_record(
            offer.observation_id,
            &stored_offer,
            receipt,
            reference,
            committed,
        )
    }

    fn resolve_offer_for_ack(
        &self,
        offer: &DiscoveryOfferRefV1,
    ) -> Result<Option<PendingDiscoveryV1>, DiscoverySpoolError> {
        let retirement_path = self.retirement_path(&offer.observation_id);
        if regular_file_exists(&retirement_path)? {
            let retirement = self.read_retirement_at(&retirement_path)?;
            if retirement.reference == *offer {
                return Ok(None);
            }
            return self.latch_conflict(offer.observation_id, "retired ACK authority");
        }
        let offer_path = self.offer_path(&offer.observation_id);
        if !regular_file_exists(&offer_path)? {
            if self.has_ack_or_pending(&offer.observation_id)? {
                return Err(DiscoverySpoolError::MissingOffer(offer.observation_id));
            }
            // `put_ack` is reached only from work previously read from this
            // spool. With every record gone, a completed retirement won the
            // cross-process race. The late completion has no authority.
            return Ok(None);
        }
        self.read_offer_at(&offer_path).map(Some)
    }

    fn ack_matches_stored_offer(
        &self,
        offer: &DiscoveryOfferRefV1,
        stored_offer: &PendingDiscoveryV1,
        receipt: &VerifiedExportReceipt,
        committed: &SnapshotExportCommittedV1,
        bundle: &ProtocolBundleV1,
    ) -> bool {
        let receipt_matches = self
            .verified_receipt_matches_offer(&stored_offer.spec, receipt, bundle)
            .unwrap_or(false);
        let expected_ack_fields = (
            &stored_offer.reference,
            self.chain_id,
            self.genesis_hash,
            stored_offer.spec.summary.job_id,
            offer.generation,
            Some(committed.pin_generation),
            true,
        );
        let actual_ack_fields = (
            offer,
            offer.chain_id,
            offer.genesis_hash,
            committed.job_id,
            receipt.source_pin_generation(),
            offer.generation.checked_add(1),
            receipt_matches,
        );
        actual_ack_fields == expected_ack_fields
    }

    fn commit_ack_record(
        &self,
        observation: B256,
        stored_offer: &PendingDiscoveryV1,
        receipt: &VerifiedExportReceipt,
        reference: DiscoveryAckRefV1,
        committed: SnapshotExportCommittedV1,
    ) -> Result<AckPutOutcomeV1, DiscoverySpoolError> {
        let canonical = committed.encode_body(&self.limits)?;
        let path = self.ack_path(&observation);
        if regular_file_exists(&path)? {
            let stored = self.read_ack_for_offer(&path, stored_offer)?;
            if stored.reference == reference && stored.committed == committed {
                self.remove_pending_marker_unlocked(observation)?;
                return Ok(AckPutOutcomeV1::Committed {
                    reference,
                    outcome: PutOutcomeV1::ExactDuplicate,
                });
            }
            return self
                .latch_conflict(observation, "ack replay")
                .map(|outcome| AckPutOutcomeV1::Committed { reference, outcome });
        }
        let encoded = encode_ack_record(
            &reference,
            receipt.lease_generation(),
            receipt.manifest_hash(),
            &canonical,
        )?;
        persist_immutable_atomic(&self.acks, &path, &encoded)?;
        self.remove_pending_marker_unlocked(observation)?;
        Ok(AckPutOutcomeV1::Committed {
            reference,
            outcome: PutOutcomeV1::Inserted,
        })
    }

    pub fn pending(
        &self,
        observation: &B256,
    ) -> Result<Option<PendingDiscoveryV1>, DiscoverySpoolError> {
        let _lock = FileLock::acquire(&self.root, LOCK_FILE)?;
        self.pending_unlocked(observation)
    }

    fn pending_from_index(
        &self,
        observation: &B256,
    ) -> Result<Option<PendingDiscoveryV1>, DiscoverySpoolError> {
        let _lock = FileLock::acquire(&self.root, LOCK_FILE)?;
        let marker = self.pending_path(observation);
        if !regular_file_exists(&marker)? {
            return Ok(None);
        }
        verify_pending_marker_at(&marker, *observation)?;
        if !regular_file_exists(&self.offer_path(observation))? {
            return Err(DiscoverySpoolError::MissingOffer(*observation));
        }
        self.pending_unlocked(observation)
    }

    pub fn ack(
        &self,
        observation: &B256,
    ) -> Result<Option<StoredDiscoveryAckV1>, DiscoverySpoolError> {
        let _lock = FileLock::acquire(&self.root, LOCK_FILE)?;
        self.require_not_quarantined(*observation)?;
        let path = self.ack_path(observation);
        if !regular_file_exists(&path)? {
            return Ok(None);
        }
        let offer_path = self.offer_path(observation);
        if !regular_file_exists(&offer_path)? {
            return Err(DiscoverySpoolError::MissingOffer(*observation));
        }
        let offer = self.read_offer_at(&offer_path)?;
        self.read_ack_for_offer(&path, &offer).map(Some)
    }

    pub fn pending_count(&self) -> Result<u64, DiscoverySpoolError> {
        let mut count = 0_u64;
        for pending in self.pending_cursor()? {
            pending?;
            count = count.checked_add(1).ok_or(DiscoverySpoolError::Overflow)?;
        }
        Ok(count)
    }

    pub fn pending_cursor(&self) -> Result<DiscoveryPendingCursorV1, DiscoverySpoolError> {
        let entries = fs::read_dir(&self.pending)
            .map_err(|source| io_error("list pending discovery offers", &self.pending, source))?;
        Ok(DiscoveryPendingCursorV1 {
            spool: self.clone(),
            entries,
        })
    }

    pub fn is_quarantined(&self, observation: &B256) -> Result<bool, DiscoverySpoolError> {
        regular_file_exists(&self.quarantine_path(observation))
    }
}

struct FileLock {
    file: File,
}

impl FileLock {
    #[allow(unsafe_code)]
    fn acquire(root: &Path, name: &str) -> Result<Self, DiscoverySpoolError> {
        let path = root.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|source| io_error("open spool lock", &path, source))?;
        inspect_private_file(&path)?;
        // SAFETY: `file` owns the live descriptor for the duration of the flock call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io_error(
                "lock spool",
                &path,
                std::io::Error::last_os_error(),
            ));
        }
        Ok(Self { file })
    }
}

impl Drop for FileLock {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `self.file` still owns a live descriptor until this drop completes.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[derive(Debug, Error)]
pub enum DiscoverySpoolError {
    #[error("invalid discovery spool network identity")]
    InvalidIdentity,
    #[error("unsafe discovery spool path {0}")]
    UnsafePath(PathBuf),
    #[error("discovery spool path {path} has mode {actual:o}")]
    PermissiveMode { path: PathBuf, actual: u32 },
    #[error("unexpected discovery spool entry {0}")]
    UnexpectedEntry(PathBuf),
    #[error("ambiguous discovery spool temporary {0}")]
    AmbiguousTemporary(PathBuf),
    #[error("immutable discovery record already exists at {0}")]
    ImmutableRecordExists(PathBuf),
    #[error("discovery record is too large at {0}")]
    RecordTooLarge(PathBuf),
    #[error("malformed discovery record at {path}")]
    MalformedRecord { path: PathBuf },
    #[error("corrupt discovery record at {path}")]
    CorruptRecord { path: PathBuf },
    #[error("discovery observation {observation} is quarantined")]
    Quarantined { observation: B256 },
    #[error("discovery conflict latched for {observation}: {reason}")]
    ConflictLatched {
        observation: B256,
        reason: &'static str,
    },
    #[error("missing discovery offer {0}")]
    MissingOffer(B256),
    #[error("integer overflow")]
    Overflow,
    #[error("invalid discovery retirement authority")]
    InvalidRetirement,
    #[error("invalid projection checkpoint")]
    InvalidCheckpoint,
    #[error("malformed projection checkpoint")]
    MalformedCheckpoint,
    #[error("corrupt projection checkpoint")]
    CorruptCheckpoint,
    #[error("projection checkpoint baseline mismatch")]
    CheckpointBaselineMismatch,
    #[error("non-contiguous projection checkpoint advance")]
    NonContiguousCheckpoint,
    #[error("projection checkpoint compare failed: stored {stored:?}, expected {expected:?}")]
    CheckpointCompareFailed {
        stored: ProjectionCheckpoint,
        expected: ProjectionCheckpoint,
    },
    #[error("discovery spool I/O during {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Control(#[from] DiscoveryControlError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

#[cfg(test)]
mod readonly_bounded_read_tests {
    use super::*;

    struct CountingReader {
        source: std::io::Cursor<Vec<u8>>,
        consumed: usize,
    }

    impl std::io::Read for CountingReader {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let count = self.source.read(bytes)?;
            self.consumed += count;
            Ok(count)
        }
    }

    #[test]
    fn changed_source_read_stops_after_observed_length_plus_one() {
        let mut reader = CountingReader {
            source: std::io::Cursor::new(vec![7; 100]),
            consumed: 0,
        };
        let error = read_spool_bytes(&mut reader, Path::new("growing.offer"), 2)
            .expect_err("growth after stat must fail");
        assert!(matches!(error, DiscoverySpoolError::CorruptRecord { .. }));
        assert_eq!(
            reader.consumed, 3,
            "never consume the remaining growing source"
        );
    }

    #[test]
    fn bounded_read_preserves_stable_bytes_and_rejects_shortened_source() {
        for length in [0, 1, 42, 100] {
            let bytes = vec![7; length];
            let mut reader = CountingReader {
                source: std::io::Cursor::new(bytes.clone()),
                consumed: 0,
            };
            assert_eq!(
                read_spool_bytes(&mut reader, Path::new("stable.offer"), length).unwrap(),
                bytes
            );
            assert_eq!(reader.consumed, length);
        }
        let mut shortened = std::io::Cursor::new(vec![7; 2]);
        assert!(matches!(
            read_spool_bytes(&mut shortened, Path::new("short.offer"), 3),
            Err(DiscoverySpoolError::CorruptRecord { .. })
        ));
    }
}
