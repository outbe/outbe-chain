use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use alloy_primitives::{keccak256, B256};
use outbe_primitives::projection::ProjectionCheckpoint;

use crate::discovery_control::DiscoveryOfferRefV1;

use super::{
    records::{encode_retirement_intent, io_error, read_b256, read_u16, read_u64},
    DiscoverySpoolError, DiscoverySpoolV1, FileLock, PutOutcomeV1, RetirementIntentV1,
    DIRECTORY_MODE, FILE_MODE, LOCK_FILE, RECORD_VERSION, TEMP_SUFFIX,
};

const CHECKPOINT_MAGIC: [u8; 8] = *b"OUTBDCP1";
const CHECKPOINT_FILE: &str = "checkpoint.v1";
const CHECKPOINT_TEMP: &str = "checkpoint.v1.tmp";
const CHECKPOINT_LOCK: &str = ".lock";
const CHECKPOINT_FIXED_BYTES: usize = 8 + 2 + (8 + 32) * 3 + 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointAdvanceOutcomeV1 {
    Advanced,
    ExactReplay,
}

/// The three native frontiers observed in an existing closure checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClosureCheckpointInspectionV1 {
    pub baseline: ProjectionCheckpoint,
    pub previous: ProjectionCheckpoint,
    pub current: ProjectionCheckpoint,
}

/// Inspect a stopped store without initializing, locking, recovering or writing it.
///
/// `root` is the native closure-checkpoint directory. Its writer must already
/// be stopped. A successful read does not establish that precondition.
pub fn inspect_closure_checkpoint(
    root: impl AsRef<Path>,
    expected_baseline: ProjectionCheckpoint,
) -> Result<ClosureCheckpointInspectionV1, DiscoverySpoolError> {
    validate_checkpoint(expected_baseline)?;
    let state = decode_checkpoint_state(&read_bounded_private_file(
        &root.as_ref().join(CHECKPOINT_FILE),
        CHECKPOINT_FIXED_BYTES,
    )?)?;
    if state.baseline != expected_baseline {
        return Err(DiscoverySpoolError::CheckpointBaselineMismatch);
    }
    Ok(ClosureCheckpointInspectionV1 {
        baseline: state.baseline,
        previous: state.previous,
        current: state.current,
    })
}

pub struct ContiguousCheckpointStoreV1 {
    root: PathBuf,
    baseline: ProjectionCheckpoint,
}

impl ContiguousCheckpointStoreV1 {
    pub fn open(
        root: impl AsRef<Path>,
        baseline: ProjectionCheckpoint,
    ) -> Result<Self, DiscoverySpoolError> {
        validate_checkpoint(baseline)?;
        let root = root.as_ref().to_path_buf();
        create_private_directory(&root)?;
        let store = Self { root, baseline };
        let _lock = FileLock::acquire(&store.root, CHECKPOINT_LOCK)?;
        recover_named_temp(&store.root, CHECKPOINT_TEMP)?;
        let path = store.root.join(CHECKPOINT_FILE);
        if regular_file_exists(&path)? {
            let state = decode_checkpoint_state(&read_bounded_private_file(
                &path,
                CHECKPOINT_FIXED_BYTES,
            )?)?;
            if state.baseline != baseline {
                return Err(DiscoverySpoolError::CheckpointBaselineMismatch);
            }
        } else {
            persist_replace_atomic(
                &store.root,
                &path,
                &store.root.join(CHECKPOINT_TEMP),
                &encode_checkpoint_state(CheckpointStateV1 {
                    baseline,
                    previous: baseline,
                    current: baseline,
                }),
            )?;
        }
        Ok(store)
    }

    pub fn current(&self) -> Result<ProjectionCheckpoint, DiscoverySpoolError> {
        let _lock = FileLock::acquire(&self.root, CHECKPOINT_LOCK)?;
        Ok(self.load_state()?.current)
    }

    pub fn compare_and_advance(
        &self,
        expected: ProjectionCheckpoint,
        next: ProjectionCheckpoint,
    ) -> Result<CheckpointAdvanceOutcomeV1, DiscoverySpoolError> {
        validate_checkpoint(expected)?;
        validate_checkpoint(next)?;
        if next.block_number
            != expected
                .block_number
                .checked_add(1)
                .ok_or(DiscoverySpoolError::NonContiguousCheckpoint)?
        {
            return Err(DiscoverySpoolError::NonContiguousCheckpoint);
        }
        self.apply_advance(expected, next)
    }

