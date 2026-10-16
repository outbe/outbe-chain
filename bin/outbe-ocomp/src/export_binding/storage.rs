use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use alloy_primitives::{keccak256, B256};
use outbe_ocomp_protocol::{input::CheckpointIdentityV1, CasObjectRefV1};

pub(crate) const FORMAT_VERSION: u16 = 1;
pub(crate) const LOCATOR_FIXED_BYTES: usize = 8 + 2 + 32 + 8 + 32;
const CONFLICT_FIXED_BYTES: usize = 8 + 32 + 32 + 32;

#[derive(Debug)]
pub(crate) enum BindingStorageError {
    AuthorityMismatch(&'static str),
    InvalidEnvelope,
    UnexpectedEntry(PathBuf),
    UnsafePath(PathBuf),
    LockHeld(PathBuf),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

macro_rules! impl_from_binding_storage_error {
    ($target:ty) => {
        impl From<BindingStorageError> for $target {
            fn from(error: BindingStorageError) -> Self {
                match error {
                    BindingStorageError::AuthorityMismatch(what) => Self::AuthorityMismatch(what),
                    BindingStorageError::InvalidEnvelope => Self::InvalidEnvelope,
                    BindingStorageError::UnexpectedEntry(path) => Self::UnexpectedEntry(path),
                    BindingStorageError::UnsafePath(path) => Self::UnsafePath(path),
                    BindingStorageError::LockHeld(path) => Self::LockHeld(path),
                    BindingStorageError::Io {
                        operation,
                        path,
                        source,
                    } => Self::Io {
                        operation,
                        path,
                        source,
                    },
                }
            }
        }
    };
}

impl_from_binding_storage_error!(crate::export_binding::ExportBindingError);
impl_from_binding_storage_error!(crate::export_receipt::ExportReceiptError);

pub(crate) fn verify_envelope_checksum(
    encoded: &[u8],
    fixed_bytes: usize,
    magic: [u8; 8],
    checksum_offset: usize,
) -> Result<(), BindingStorageError> {
    if encoded.len() != fixed_bytes
        || encoded[..8] != magic
        || read_u16(encoded, 8)? != FORMAT_VERSION
    {
        return Err(BindingStorageError::InvalidEnvelope);
    }
    if keccak256(&encoded[..checksum_offset]) != B256::from_slice(&encoded[checksum_offset..]) {
        return Err(BindingStorageError::InvalidEnvelope);
    }
    Ok(())
}

pub(crate) fn encode_checkpoint(output: &mut Vec<u8>, checkpoint: &CheckpointIdentityV1) {
    output.extend_from_slice(&checkpoint.finalized_block_number.to_be_bytes());
    output.extend_from_slice(checkpoint.finalized_block_hash.as_slice());
    output.extend_from_slice(checkpoint.finalized_state_root.as_slice());
}

pub(crate) fn decode_checkpoint_at(
    encoded: &[u8],
    offset: usize,
) -> Result<CheckpointIdentityV1, BindingStorageError> {
    Ok(CheckpointIdentityV1 {
        finalized_block_number: read_u64(encoded, offset)?,
        finalized_block_hash: read_b256(encoded, offset + 8)?,
        finalized_state_root: read_b256(encoded, offset + 40)?,
    })
}

pub(crate) fn encode_manifest_ref(output: &mut Vec<u8>, reference: &CasObjectRefV1) {
    output.extend_from_slice(reference.transport_digest.as_slice());
    output.extend_from_slice(&reference.encoded_bytes.to_be_bytes());
    output.extend_from_slice(
        &reference
            .expected_ocb1_kind
            .unwrap_or_default()
            .to_be_bytes(),
    );
}

pub(crate) fn decode_manifest_ref_at(
    encoded: &[u8],
    offset: usize,
) -> Result<(CasObjectRefV1, u16), BindingStorageError> {
    let expected_kind = read_u16(encoded, offset + 40)?;
    let reference = CasObjectRefV1 {
        transport_digest: read_b256(encoded, offset)?,
        encoded_bytes: read_u64(encoded, offset + 32)?,
        expected_ocb1_kind: Some(expected_kind),
    };
    Ok((reference, expected_kind))
}

pub(crate) fn encode_locator_with_magic(
    reference: &CasObjectRefV1,
    magic: [u8; 8],
    what: &'static str,
) -> Result<Vec<u8>, BindingStorageError> {
    if reference.expected_ocb1_kind.is_some()
        || reference.transport_digest.is_zero()
        || reference.encoded_bytes == 0
    {
        return Err(BindingStorageError::AuthorityMismatch(what));
    }
    let mut encoded = Vec::with_capacity(LOCATOR_FIXED_BYTES);
    encoded.extend_from_slice(&magic);
    encoded.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    encoded.extend_from_slice(reference.transport_digest.as_slice());
    encoded.extend_from_slice(&reference.encoded_bytes.to_be_bytes());
    encoded.extend_from_slice(keccak256(&encoded).as_slice());
    Ok(encoded)
}

pub(crate) fn decode_locator_with_magic(
    encoded: &[u8],
    magic: [u8; 8],
    what: &'static str,
) -> Result<CasObjectRefV1, BindingStorageError> {
    verify_envelope_checksum(encoded, LOCATOR_FIXED_BYTES, magic, 50)?;
    let reference = CasObjectRefV1 {
        transport_digest: read_b256(encoded, 10)?,
        encoded_bytes: read_u64(encoded, 42)?,
        expected_ocb1_kind: None,
    };
    if reference.transport_digest.is_zero() || reference.encoded_bytes == 0 {
        return Err(BindingStorageError::AuthorityMismatch(what));
    }
    Ok(reference)
}

pub(crate) fn latch_conflict_file(
    root: &Path,
    conflict_file: &str,
    file_mode: u32,
    magic: [u8; 8],
    conflict_pair: (B256, B256),
) -> Result<(), BindingStorageError> {
    let path = root.join(conflict_file);
    if !path_exists(&path)? {
        let mut encoded = Vec::with_capacity(CONFLICT_FIXED_BYTES);
        encoded.extend_from_slice(&magic);
        encoded.extend_from_slice(conflict_pair.0.as_slice());
        encoded.extend_from_slice(conflict_pair.1.as_slice());
        encoded.extend_from_slice(keccak256(&encoded).as_slice());
        persist_new(root, &path, file_mode, &encoded)?;
    }
    Ok(())
}

pub(crate) fn read_bounded(path: &Path, cap: usize) -> Result<Vec<u8>, BindingStorageError> {
    let mut file = open_regular_nofollow(path)?;
    let len = usize::try_from(
        file.metadata()
            .map_err(|source| io_error("stat file", path, source))?
            .len(),
    )
    .map_err(|_| BindingStorageError::InvalidEnvelope)?;
    if len > cap {
        return Err(BindingStorageError::InvalidEnvelope);
    }
    let read_limit = u64::try_from(len)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(BindingStorageError::InvalidEnvelope)?;
    let mut encoded = Vec::with_capacity(len);
    (&mut file)
        .take(read_limit)
        .read_to_end(&mut encoded)
        .map_err(|source| io_error("read file", path, source))?;
    if encoded.len() != len {
        return Err(BindingStorageError::InvalidEnvelope);
    }
    Ok(encoded)
}

pub(crate) fn create_private_directory(
    path: &Path,
    dir_mode: u32,
) -> Result<(), BindingStorageError> {
    fs::create_dir_all(path).map_err(|source| io_error("create directory", path, source))?;
    inspect_private_directory(path)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(dir_mode))
        .map_err(|source| io_error("set directory permissions", path, source))
}

pub(crate) fn inspect_private_directory(path: &Path) -> Result<(), BindingStorageError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect directory", path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(BindingStorageError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

pub(crate) fn directory_exists(path: &Path) -> Result<bool, BindingStorageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(true)
        }
        Ok(_) => Err(BindingStorageError::UnsafePath(path.to_path_buf())),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("inspect directory", path, source)),
    }
}

