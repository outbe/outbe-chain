//! Bounded one-artifact-per-unit handoff from an isolated worker to its supervisor.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use alloy_primitives::B256;
use outbe_ocomp_protocol::CasObjectRefV1;
use sha3::{Digest, Keccak256};
use thiserror::Error;

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerInboxLimits {
    pub max_artifact_bytes: u64,
    pub max_total_bytes: u64,
}

impl WorkerInboxLimits {
    fn validate(self) -> Result<Self, WorkerInboxError> {
        if self.max_artifact_bytes == 0
            || self.max_total_bytes == 0
            || self.max_artifact_bytes > self.max_total_bytes
        {
            return Err(WorkerInboxError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Debug, Error)]
pub enum WorkerInboxError {
    #[error("worker inbox limits must be non-zero and artifact limit cannot exceed total limit")]
    InvalidLimits,
    #[error("worker artifact has {actual} bytes, exceeding artifact limit {limit}")]
    ArtifactLimitExceeded { limit: u64, actual: u64 },
    #[error("worker inbox write would use {attempted} bytes, exceeding total limit {limit}")]
    TotalLimitExceeded { limit: u64, attempted: u64 },
    #[error("worker inbox already contains an artifact for UnitId {unit_id}")]
    AlreadyStaged { unit_id: B256 },
    #[error("worker inbox contains conflicting bytes for UnitId {unit_id}")]
    ArtifactConflict { unit_id: B256 },
    #[error("worker inbox I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("worker inbox entry at {path} is not a safe regular no-follow file")]
    UnsafeArtifact { path: PathBuf },
    #[error("worker artifact length mismatch: expected {expected}, actual {actual}")]
    LengthMismatch { expected: u64, actual: u64 },
    #[error("worker artifact digest mismatch: expected {expected}, actual {actual}")]
    DigestMismatch { expected: B256, actual: B256 },
    #[error("worker artifact changed while the same descriptor was being verified")]
    ArtifactChanged,
    #[error("worker completion report contains a zero identity, length or digest")]
    InvalidReport,
    #[error("worker inbox byte count does not fit this host")]
    LengthOverflow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedWorkerArtifact {
    unit_id: B256,
    reference: CasObjectRefV1,
}

impl StagedWorkerArtifact {
    #[must_use]
    pub const fn unit_id(&self) -> B256 {
        self.unit_id
    }

    #[must_use]
    pub fn reference(&self) -> CasObjectRefV1 {
        self.reference.clone()
    }
}

#[derive(Debug)]
pub struct VerifiedWorkerArtifact {
    staged: StagedWorkerArtifact,
    bytes: Vec<u8>,
}

impl VerifiedWorkerArtifact {
    #[must_use]
    pub fn staged(&self) -> &StagedWorkerArtifact {
        &self.staged
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug)]
pub struct WorkerInbox {
    root: PathBuf,
    artifacts: PathBuf,
    staging: PathBuf,
    limits: WorkerInboxLimits,
}

impl WorkerInbox {
    pub fn open(
        root: impl AsRef<Path>,
        limits: WorkerInboxLimits,
    ) -> Result<Self, WorkerInboxError> {
        let limits = limits.validate()?;
        let root = root.as_ref().to_path_buf();
        create_directory(&root)?;
        reject_symlink_directory(&root)?;
        let artifacts = root.join("artifacts");
        let staging = root.join("staging");
        create_directory(&artifacts)?;
        create_directory(&staging)?;
        reject_symlink_directory(&artifacts)?;
        reject_symlink_directory(&staging)?;
        Ok(Self {
            root,
            artifacts,
            staging,
            limits,
        })
    }

    /// Publishes exactly one immutable artifact under a path derived solely
    /// from the already-validated canonical UnitId.
    pub fn stage(
        &self,
        unit_id: B256,
        bytes: &[u8],
    ) -> Result<StagedWorkerArtifact, WorkerInboxError> {
        let declared = u64::try_from(bytes.len()).map_err(|_| WorkerInboxError::LengthOverflow)?;
        if declared > self.limits.max_artifact_bytes {
            return Err(WorkerInboxError::ArtifactLimitExceeded {
                limit: self.limits.max_artifact_bytes,
                actual: declared,
            });
        }
        let _lock = InboxLock::acquire(&self.root)?;
        let attempted = self
            .used_bytes()?
            .checked_add(declared)
            .ok_or(WorkerInboxError::LengthOverflow)?;
        if attempted > self.limits.max_total_bytes {
            return Err(WorkerInboxError::TotalLimitExceeded {
                limit: self.limits.max_total_bytes,
                attempted,
            });
        }

        let staging_path = self.next_staging_path();
        let result = self.stage_inner(unit_id, bytes, declared, &staging_path);
        let _ = fs::remove_file(&staging_path);
        result
    }

    /// Adopts one deterministic execution result. A byte-identical retry is an
    /// idempotent cache hit; different bytes for the same UnitId are a
    /// consensus-safety conflict and are never allowed to replace the original.
    pub fn adopt(
        &self,
        unit_id: B256,
        bytes: &[u8],
    ) -> Result<StagedWorkerArtifact, WorkerInboxError> {
        let declared = u64::try_from(bytes.len()).map_err(|_| WorkerInboxError::LengthOverflow)?;
        if declared > self.limits.max_artifact_bytes {
            return Err(WorkerInboxError::ArtifactLimitExceeded {
                limit: self.limits.max_artifact_bytes,
                actual: declared,
            });
        }
        let candidate = staged_descriptor(unit_id, bytes)?;
        let _lock = InboxLock::acquire(&self.root)?;
        let artifact_path = self.artifact_path(unit_id);
        match fs::symlink_metadata(&artifact_path) {
            Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
                return match self.read_verified(&candidate) {
                    Ok(_) => Ok(candidate),
                    Err(
                        WorkerInboxError::LengthMismatch { .. }
                        | WorkerInboxError::DigestMismatch { .. },
                    ) => Err(WorkerInboxError::ArtifactConflict { unit_id }),
                    Err(error) => Err(error),
                };
            }
            Ok(_) => {
                return Err(WorkerInboxError::UnsafeArtifact {
                    path: artifact_path,
                });
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(WorkerInboxError::Io {
                    path: artifact_path,
                    source,
                });
            }
        }
        let attempted = self
            .used_bytes()?
            .checked_add(declared)
            .ok_or(WorkerInboxError::LengthOverflow)?;
        if attempted > self.limits.max_total_bytes {
            return Err(WorkerInboxError::TotalLimitExceeded {
                limit: self.limits.max_total_bytes,
                attempted,
            });
        }
        let staging_path = self.next_staging_path();
        let result = self.stage_inner(unit_id, bytes, declared, &staging_path);
        let _ = fs::remove_file(&staging_path);
        result
    }

    pub fn read_verified(
        &self,
        staged: &StagedWorkerArtifact,
    ) -> Result<VerifiedWorkerArtifact, WorkerInboxError> {
        if staged.reference.encoded_bytes > self.limits.max_artifact_bytes {
            return Err(WorkerInboxError::ArtifactLimitExceeded {
                limit: self.limits.max_artifact_bytes,
                actual: staged.reference.encoded_bytes,
            });
        }
        let path = self.artifact_path(staged.unit_id);
        let mut file = open_regular_nofollow(&path)?;
        let before = FileIdentity::read(&file, &path)?;
        if before.len != staged.reference.encoded_bytes {
            return Err(WorkerInboxError::LengthMismatch {
                expected: staged.reference.encoded_bytes,
                actual: before.len,
            });
        }
        let capacity = usize::try_from(before.len).map_err(|_| WorkerInboxError::LengthOverflow)?;
        let mut bytes = Vec::with_capacity(capacity);
        let mut hasher = Keccak256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|source| WorkerInboxError::Io {
                    path: path.clone(),
                    source,
                })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            bytes.extend_from_slice(&buffer[..read]);
        }
        let actual_len =
            u64::try_from(bytes.len()).map_err(|_| WorkerInboxError::LengthOverflow)?;
        if actual_len != staged.reference.encoded_bytes {
            return Err(WorkerInboxError::LengthMismatch {
                expected: staged.reference.encoded_bytes,
                actual: actual_len,
            });
        }
        if before != FileIdentity::read(&file, &path)? {
            return Err(WorkerInboxError::ArtifactChanged);
        }
        let actual_digest = finalize_digest(hasher);
        if actual_digest != staged.reference.transport_digest {
            return Err(WorkerInboxError::DigestMismatch {
                expected: staged.reference.transport_digest,
                actual: actual_digest,
            });
        }
        Ok(VerifiedWorkerArtifact {
            staged: staged.clone(),
            bytes,
        })
    }