    /// Atomically advances to a later sparse checkpoint while comparing the
    /// exact current authority. The unified reader already validated intermediate
    /// block identities. They do not need to stay in RAM.
    pub fn compare_and_advance_to(
        &self,
        expected: ProjectionCheckpoint,
        next: ProjectionCheckpoint,
    ) -> Result<CheckpointAdvanceOutcomeV1, DiscoverySpoolError> {
        validate_checkpoint(expected)?;
        validate_checkpoint(next)?;
        if next.block_number <= expected.block_number {
            return Err(DiscoverySpoolError::NonContiguousCheckpoint);
        }
        self.apply_advance(expected, next)
    }

    fn apply_advance(
        &self,
        expected: ProjectionCheckpoint,
        next: ProjectionCheckpoint,
    ) -> Result<CheckpointAdvanceOutcomeV1, DiscoverySpoolError> {
        let _lock = FileLock::acquire(&self.root, CHECKPOINT_LOCK)?;
        let state = self.load_state()?;
        if state.current == next && state.previous == expected {
            return Ok(CheckpointAdvanceOutcomeV1::ExactReplay);
        }
        if state.current != expected {
            return Err(DiscoverySpoolError::CheckpointCompareFailed {
                stored: state.current,
                expected,
            });
        }
        persist_replace_atomic(
            &self.root,
            &self.root.join(CHECKPOINT_FILE),
            &self.root.join(CHECKPOINT_TEMP),
            &encode_checkpoint_state(CheckpointStateV1 {
                baseline: state.baseline,
                previous: expected,
                current: next,
            }),
        )?;
        Ok(CheckpointAdvanceOutcomeV1::Advanced)
    }

    fn load_state(&self) -> Result<CheckpointStateV1, DiscoverySpoolError> {
        let state = decode_checkpoint_state(&read_bounded_private_file(
            &self.root.join(CHECKPOINT_FILE),
            CHECKPOINT_FIXED_BYTES,
        )?)?;
        if state.baseline != self.baseline {
            return Err(DiscoverySpoolError::CheckpointBaselineMismatch);
        }
        Ok(state)
    }
}

#[derive(Clone, Copy)]
struct CheckpointStateV1 {
    baseline: ProjectionCheckpoint,
    previous: ProjectionCheckpoint,
    current: ProjectionCheckpoint,
}

fn encode_checkpoint_state(state: CheckpointStateV1) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(CHECKPOINT_FIXED_BYTES);
    encoded.extend_from_slice(&CHECKPOINT_MAGIC);
    encoded.extend_from_slice(&RECORD_VERSION.to_be_bytes());
    encode_checkpoint(&mut encoded, state.baseline);
    encode_checkpoint(&mut encoded, state.previous);
    encode_checkpoint(&mut encoded, state.current);
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    encoded
}

fn decode_checkpoint_state(encoded: &[u8]) -> Result<CheckpointStateV1, DiscoverySpoolError> {
    if encoded.len() != CHECKPOINT_FIXED_BYTES || encoded[..8] != CHECKPOINT_MAGIC {
        return Err(DiscoverySpoolError::MalformedCheckpoint);
    }
    if read_u16(encoded, 8)? != RECORD_VERSION {
        return Err(DiscoverySpoolError::MalformedCheckpoint);
    }
    if keccak256(&encoded[..encoded.len() - 32]) != B256::from_slice(&encoded[encoded.len() - 32..])
    {
        return Err(DiscoverySpoolError::CorruptCheckpoint);
    }
    let state = CheckpointStateV1 {
        baseline: decode_checkpoint(encoded, 10)?,
        previous: decode_checkpoint(encoded, 50)?,
        current: decode_checkpoint(encoded, 90)?,
    };
    validate_checkpoint(state.baseline)?;
    validate_checkpoint(state.previous)?;
    validate_checkpoint(state.current)?;
    if state.current.block_number < state.baseline.block_number
        || state.previous.block_number > state.current.block_number
    {
        return Err(DiscoverySpoolError::CorruptCheckpoint);
    }
    Ok(state)
}

fn encode_checkpoint(encoded: &mut Vec<u8>, checkpoint: ProjectionCheckpoint) {
    encoded.extend_from_slice(&checkpoint.block_number.to_be_bytes());
    encoded.extend_from_slice(checkpoint.block_hash.as_slice());
}

fn decode_checkpoint(
    encoded: &[u8],
    start: usize,
) -> Result<ProjectionCheckpoint, DiscoverySpoolError> {
    Ok(ProjectionCheckpoint {
        block_number: read_u64(encoded, start)?,
        block_hash: read_b256(encoded, start + 8)?,
    })
}

fn validate_checkpoint(checkpoint: ProjectionCheckpoint) -> Result<(), DiscoverySpoolError> {
    if checkpoint.block_hash.is_zero() {
        Err(DiscoverySpoolError::InvalidCheckpoint)
    } else {
        Ok(())
    }
}

