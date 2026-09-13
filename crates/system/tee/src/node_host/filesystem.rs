use crate::TransportError;

use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;

use std::fs;
use std::fs::DirBuilder;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::path::PathBuf;

pub const NODE_HOST_DIRECTORY_V1: &str = "tee-node-host-v1";

pub const NODE_HOST_NOISE_KEY_V1: &str = "noise-initiator.key";

pub const NODE_HOST_MANIFEST_V1: &str = "initialization-manifest.bin";

const NODE_HOST_PENDING_MANIFEST_V1: &str = "initialization-manifest.pending";

pub const NODE_HOST_REPLACEMENT_CANDIDATE_V1: &str = "replacement-candidate.v1";

pub const NODE_HOST_REPLACEMENT_SUBMISSION_V1: &str = "replacement-submission.v1";

pub const NODE_HOST_REPLACEMENT_RELAY_V1: &str = "replacement-relay.v1";

pub const NODE_HOST_REPLACEMENT_PROMOTION_V1: &str = "replacement-promotion.v1";

pub const NODE_HOST_COMMITTED_JOIN_SUBMISSION_V1: &str = "committed-join-submission.v1";

pub const NODE_HOST_COMMITTED_JOIN_RELAY_V1: &str = "committed-join-relay.v1";

pub const NODE_HOST_FINALIZED_JOIN_ADMISSION_ANCHOR_V1: &str = "finalized-join-admission-anchor.v1";

const NODE_HOST_NEXT_MANIFEST_V1: &str = "initialization-manifest.next";

const NODE_HOST_REPLACEMENT_CANDIDATE_NEXT_V1: &str = "replacement-candidate.next";

const NODE_HOST_REPLACEMENT_SUBMISSION_NEXT_V1: &str = "replacement-submission.next";

const NODE_HOST_REPLACEMENT_RELAY_NEXT_V1: &str = "replacement-relay.next";

const NODE_HOST_REPLACEMENT_PROMOTION_NEXT_V1: &str = "replacement-promotion.next";

const NODE_HOST_COMMITTED_JOIN_SUBMISSION_NEXT_V1: &str = "committed-join-submission.next";

const NODE_HOST_COMMITTED_JOIN_RELAY_NEXT_V1: &str = "committed-join-relay.next";

const NODE_HOST_FINALIZED_JOIN_ADMISSION_ANCHOR_NEXT_V1: &str =
    "finalized-join-admission-anchor.next";

const NODE_HOST_STATE_LOCK_V1: &str = "state.lock";

const NODE_HOST_REPLACEMENT_WRITE_SCRATCH_V1: &str = "replacement-write.tmp";

const NODE_HOST_COMMITTED_JOIN_WRITE_SCRATCH_V1: &str = "committed-join-write.tmp";

const NODE_HOST_FINALIZED_JOIN_ADMISSION_ANCHOR_SCRATCH_V1: &str =
    "finalized-join-admission-anchor.tmp";

pub(super) fn read_owned_bounded_file(
    path: &Path,
    maximum_len: u64,
    label: &'static str,
) -> Result<Vec<u8>, TransportError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.len() > maximum_len
    {
        return Err(TransportError::Codec(format!(
            "NodeHost {label} must be an owner-only bounded regular file"
        )));
    }
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| TransportError::Codec(format!("NodeHost {label} length overflow")))?;
    let read_limit = maximum_len
        .checked_add(1)
        .ok_or_else(|| TransportError::Codec(format!("NodeHost {label} read limit overflow")))?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(read_limit).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_len {
        return Err(TransportError::Codec(format!(
            "NodeHost {label} grew beyond its byte bound while being read"
        )));
    }
    Ok(bytes)
}

pub(super) struct NodeHostStateLock {
    _file: File,
}

impl NodeHostStateLock {
    pub(super) fn acquire(path: &Path) -> Result<Self, TransportError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o777 != 0o600
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(TransportError::Codec(
                "NodeHost state lock must be an owner-only regular file".into(),
            ));
        }
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .map_err(std::io::Error::other)?;
        Ok(Self { _file: file })
    }
}