    pub fn read_reported(
        &self,
        unit_id: B256,
        exact_staged_bytes: u64,
        transport_digest: B256,
    ) -> Result<VerifiedWorkerArtifact, WorkerInboxError> {
        if unit_id.is_zero() || exact_staged_bytes == 0 || transport_digest.is_zero() {
            return Err(WorkerInboxError::InvalidReport);
        }
        self.read_verified(&StagedWorkerArtifact {
            unit_id,
            reference: CasObjectRefV1 {
                transport_digest,
                encoded_bytes: exact_staged_bytes,
                expected_ocb1_kind: None,
            },
        })
    }

    pub fn artifact_count(&self) -> Result<usize, WorkerInboxError> {
        directory_file_count(&self.artifacts)
    }

    pub fn staging_count(&self) -> Result<usize, WorkerInboxError> {
        directory_file_count(&self.staging)
    }

    fn stage_inner(
        &self,
        unit_id: B256,
        bytes: &[u8],
        declared: u64,
        staging_path: &Path,
    ) -> Result<StagedWorkerArtifact, WorkerInboxError> {
        let mut staging = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(staging_path)
            .map_err(|source| WorkerInboxError::Io {
                path: staging_path.to_path_buf(),
                source,
            })?;
        let mut hasher = Keccak256::new();
        for chunk in bytes.chunks(64 * 1024) {
            staging
                .write_all(chunk)
                .map_err(|source| WorkerInboxError::Io {
                    path: staging_path.to_path_buf(),
                    source,
                })?;
            hasher.update(chunk);
        }
        staging.sync_all().map_err(|source| WorkerInboxError::Io {
            path: staging_path.to_path_buf(),
            source,
        })?;
        let staged = StagedWorkerArtifact {
            unit_id,
            reference: CasObjectRefV1 {
                transport_digest: finalize_digest(hasher),
                encoded_bytes: declared,
                expected_ocb1_kind: None,
            },
        };
        let artifact_path = self.artifact_path(unit_id);
        match fs::symlink_metadata(&artifact_path) {
            Ok(metadata) => {
                return if metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
                    Err(WorkerInboxError::AlreadyStaged { unit_id })
                } else {
                    Err(WorkerInboxError::UnsafeArtifact {
                        path: artifact_path,
                    })
                };
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(WorkerInboxError::Io {
                    path: artifact_path,
                    source,
                });
            }
        }
        match fs::hard_link(staging_path, &artifact_path) {
            Ok(()) => {
                sync_directory(&self.artifacts)?;
                fs::remove_file(staging_path).map_err(|source| WorkerInboxError::Io {
                    path: staging_path.to_path_buf(),
                    source,
                })?;
                sync_directory(&self.staging)?;
                self.read_verified(&staged)?;
                Ok(staged)
            }
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&artifact_path).map_err(|source| {
                    WorkerInboxError::Io {
                        path: artifact_path.clone(),
                        source,
                    }
                })?;
                if metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
                    Err(WorkerInboxError::AlreadyStaged { unit_id })
                } else {
                    Err(WorkerInboxError::UnsafeArtifact {
                        path: artifact_path,
                    })
                }
            }
            Err(source) => Err(WorkerInboxError::Io {
                path: artifact_path,
                source,
            }),
        }
    }

    fn artifact_path(&self, unit_id: B256) -> PathBuf {
        self.artifacts
            .join(format!("{}.ocb1", hex::encode(unit_id.as_slice())))
    }

    fn next_staging_path(&self) -> PathBuf {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.staging
            .join(format!("{}-{sequence}.tmp", std::process::id()))
    }

    fn used_bytes(&self) -> Result<u64, WorkerInboxError> {
        let mut total = 0_u64;
        for entry in read_directory(&self.artifacts)? {
            let entry = entry.map_err(|source| WorkerInboxError::Io {
                path: self.artifacts.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| WorkerInboxError::Io {
                path: path.clone(),
                source,
            })?;
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Err(WorkerInboxError::UnsafeArtifact { path });
            }
            total = total
                .checked_add(metadata.len())
                .ok_or(WorkerInboxError::LengthOverflow)?;
        }
        Ok(total)
    }
}