pub(crate) fn inspect_known_entries(
    root: &Path,
    allowed: &[&str],
) -> Result<(), BindingStorageError> {
    for entry in fs::read_dir(root).map_err(|source| io_error("list directory", root, source))? {
        let entry = entry.map_err(|source| io_error("read directory", root, source))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| BindingStorageError::UnsafePath(entry.path()))?;
        if !allowed.contains(&name.as_str()) {
            return Err(BindingStorageError::UnexpectedEntry(entry.path()));
        }
        let metadata = entry
            .metadata()
            .map_err(|source| io_error("inspect entry", &entry.path(), source))?;
        if !metadata.file_type().is_file() {
            return Err(BindingStorageError::UnsafePath(entry.path()));
        }
    }
    Ok(())
}

pub(crate) fn path_exists(path: &Path) -> Result<bool, BindingStorageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            Ok(true)
        }
        Ok(_) => Err(BindingStorageError::UnsafePath(path.to_path_buf())),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("stat path", path, source)),
    }
}

fn open_regular_nofollow(path: &Path) -> Result<File, BindingStorageError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| io_error("open file", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect file", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(BindingStorageError::UnsafePath(path.to_path_buf()));
    }
    Ok(file)
}

pub(crate) fn persist_atomic(
    root: &Path,
    temp: &Path,
    final_path: &Path,
    file_mode: u32,
    encoded: &[u8],
) -> Result<(), BindingStorageError> {
    let result = (|| {
        write_synced_new(
            temp,
            file_mode,
            encoded,
            (
                "create temporary file",
                "write temporary file",
                "fsync temporary file",
            ),
        )?;
        fs::rename(temp, final_path)
            .map_err(|source| io_error("publish file", final_path, source))?;
        sync_directory(root)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

pub(crate) fn persist_new(
    root: &Path,
    path: &Path,
    file_mode: u32,
    encoded: &[u8],
) -> Result<(), BindingStorageError> {
    write_synced_new(
        path,
        file_mode,
        encoded,
        ("create file", "write file", "fsync file"),
    )?;
    sync_directory(root)
}

fn write_synced_new(
    path: &Path,
    file_mode: u32,
    encoded: &[u8],
    ops: (&'static str, &'static str, &'static str),
) -> Result<(), BindingStorageError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(file_mode)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| io_error(ops.0, path, source))?;
    file.write_all(encoded)
        .map_err(|source| io_error(ops.1, path, source))?;
    file.sync_all()
        .map_err(|source| io_error(ops.2, path, source))
}

fn sync_directory(path: &Path) -> Result<(), BindingStorageError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("fsync directory", path, source))
}