pub(super) fn create_private_directory(path: &Path) -> Result<(), DiscoverySpoolError> {
    reject_symlink_ancestors(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => inspect_directory_mode(path, &metadata),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|source| io_error("create private directory", path, source))?;
            fs::set_permissions(path, fs::Permissions::from_mode(DIRECTORY_MODE))
                .map_err(|source| io_error("set private directory mode", path, source))?;
            sync_parent(path)?;
            inspect_private_directory(path)
        }
        Err(source) => Err(io_error("inspect private directory", path, source)),
    }
}

pub(super) fn inspect_private_directory(path: &Path) -> Result<(), DiscoverySpoolError> {
    reject_symlink_ancestors(path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspect private directory", path, source))?;
    inspect_directory_mode(path, &metadata)
}

fn inspect_directory_mode(path: &Path, metadata: &fs::Metadata) -> Result<(), DiscoverySpoolError> {
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(DiscoverySpoolError::UnsafePath(path.to_path_buf()));
    }
    let actual = metadata.permissions().mode() & 0o777;
    if actual != DIRECTORY_MODE {
        return Err(DiscoverySpoolError::PermissiveMode {
            path: path.to_path_buf(),
            actual,
        });
    }
    Ok(())
}

fn reject_symlink_ancestors(path: &Path) -> Result<(), DiscoverySpoolError> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(DiscoverySpoolError::UnsafePath(ancestor.to_path_buf()))
            }
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error("inspect path ancestor", ancestor, source)),
        }
    }
    Ok(())
}

pub(super) fn recover_crash_temps(root: &Path) -> Result<(), DiscoverySpoolError> {
    let mut removed = false;
    for entry in
        fs::read_dir(root).map_err(|source| io_error("list spool directory", root, source))?
    {
        let entry = entry.map_err(|source| io_error("read spool directory", root, source))?;
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(TEMP_SUFFIX))
        {
            continue;
        }
        inspect_private_file(&path)?;
        fs::remove_file(&path)
            .map_err(|source| io_error("remove crash temporary", &path, source))?;
        removed = true;
    }
    if removed {
        sync_directory(root)?;
    }
    Ok(())
}

fn recover_named_temp(root: &Path, name: &str) -> Result<(), DiscoverySpoolError> {
    let path = root.join(name);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            inspect_private_file(&path)?;
            fs::remove_file(&path)
                .map_err(|source| io_error("remove checkpoint temporary", &path, source))?;
            sync_directory(root)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("inspect checkpoint temporary", &path, source)),
    }
}

pub(super) fn persist_immutable_atomic(
    root: &Path,
    target: &Path,
    encoded: &[u8],
) -> Result<(), DiscoverySpoolError> {
    if regular_file_exists(target)? {
        return Err(DiscoverySpoolError::ImmutableRecordExists(
            target.to_path_buf(),
        ));
    }
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| DiscoverySpoolError::UnsafePath(target.to_path_buf()))?;
    let temp = root.join(format!("{file_name}{TEMP_SUFFIX}"));
    persist_replace_atomic(root, target, &temp, encoded)
}

fn persist_replace_atomic(
    root: &Path,
    target: &Path,
    temp: &Path,
    encoded: &[u8],
) -> Result<(), DiscoverySpoolError> {
    if regular_file_exists(temp)? {
        return Err(DiscoverySpoolError::AmbiguousTemporary(temp.to_path_buf()));
    }
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(temp)
            .map_err(|source| io_error("create atomic temporary", temp, source))?;
        file.write_all(encoded)
            .map_err(|source| io_error("write atomic temporary", temp, source))?;
        file.sync_all()
            .map_err(|source| io_error("fsync atomic temporary", temp, source))?;
        fs::rename(temp, target)
            .map_err(|source| io_error("publish atomic record", target, source))?;
        sync_directory(root)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

pub(super) fn regular_file_exists(path: &Path) -> Result<bool, DiscoverySpoolError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            inspect_private_file(path)?;
            Ok(true)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("inspect spool path", path, source)),
    }
}

pub(super) fn remove_private_file_if_present(
    root: &Path,
    path: &Path,
    operation: &'static str,
) -> Result<(), DiscoverySpoolError> {
    if !regular_file_exists(path)? {
        return Ok(());
    }
    fs::remove_file(path).map_err(|source| io_error(operation, path, source))?;
    sync_directory(root)
}

pub(super) fn inspect_private_file(path: &Path) -> Result<(), DiscoverySpoolError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspect private file", path, source))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(DiscoverySpoolError::UnsafePath(path.to_path_buf()));
    }
    let actual = metadata.permissions().mode() & 0o777;
    if actual != FILE_MODE {
        return Err(DiscoverySpoolError::PermissiveMode {
            path: path.to_path_buf(),
            actual,
        });
    }
    Ok(())
}

