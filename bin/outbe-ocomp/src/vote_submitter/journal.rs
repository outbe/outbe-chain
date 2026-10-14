use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use alloy_primitives::{keccak256, Address, B256};

use super::{
    VoteInclusionV1, VoteSubmissionErrorV1, VoteSubmissionRecordV1, VoteSubmissionStageV1,
    MAX_RAW_TRANSACTION_BYTES,
};

const JOURNAL_MAGIC: [u8; 8] = *b"OUTBVOT1";
const JOURNAL_VERSION: u16 = 1;
const JOURNAL_LOCK_FILE: &str = "vote-submissions.lock";
const JOURNAL_FIXED_BYTES_WITHOUT_RAW: usize =
    8 + 2 + 8 + 1 + 32 + 32 + 20 + 8 + 16 + 32 + 4 + 1 + 8 + 32 + 1 + 32;

pub(super) struct VoteSubmissionJournalV1 {
    root: PathBuf,
    _lock: File,
}

impl VoteSubmissionJournalV1 {
    #[allow(unsafe_code)]
    pub(super) fn open(root: &Path) -> Result<Self, VoteSubmissionErrorV1> {
        ensure_private_directory(root)?;
        let lock_path = root.join(JOURNAL_LOCK_FILE);
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&lock_path)
            .map_err(|source| io_error("open vote submission lock", &lock_path, source))?;
        // SAFETY: `lock` owns a live descriptor for the complete `flock` call.
        let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(io_error(
                "lock vote submission journal",
                &lock_path,
                std::io::Error::last_os_error(),
            ));
        }
        Ok(Self {
            root: root.to_path_buf(),
            _lock: lock,
        })
    }

    pub(super) fn load(
        &self,
        job_id: B256,
    ) -> Result<Option<VoteSubmissionRecordV1>, VoteSubmissionErrorV1> {
        let path = self.record_path(job_id);
        let temp = self.temp_path(job_id);
        if path_exists(&temp)? {
            return Err(VoteSubmissionErrorV1::AmbiguousJournal(temp));
        }
        if !path_exists(&path)? {
            return Ok(None);
        }
        let mut file = open_regular_nofollow(&path)?;
        let metadata = file
            .metadata()
            .map_err(|source| io_error("stat vote submission journal", &path, source))?;
        let len =
            usize::try_from(metadata.len()).map_err(|_| VoteSubmissionErrorV1::JournalTooLarge)?;
        if !(JOURNAL_FIXED_BYTES_WITHOUT_RAW
            ..=JOURNAL_FIXED_BYTES_WITHOUT_RAW + MAX_RAW_TRANSACTION_BYTES)
            .contains(&len)
        {
            return Err(VoteSubmissionErrorV1::JournalTooLarge);
        }
        let mut bytes = Vec::with_capacity(len);
        file.read_to_end(&mut bytes)
            .map_err(|source| io_error("read vote submission journal", &path, source))?;
        let record = decode_record(&bytes)?;
        if record.job_id != job_id {
            return Err(VoteSubmissionErrorV1::InvalidJournal);
        }
        Ok(Some(record))
    }

    pub(super) fn persist(
        &self,
        record: VoteSubmissionRecordV1,
    ) -> Result<(), VoteSubmissionErrorV1> {
        validate_record(&record)?;
        let encoded = encode_record(&record)?;
        let temp = self.temp_path(record.job_id);
        let final_path = self.record_path(record.job_id);
        if path_exists(&temp)? {
            return Err(VoteSubmissionErrorV1::AmbiguousJournal(temp));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temp)
            .map_err(|source| io_error("create vote journal temp", &temp, source))?;
        file.write_all(&encoded)
            .map_err(|source| io_error("write vote journal temp", &temp, source))?;
        file.sync_all()
            .map_err(|source| io_error("fsync vote journal temp", &temp, source))?;
        fs::rename(&temp, &final_path)
            .map_err(|source| io_error("publish vote submission journal", &final_path, source))?;
        sync_directory(&self.root)
    }

    /// Best-effort operational age of the current durable stage. Filesystem
    /// clocks are never part of submission control flow or journal validity.
    pub(super) fn stage_age(&self, job_id: B256) -> Option<Duration> {
        let metadata = fs::symlink_metadata(self.record_path(job_id)).ok()?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return None;
        }
        SystemTime::now()
            .duration_since(metadata.modified().ok()?)
            .ok()
    }

    pub(super) fn record_path(&self, job_id: B256) -> PathBuf {
        self.root
            .join(format!("{}.vote.v1", hex::encode(job_id.as_slice())))
    }

    fn temp_path(&self, job_id: B256) -> PathBuf {
        self.root
            .join(format!("{}.vote.v1.tmp", hex::encode(job_id.as_slice())))
    }
}