pub(crate) fn read_u16(encoded: &[u8], start: usize) -> Result<u16, BindingStorageError> {
    Ok(u16::from_be_bytes(
        encoded
            .get(start..start + 2)
            .ok_or(BindingStorageError::InvalidEnvelope)?
            .try_into()
            .map_err(|_| BindingStorageError::InvalidEnvelope)?,
    ))
}

pub(crate) fn read_u32(encoded: &[u8], start: usize) -> Result<u32, BindingStorageError> {
    Ok(u32::from_be_bytes(
        encoded
            .get(start..start + 4)
            .ok_or(BindingStorageError::InvalidEnvelope)?
            .try_into()
            .map_err(|_| BindingStorageError::InvalidEnvelope)?,
    ))
}

pub(crate) fn read_u64(encoded: &[u8], start: usize) -> Result<u64, BindingStorageError> {
    Ok(u64::from_be_bytes(
        encoded
            .get(start..start + 8)
            .ok_or(BindingStorageError::InvalidEnvelope)?
            .try_into()
            .map_err(|_| BindingStorageError::InvalidEnvelope)?,
    ))
}

pub(crate) fn read_b256(encoded: &[u8], start: usize) -> Result<B256, BindingStorageError> {
    let bytes = encoded
        .get(start..start + 32)
        .ok_or(BindingStorageError::InvalidEnvelope)?;
    Ok(B256::from_slice(bytes))
}

pub(crate) fn io_error(
    operation: &'static str,
    path: &Path,
    source: std::io::Error,
) -> BindingStorageError {
    BindingStorageError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

pub(crate) struct ExclusiveFileLock {
    file: File,
}

impl ExclusiveFileLock {
    #[allow(unsafe_code)]
    pub(crate) fn acquire(
        root: &Path,
        lock_file: &str,
        file_mode: u32,
        open_op: &'static str,
    ) -> Result<Self, BindingStorageError> {
        let path = root.join(lock_file);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(file_mode)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|source| io_error(open_op, &path, source))?;
        // SAFETY: `file` owns a live descriptor for the complete flock call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(BindingStorageError::LockHeld(path));
        }
        Ok(Self { file })
    }
}

impl Drop for ExclusiveFileLock {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `self.file` remains open for the complete flock call.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}
