use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use alloy_primitives::{keccak256, B256};
use outbe_ocomp_protocol::{CanonicalReader, CanonicalWriter, CasObjectRefV1, ProtocolError};

const LOCK_FILE: &str = "catalog.lock";
const TEMP_SUFFIX: &str = ".tmp";

#[derive(Debug)]
pub(crate) enum CatalogStorageError {
    MissingHeader,
    AmbiguousTemporary(PathBuf),
    LockHeld(PathBuf),
    ObjectTooLarge,
    InvalidEnvelope,
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct CatalogStorageProfile {
    pub file_mode: u32,
    pub create_temp_op: &'static str,
    pub write_temp_op: &'static str,
    pub fsync_temp_op: &'static str,
    pub install_op: &'static str,
    pub open_read_op: &'static str,
    pub inspect_read_op: &'static str,
    pub read_op: &'static str,
}

pub(crate) const ADMISSION_STORAGE_PROFILE: CatalogStorageProfile = CatalogStorageProfile {
    file_mode: 0o600,
    create_temp_op: "create temporary admission",
    write_temp_op: "write temporary admission",
    fsync_temp_op: "fsync temporary admission",
    install_op: "install admission",
    open_read_op: "open admission",
    inspect_read_op: "inspect admission",
    read_op: "read admission",
};

pub(crate) const INPUT_REF_STORAGE_PROFILE: CatalogStorageProfile = CatalogStorageProfile {
    file_mode: 0o640,
    create_temp_op: "create temporary catalog object",
    write_temp_op: "write temporary catalog object",
    fsync_temp_op: "fsync temporary catalog object",
    install_op: "install catalog object",
    open_read_op: "open catalog object",
    inspect_read_op: "inspect catalog object",
    read_op: "read catalog object",
};

impl CatalogStorageProfile {
    pub(crate) fn persist_atomic(
        self,
        root: &Path,
        target: &Path,
        bytes: &[u8],
    ) -> Result<(), CatalogStorageError> {
        let file_name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(CatalogStorageError::InvalidEnvelope)?;
        let temp = root.join(format!("{file_name}{TEMP_SUFFIX}"));
        if path_exists(&temp)? {
            return Err(CatalogStorageError::AmbiguousTemporary(temp));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(self.file_mode)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temp)
            .map_err(|source| storage_io_error(self.create_temp_op, &temp, source))?;
        file.write_all(bytes)
            .map_err(|source| storage_io_error(self.write_temp_op, &temp, source))?;
        file.sync_all()
            .map_err(|source| storage_io_error(self.fsync_temp_op, &temp, source))?;
        fs::rename(&temp, target)
            .map_err(|source| storage_io_error(self.install_op, target, source))?;
        sync_directory(root)
    }

    pub(crate) fn read_bounded(
        self,
        path: &Path,
        max_bytes: usize,
    ) -> Result<Vec<u8>, CatalogStorageError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|source| storage_io_error(self.open_read_op, path, source))?;
        let metadata = file
            .metadata()
            .map_err(|source| storage_io_error(self.inspect_read_op, path, source))?;
        let max_len = u64::try_from(max_bytes).unwrap_or(u64::MAX);
        if !metadata.file_type().is_file() || metadata.len() > max_len {
            return Err(CatalogStorageError::ObjectTooLarge);
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        file.take(max_len.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|source| storage_io_error(self.read_op, path, source))?;
        if bytes.len() > max_bytes {
            return Err(CatalogStorageError::ObjectTooLarge);
        }
        Ok(bytes)
    }

    pub(crate) fn acquire_lock(self, root: &Path) -> Result<CatalogLock, CatalogStorageError> {
        CatalogLock::acquire(root, self.file_mode)
    }

    pub(crate) fn load_or_init_header(
        self,
        root: &Path,
        header_path: &Path,
        max_bytes: usize,
        encoded_expected: &[u8],
    ) -> Result<Option<Vec<u8>>, CatalogStorageError> {
        if path_exists(header_path)? {
            return Ok(Some(self.read_bounded(header_path, max_bytes)?));
        }
        require_fresh_catalog_without_header(root)?;
        self.persist_atomic(root, header_path, encoded_expected)?;
        Ok(None)
    }
}