fn staged_descriptor(
    unit_id: B256,
    bytes: &[u8],
) -> Result<StagedWorkerArtifact, WorkerInboxError> {
    let encoded_bytes =
        u64::try_from(bytes.len()).map_err(|_| WorkerInboxError::LengthOverflow)?;
    let mut hasher = Keccak256::new();
    hasher.update(bytes);
    Ok(StagedWorkerArtifact {
        unit_id,
        reference: CasObjectRefV1 {
            transport_digest: finalize_digest(hasher),
            encoded_bytes,
            expected_ocb1_kind: None,
        },
    })
}

struct InboxLock {
    file: File,
}

impl InboxLock {
    #[allow(unsafe_code)]
    fn acquire(root: &Path) -> Result<Self, WorkerInboxError> {
        let path = root.join(".stage.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|source| WorkerInboxError::Io {
                path: path.clone(),
                source,
            })?;
        // SAFETY: `file` owns a live descriptor for the duration of the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(WorkerInboxError::Io {
                path,
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(Self { file })
    }
}

impl Drop for InboxLock {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `self.file` remains open until after this drop body.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    len: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileIdentity {
    fn read(file: &File, path: &Path) -> Result<Self, WorkerInboxError> {
        let metadata = file.metadata().map_err(|source| WorkerInboxError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if !metadata.file_type().is_file() {
            return Err(WorkerInboxError::UnsafeArtifact {
                path: path.to_path_buf(),
            });
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            len: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
}

fn open_regular_nofollow(path: &Path) -> Result<File, WorkerInboxError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| {
            if source.raw_os_error() == Some(libc::ELOOP) {
                WorkerInboxError::UnsafeArtifact {
                    path: path.to_path_buf(),
                }
            } else {
                WorkerInboxError::Io {
                    path: path.to_path_buf(),
                    source,
                }
            }
        })
}

fn directory_file_count(path: &Path) -> Result<usize, WorkerInboxError> {
    read_directory(path)?.try_fold(0_usize, |count, entry| {
        let entry = entry.map_err(|source| WorkerInboxError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let entry_path = entry.path();
        let metadata =
            fs::symlink_metadata(&entry_path).map_err(|source| WorkerInboxError::Io {
                path: entry_path.clone(),
                source,
            })?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(WorkerInboxError::UnsafeArtifact { path: entry_path });
        }
        count.checked_add(1).ok_or(WorkerInboxError::LengthOverflow)
    })
}

fn reject_symlink_directory(path: &Path) -> Result<(), WorkerInboxError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| WorkerInboxError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(WorkerInboxError::UnsafeArtifact {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn create_directory(path: &Path) -> Result<(), WorkerInboxError> {
    fs::create_dir_all(path).map_err(|source| WorkerInboxError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read_directory(path: &Path) -> Result<fs::ReadDir, WorkerInboxError> {
    fs::read_dir(path).map_err(|source| WorkerInboxError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn sync_directory(path: &Path) -> Result<(), WorkerInboxError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| WorkerInboxError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn finalize_digest(hasher: Keccak256) -> B256 {
    B256::from_slice(&hasher.finalize())
}