pub(super) fn record_stage_transition(
    record: &VoteSubmissionRecordV1,
    prior_stage: Option<VoteSubmissionStageV1>,
    prior_age: Option<Duration>,
) {
    metrics::counter!(
        "outbe_ocomp_vote_submission_transitions_total",
        "stage" => record.stage.label()
    )
    .increment(1);
    if let (Some(stage), Some(age)) = (prior_stage, prior_age) {
        metrics::histogram!(
            "outbe_ocomp_vote_submission_stage_age_seconds",
            "stage" => stage.label()
        )
        .record(age.as_secs_f64());
    }
    tracing::info!(
        target: "outbe::ocomp::vote_submission",
        job_id = %record.job_id,
        stage = record.stage.label(),
        prior_stage = prior_stage.map(VoteSubmissionStageV1::label).unwrap_or("absent"),
        prior_stage_age_seconds = prior_age.map(|age| age.as_secs_f64()),
        generation = record.generation,
        nonce = record.nonce,
        transaction_hash = %record.transaction_hash,
        "OCOMP vote submission stage advanced"
    );
}

pub(super) fn record_nonce_bypass(
    record: &VoteSubmissionRecordV1,
    canonical_nonce: u64,
    stage_age: Option<Duration>,
) {
    metrics::counter!(
        "outbe_ocomp_vote_nonce_bypassed_total",
        "stage" => record.stage.label()
    )
    .increment(1);
    if let Some(age) = stage_age {
        metrics::histogram!(
            "outbe_ocomp_vote_submission_stage_age_seconds",
            "stage" => record.stage.label()
        )
        .record(age.as_secs_f64());
    }
    tracing::warn!(
        target: "outbe::ocomp::vote_submission",
        job_id = %record.job_id,
        stage = record.stage.label(),
        stage_age_seconds = stage_age.map(|age| age.as_secs_f64()),
        generation = record.generation,
        pinned_nonce = record.nonce,
        canonical_nonce,
        transaction_hash = %record.transaction_hash,
        "OCOMP vote nonce was bypassed; rebuilding the same vote in a fresh envelope"
    );
}

fn validate_record(record: &VoteSubmissionRecordV1) -> Result<(), VoteSubmissionErrorV1> {
    if record.generation == 0
        || record.raw_transaction.is_empty()
        || record.raw_transaction.len() > MAX_RAW_TRANSACTION_BYTES
        || matches!(
            record.stage,
            VoteSubmissionStageV1::Prepared | VoteSubmissionStageV1::Submitted
        ) != record.inclusion.is_none()
    {
        return Err(VoteSubmissionErrorV1::InvalidJournal);
    }
    Ok(())
}