macro_rules! impl_from_catalog_storage_error {
    ($target:ty) => {
        impl From<CatalogStorageError> for $target {
            fn from(error: CatalogStorageError) -> Self {
                match error {
                    CatalogStorageError::MissingHeader => Self::MissingHeader,
                    CatalogStorageError::AmbiguousTemporary(path) => Self::AmbiguousTemporary(path),
                    CatalogStorageError::LockHeld(path) => Self::LockHeld(path),
                    CatalogStorageError::ObjectTooLarge => Self::ObjectTooLarge,
                    CatalogStorageError::InvalidEnvelope => Self::InvalidEnvelope,
                    CatalogStorageError::Io {
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

impl_from_catalog_storage_error!(crate::admission_catalog::AdmissionCatalogError);
impl_from_catalog_storage_error!(crate::input_ref_catalog::InputRefCatalogError);

pub(crate) fn reject_orphaned_temps(root: &Path) -> Result<(), CatalogStorageError> {
    let entries = fs::read_dir(root)
        .map_err(|source| storage_io_error("list catalog directory", root, source))?;
    for entry in entries {
        let entry =
            entry.map_err(|source| storage_io_error("read catalog directory", root, source))?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(TEMP_SUFFIX))
        {
            return Err(CatalogStorageError::AmbiguousTemporary(path));
        }
    }
    Ok(())
}

pub(crate) fn require_fresh_catalog_without_header(root: &Path) -> Result<(), CatalogStorageError> {
    let entries = fs::read_dir(root)
        .map_err(|source| storage_io_error("list catalog directory", root, source))?;
    for entry in entries {
        let entry =
            entry.map_err(|source| storage_io_error("read catalog directory", root, source))?;
        if entry.file_name() != LOCK_FILE {
            return Err(CatalogStorageError::MissingHeader);
        }
    }
    Ok(())
}

pub(crate) fn path_exists(path: &Path) -> Result<bool, CatalogStorageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Err(CatalogStorageError::InvalidEnvelope);
            }
            Ok(true)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(storage_io_error("inspect path", path, source)),
    }
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), CatalogStorageError> {
    let directory = File::open(path)
        .map_err(|source| storage_io_error("open catalog directory", path, source))?;
    directory
        .sync_all()
        .map_err(|source| storage_io_error("fsync catalog directory", path, source))
}

pub(crate) fn storage_io_error(
    operation: &'static str,
    path: &Path,
    source: std::io::Error,
) -> CatalogStorageError {
    CatalogStorageError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

pub(crate) fn encode_envelope(magic: [u8; 8], body: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(magic.len() + B256::len_bytes() + body.len());
    output.extend_from_slice(&magic);
    output.extend_from_slice(keccak256(body).as_slice());
    output.extend_from_slice(body);
    output
}

pub(crate) fn decode_envelope(
    encoded: &[u8],
    magic: [u8; 8],
) -> Result<&[u8], CatalogStorageError> {
    let body_offset = magic.len() + B256::len_bytes();
    if encoded.len() < body_offset || encoded[..magic.len()] != magic {
        return Err(CatalogStorageError::InvalidEnvelope);
    }
    let expected = B256::from_slice(&encoded[magic.len()..body_offset]);
    let body = &encoded[body_offset..];
    if keccak256(body) != expected {
        return Err(CatalogStorageError::InvalidEnvelope);
    }
    Ok(body)
}

pub(crate) fn encode_object_ref(
    output: &mut CanonicalWriter,
    reference: &CasObjectRefV1,
) -> Result<(), ProtocolError> {
    output.write_b256(reference.transport_digest)?;
    output.write_u64(reference.encoded_bytes)?;
    output.write_option(reference.expected_ocb1_kind.as_ref(), |writer, kind| {
        writer.write_u16(*kind)
    })
}

pub(crate) fn decode_object_ref(
    input: &mut CanonicalReader<'_>,
) -> Result<CasObjectRefV1, ProtocolError> {
    Ok(CasObjectRefV1 {
        transport_digest: input.read_b256()?,
        encoded_bytes: input.read_u64()?,
        expected_ocb1_kind: input.read_option(|reader| reader.read_u16())?,
    })
}

pub(crate) fn parse_entry_ordinal(prefix: &str) -> Option<u32> {
    if prefix.len() != 10 || !prefix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let ordinal = prefix.parse::<u32>().ok()?;
    (format!("{ordinal:010}") == prefix).then_some(ordinal)
}

pub(crate) struct CatalogLock {
    file: File,
}

impl CatalogLock {
    #[allow(unsafe_code)]
    pub(crate) fn acquire_shared(root: &Path) -> Result<Self, CatalogStorageError> {
        let path = root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|source| storage_io_error("open read-only catalog lock", &path, source))?;
        let metadata = file
            .metadata()
            .map_err(|source| storage_io_error("inspect read-only catalog lock", &path, source))?;
        if !metadata.file_type().is_file() {
            return Err(CatalogStorageError::InvalidEnvelope);
        }
        // SAFETY: `file` owns a live descriptor for the complete flock call.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
        if result != 0 {
            let source = std::io::Error::last_os_error();
            if source.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Err(CatalogStorageError::LockHeld(path));
            }
            return Err(storage_io_error("lock read-only catalog", &path, source));
        }
        Ok(Self { file })
    }

    #[allow(unsafe_code)]
    pub(crate) fn acquire(root: &Path, mode: u32) -> Result<Self, CatalogStorageError> {
        let path = root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(mode)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|source| storage_io_error("open catalog lock", &path, source))?;
        // SAFETY: `file` owns a live descriptor for the complete flock call.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let source = std::io::Error::last_os_error();
            if source.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Err(CatalogStorageError::LockHeld(path));
            }
            return Err(storage_io_error("lock catalog", &path, source));
        }
        Ok(Self { file })
    }
}

impl Drop for CatalogLock {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `self.file` remains open for the complete flock call.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}