pub(super) fn read_bounded_private_file(
    path: &Path,
    max_bytes: usize,
) -> Result<Vec<u8>, DiscoverySpoolError> {
    inspect_private_file(path)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| io_error("open private spool file", path, source))?;
    let length = usize::try_from(
        file.metadata()
            .map_err(|source| io_error("stat private spool file", path, source))?
            .len(),
    )
    .map_err(|_| DiscoverySpoolError::RecordTooLarge(path.to_path_buf()))?;
    if length > max_bytes {
        return Err(DiscoverySpoolError::RecordTooLarge(path.to_path_buf()));
    }
    read_spool_bytes(&mut file, path, length)
}

pub(super) fn read_spool_bytes(
    reader: &mut impl std::io::Read,
    path: &Path,
    length: usize,
) -> Result<Vec<u8>, DiscoverySpoolError> {
    let read_limit = length
        .checked_add(1)
        .and_then(|limit| u64::try_from(limit).ok())
        .ok_or(DiscoverySpoolError::Overflow)?;
    let mut encoded = Vec::with_capacity(length);
    reader
        .take(read_limit)
        .read_to_end(&mut encoded)
        .map_err(|source| io_error("read private spool file", path, source))?;
    if encoded.len() != length {
        return Err(DiscoverySpoolError::CorruptRecord {
            path: path.to_path_buf(),
        });
    }
    Ok(encoded)
}

fn sync_directory(path: &Path) -> Result<(), DiscoverySpoolError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("fsync directory", path, source))
}

fn sync_parent(path: &Path) -> Result<(), DiscoverySpoolError> {
    path.parent().map_or(Ok(()), sync_directory)
}

impl DiscoverySpoolV1 {
    /// Durably records that one discovery record may be removed after the
    /// closure checkpoint reaches `closed_through`. This method writes the intent
    /// before the checkpoint advances. Thus every crash cut has enough authority
    /// to either retain the record or finish removing it after restart.
    pub fn prepare_retirement(
        &self,
        reference: &DiscoveryOfferRefV1,
        closed_through: u64,
    ) -> Result<PutOutcomeV1, DiscoverySpoolError> {
        self.validate_retirement_request(reference, closed_through)?;
        let _lock = FileLock::acquire(&self.root, LOCK_FILE)?;
        self.require_not_quarantined(reference.observation_id)?;
        let intent_path = self.retirement_path(&reference.observation_id);
        if regular_file_exists(&intent_path)? {
            return self.check_existing_retirement(&intent_path, reference);
        }
        if !self.verify_offer_ready_for_retirement(reference)? {
            return Ok(PutOutcomeV1::ExactDuplicate);
        }
        let encoded = encode_retirement_intent(&RetirementIntentV1 {
            closed_through,
            reference: reference.clone(),
        });
        persist_immutable_atomic(&self.retirements, &intent_path, &encoded)?;
        Ok(PutOutcomeV1::Inserted)
    }

    fn validate_retirement_request(
        &self,
        reference: &DiscoveryOfferRefV1,
        closed_through: u64,
    ) -> Result<(), DiscoverySpoolError> {
        if closed_through == 0
            || (reference.chain_id, reference.genesis_hash) != (self.chain_id, self.genesis_hash)
        {
            return Err(DiscoverySpoolError::InvalidRetirement);
        }
        reference.validate()?;
        Ok(())
    }

    fn check_existing_retirement(
        &self,
        intent_path: &Path,
        reference: &DiscoveryOfferRefV1,
    ) -> Result<PutOutcomeV1, DiscoverySpoolError> {
        let stored = self.read_retirement_at(intent_path)?;
        if stored.reference == *reference {
            Ok(PutOutcomeV1::ExactDuplicate)
        } else {
            self.latch_conflict(reference.observation_id, "retirement replay")
        }
    }

    fn verify_offer_ready_for_retirement(
        &self,
        reference: &DiscoveryOfferRefV1,
    ) -> Result<bool, DiscoverySpoolError> {
        let offer_path = self.offer_path(&reference.observation_id);
        if !regular_file_exists(&offer_path)? {
            if self.has_ack_or_pending(&reference.observation_id)? {
                return Err(DiscoverySpoolError::MissingOffer(reference.observation_id));
            }
            return Ok(false);
        }
        let offer = self.read_offer_at(&offer_path)?;
        if offer.reference != *reference {
            return self.latch_conflict(reference.observation_id, "retirement authority");
        }
        let ack_path = self.ack_path(&reference.observation_id);
        if regular_file_exists(&ack_path)? {
            self.read_ack_for_offer(&ack_path, &offer)?;
        }
        Ok(true)
    }

    pub(super) fn has_ack_or_pending(&self, observation: &B256) -> Result<bool, DiscoverySpoolError> {
        Ok(regular_file_exists(&self.ack_path(observation))?
            || regular_file_exists(&self.pending_path(observation))?)
    }
}