fn encode_record(record: &VoteSubmissionRecordV1) -> Result<Vec<u8>, VoteSubmissionErrorV1> {
    validate_record(record)?;
    let raw_len = u32::try_from(record.raw_transaction.len())
        .map_err(|_| VoteSubmissionErrorV1::JournalTooLarge)?;
    let mut bytes =
        Vec::with_capacity(JOURNAL_FIXED_BYTES_WITHOUT_RAW + record.raw_transaction.len());
    bytes.extend_from_slice(&JOURNAL_MAGIC);
    bytes.extend_from_slice(&JOURNAL_VERSION.to_be_bytes());
    bytes.extend_from_slice(&record.generation.to_be_bytes());
    bytes.push(record.stage as u8);
    bytes.extend_from_slice(record.job_id.as_slice());
    bytes.extend_from_slice(record.result_digest.as_slice());
    bytes.extend_from_slice(record.sender_address.as_slice());
    bytes.extend_from_slice(&record.nonce.to_be_bytes());
    bytes.extend_from_slice(&record.max_fee_per_gas.to_be_bytes());
    bytes.extend_from_slice(record.transaction_hash.as_slice());
    bytes.extend_from_slice(&raw_len.to_be_bytes());
    bytes.extend_from_slice(&record.raw_transaction);
    match record.inclusion {
        Some(inclusion) => {
            bytes.push(1);
            bytes.extend_from_slice(&inclusion.block_number.to_be_bytes());
            bytes.extend_from_slice(inclusion.block_hash.as_slice());
            bytes.push(u8::from(inclusion.success));
        }
        None => {
            bytes.push(0);
            bytes.extend_from_slice(&0_u64.to_be_bytes());
            bytes.extend_from_slice(B256::ZERO.as_slice());
            bytes.push(0);
        }
    }
    let checksum = keccak256(&bytes);
    bytes.extend_from_slice(checksum.as_slice());
    Ok(bytes)
}

fn decode_record(bytes: &[u8]) -> Result<VoteSubmissionRecordV1, VoteSubmissionErrorV1> {
    if bytes.len() < JOURNAL_FIXED_BYTES_WITHOUT_RAW || bytes.get(..8) != Some(&JOURNAL_MAGIC) {
        return Err(VoteSubmissionErrorV1::InvalidJournal);
    }
    let version = read_u16(bytes, 8)?;
    if version != JOURNAL_VERSION {
        return Err(VoteSubmissionErrorV1::UnsupportedJournalVersion(version));
    }
    let generation = read_u64(bytes, 10)?;
    let stage = VoteSubmissionStageV1::decode(read_u8(bytes, 18)?)?;
    let job_id = read_b256(bytes, 19)?;
    let result_digest = read_b256(bytes, 51)?;
    let sender_address = Address::from_slice(read_slice(bytes, 83, 20)?);
    let nonce = read_u64(bytes, 103)?;
    let max_fee_per_gas = read_u128(bytes, 111)?;
    let transaction_hash = read_b256(bytes, 127)?;
    let raw_len = usize::try_from(read_u32(bytes, 159)?)
        .map_err(|_| VoteSubmissionErrorV1::JournalTooLarge)?;
    let (raw_start, raw_end) = verify_record_bounds_and_checksum(bytes, raw_len)?;
    let inclusion = decode_record_inclusion(bytes, raw_end)?;
    let record = VoteSubmissionRecordV1 {
        generation,
        stage,
        job_id,
        result_digest,
        sender_address,
        nonce,
        max_fee_per_gas,
        transaction_hash,
        raw_transaction: read_slice(bytes, raw_start, raw_len)?.to_vec(),
        inclusion,
    };
    validate_record(&record)?;
    Ok(record)
}

fn verify_record_bounds_and_checksum(
    bytes: &[u8],
    raw_len: usize,
) -> Result<(usize, usize), VoteSubmissionErrorV1> {
    if raw_len == 0 || raw_len > MAX_RAW_TRANSACTION_BYTES {
        return Err(VoteSubmissionErrorV1::JournalTooLarge);
    }
    let raw_start: usize = 163;
    let raw_end = raw_start
        .checked_add(raw_len)
        .ok_or(VoteSubmissionErrorV1::JournalTooLarge)?;
    let trailer_end = raw_end
        .checked_add(1 + 8 + 32 + 1)
        .ok_or(VoteSubmissionErrorV1::JournalTooLarge)?;
    let expected_len = trailer_end
        .checked_add(32)
        .ok_or(VoteSubmissionErrorV1::JournalTooLarge)?;
    if expected_len != bytes.len() {
        return Err(VoteSubmissionErrorV1::InvalidJournal);
    }
    let checksum = read_b256(bytes, trailer_end)?;
    if keccak256(&bytes[..trailer_end]) != checksum {
        return Err(VoteSubmissionErrorV1::JournalChecksumMismatch);
    }
    Ok((raw_start, raw_end))
}