pub(super) fn write_manifest_once(
    path: &Path,
    manifest: &EnclaveInitializationManifestV1,
    directory: &Path,
) -> Result<(), TransportError> {
    let bytes = manifest
        .encode_canonical()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

pub(super) fn write_bytes_once(
    path: &Path,
    bytes: &[u8],
    directory: &Path,
) -> Result<(), TransportError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

pub(super) fn write_bytes_once_or_exact(
    path: &Path,
    scratch_path: &Path,
    bytes: &[u8],
    maximum_len: u64,
    directory: &Path,
    label: &'static str,
) -> Result<(), TransportError> {
    if path_exists(path)? {
        if read_owned_bounded_file(path, maximum_len, label)? == bytes {
            return Ok(());
        }
        return Err(TransportError::Codec(format!(
            "durable NodeHost {label} conflicts with the requested value"
        )));
    }
    stage_complete_bytes(path, scratch_path, bytes, directory)
}

pub(super) fn replace_bytes_atomically(
    path: &Path,
    next_path: &Path,
    scratch_path: &Path,
    bytes: &[u8],
    directory: &Path,
) -> Result<(), TransportError> {
    remove_file_if_exists(next_path)?;
    stage_complete_bytes(next_path, scratch_path, bytes, directory)?;
    fs::rename(next_path, path)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn stage_complete_bytes(
    path: &Path,
    scratch_path: &Path,
    bytes: &[u8],
    directory: &Path,
) -> Result<(), TransportError> {
    remove_file_if_exists(scratch_path)?;
    write_bytes_once(scratch_path, bytes, directory)?;
    fs::rename(scratch_path, path)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

pub(super) fn remove_file_if_exists(path: &Path) -> Result<(), TransportError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn ensure_private_directory(path: &Path) -> Result<(), TransportError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir()
                || metadata.permissions().mode() & 0o777 != 0o700
                || metadata.uid() != rustix::process::geteuid().as_raw()
            {
                return Err(TransportError::Codec(
                    "NodeHost state path must be a current-user directory with mode 0700".into(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DirBuilder::new().mode(0o700).create(path)?;
            if let Some(parent) = path.parent() {
                File::open(parent)?.sync_all()?;
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(super) fn path_exists(path: &Path) -> Result<bool, TransportError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(super) struct NodeHostPaths {
    pub(super) root: PathBuf,
    pub(super) state_lock: PathBuf,
    pub(super) noise_key: PathBuf,
    pub(super) manifest: PathBuf,
    pub(super) pending_manifest: PathBuf,
    pub(super) replacement_candidate: PathBuf,
    pub(super) replacement_submission: PathBuf,
    pub(super) replacement_relay: PathBuf,
    pub(super) replacement_promotion: PathBuf,
    pub(super) committed_join_submission: PathBuf,
    pub(super) committed_join_relay: PathBuf,
    pub(super) finalized_join_admission_anchor: PathBuf,
    pub(super) next_manifest: PathBuf,
    pub(super) replacement_candidate_next: PathBuf,
    pub(super) replacement_submission_next: PathBuf,
    pub(super) replacement_relay_next: PathBuf,
    pub(super) replacement_promotion_next: PathBuf,
    pub(super) committed_join_submission_next: PathBuf,
    pub(super) committed_join_relay_next: PathBuf,
    pub(super) finalized_join_admission_anchor_next: PathBuf,
    pub(super) replacement_write_scratch: PathBuf,
    pub(super) committed_join_write_scratch: PathBuf,
    pub(super) finalized_join_admission_anchor_scratch: PathBuf,
}

impl NodeHostPaths {
    pub(super) fn new(node_data_dir: &Path) -> Self {
        let root = node_data_dir.join(NODE_HOST_DIRECTORY_V1);
        Self {
            state_lock: root.join(NODE_HOST_STATE_LOCK_V1),
            noise_key: root.join(NODE_HOST_NOISE_KEY_V1),
            manifest: root.join(NODE_HOST_MANIFEST_V1),
            pending_manifest: root.join(NODE_HOST_PENDING_MANIFEST_V1),
            replacement_candidate: root.join(NODE_HOST_REPLACEMENT_CANDIDATE_V1),
            replacement_submission: root.join(NODE_HOST_REPLACEMENT_SUBMISSION_V1),
            replacement_relay: root.join(NODE_HOST_REPLACEMENT_RELAY_V1),
            replacement_promotion: root.join(NODE_HOST_REPLACEMENT_PROMOTION_V1),
            committed_join_submission: root.join(NODE_HOST_COMMITTED_JOIN_SUBMISSION_V1),
            committed_join_relay: root.join(NODE_HOST_COMMITTED_JOIN_RELAY_V1),
            finalized_join_admission_anchor: root
                .join(NODE_HOST_FINALIZED_JOIN_ADMISSION_ANCHOR_V1),
            next_manifest: root.join(NODE_HOST_NEXT_MANIFEST_V1),
            replacement_candidate_next: root.join(NODE_HOST_REPLACEMENT_CANDIDATE_NEXT_V1),
            replacement_submission_next: root.join(NODE_HOST_REPLACEMENT_SUBMISSION_NEXT_V1),
            replacement_relay_next: root.join(NODE_HOST_REPLACEMENT_RELAY_NEXT_V1),
            replacement_promotion_next: root.join(NODE_HOST_REPLACEMENT_PROMOTION_NEXT_V1),
            committed_join_submission_next: root.join(NODE_HOST_COMMITTED_JOIN_SUBMISSION_NEXT_V1),
            committed_join_relay_next: root.join(NODE_HOST_COMMITTED_JOIN_RELAY_NEXT_V1),
            finalized_join_admission_anchor_next: root
                .join(NODE_HOST_FINALIZED_JOIN_ADMISSION_ANCHOR_NEXT_V1),
            replacement_write_scratch: root.join(NODE_HOST_REPLACEMENT_WRITE_SCRATCH_V1),
            committed_join_write_scratch: root.join(NODE_HOST_COMMITTED_JOIN_WRITE_SCRATCH_V1),
            finalized_join_admission_anchor_scratch: root
                .join(NODE_HOST_FINALIZED_JOIN_ADMISSION_ANCHOR_SCRATCH_V1),
            root,
        }
    }
}