fn decode_record_inclusion(
    bytes: &[u8],
    raw_end: usize,
) -> Result<Option<VoteInclusionV1>, VoteSubmissionErrorV1> {
    let has_inclusion = read_u8(bytes, raw_end)?;
    let block_number = read_u64(bytes, raw_end + 1)?;
    let block_hash = read_b256(bytes, raw_end + 9)?;
    let success = read_u8(bytes, raw_end + 41)?;
    match (has_inclusion, success) {
        (0, 0) if block_number == 0 && block_hash == B256::ZERO => Ok(None),
        (1, 0 | 1) => Ok(Some(VoteInclusionV1 {
            block_number,
            block_hash,
            success: success == 1,
        })),
        _ => Err(VoteSubmissionErrorV1::InvalidJournal),
    }
}

fn read_slice(bytes: &[u8], start: usize, len: usize) -> Result<&[u8], VoteSubmissionErrorV1> {
    bytes
        .get(start..start.saturating_add(len))
        .ok_or(VoteSubmissionErrorV1::InvalidJournal)
}

fn read_u8(bytes: &[u8], start: usize) -> Result<u8, VoteSubmissionErrorV1> {
    bytes
        .get(start)
        .copied()
        .ok_or(VoteSubmissionErrorV1::InvalidJournal)
}

fn read_u16(bytes: &[u8], start: usize) -> Result<u16, VoteSubmissionErrorV1> {
    Ok(u16::from_be_bytes(
        read_slice(bytes, start, 2)?
            .try_into()
            .map_err(|_| VoteSubmissionErrorV1::InvalidJournal)?,
    ))
}

fn read_u32(bytes: &[u8], start: usize) -> Result<u32, VoteSubmissionErrorV1> {
    Ok(u32::from_be_bytes(
        read_slice(bytes, start, 4)?
            .try_into()
            .map_err(|_| VoteSubmissionErrorV1::InvalidJournal)?,
    ))
}

fn read_u64(bytes: &[u8], start: usize) -> Result<u64, VoteSubmissionErrorV1> {
    Ok(u64::from_be_bytes(
        read_slice(bytes, start, 8)?
            .try_into()
            .map_err(|_| VoteSubmissionErrorV1::InvalidJournal)?,
    ))
}

fn read_u128(bytes: &[u8], start: usize) -> Result<u128, VoteSubmissionErrorV1> {
    Ok(u128::from_be_bytes(
        read_slice(bytes, start, 16)?
            .try_into()
            .map_err(|_| VoteSubmissionErrorV1::InvalidJournal)?,
    ))
}

fn read_b256(bytes: &[u8], start: usize) -> Result<B256, VoteSubmissionErrorV1> {
    Ok(B256::from_slice(read_slice(bytes, start, 32)?))
}

fn ensure_private_directory(path: &Path) -> Result<(), VoteSubmissionErrorV1> {
    fs::create_dir_all(path)
        .map_err(|source| io_error("create vote submission directory", path, source))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("stat vote submission directory", path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(VoteSubmissionErrorV1::UnsafePath(path.to_path_buf()));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|source| io_error("chmod vote submission directory", path, source))?;
    }
    Ok(())
}

fn path_exists(path: &Path) -> Result<bool, VoteSubmissionErrorV1> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                return Err(VoteSubmissionErrorV1::UnsafePath(path.to_path_buf()));
            }
            Ok(true)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("stat vote submission path", path, source)),
    }
}

fn open_regular_nofollow(path: &Path) -> Result<File, VoteSubmissionErrorV1> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| io_error("open vote submission journal", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("stat vote submission journal", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(VoteSubmissionErrorV1::UnsafePath(path.to_path_buf()));
    }
    Ok(file)
}

fn sync_directory(path: &Path) -> Result<(), VoteSubmissionErrorV1> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("fsync vote submission directory", path, source))
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> VoteSubmissionErrorV1 {
    VoteSubmissionErrorV1::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
