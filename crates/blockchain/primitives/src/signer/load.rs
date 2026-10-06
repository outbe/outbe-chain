//! File loading policies for local EVM signing keys.

use std::path::Path;

use super::{OutbeEvmSigner, SignerError};

pub fn from_file(path: impl AsRef<Path>) -> Result<OutbeEvmSigner, SignerError> {
    let path = path.as_ref();
    ensure_safe_key_file_permissions(path)?;
    let secret = std::fs::read_to_string(path).map_err(|source| SignerError::ReadKey {
        path: path.to_path_buf(),
        source,
    })?;
    OutbeEvmSigner::from_hex(&secret)
}

/// Loads a role-custody key from one canonical owner-bound file.
///
/// Unlike [`from_file`], this rejects symlinks, non-regular files,
/// unexpected owners, modes other than `0600`, hard links, non-canonical
/// lowercase 32-byte hex payloads, and path replacement during open.
/// This function ignores surrounding ASCII whitespace. That whitespace is not
/// part of the key.
#[cfg(unix)]
pub fn from_strict_file(
    path: impl AsRef<Path>,
    expected_owner_uid: u32,
) -> Result<OutbeEvmSigner, SignerError> {
    let path = path.as_ref();
    let mut file = StrictKeyFileLoader {
        path,
        expected_owner_uid,
    }
    .open()?;
    let encoded = read_canonical_key(path, &mut file)?;
    OutbeEvmSigner::from_hex(encoded.trim_ascii())
}

#[cfg(not(unix))]
pub fn from_strict_file(
    path: impl AsRef<Path>,
    _expected_owner_uid: u32,
) -> Result<OutbeEvmSigner, SignerError> {
    from_file(path)
}

#[cfg(unix)]
struct StrictKeyFileLoader<'a> {
    path: &'a Path,
    expected_owner_uid: u32,
}

#[cfg(unix)]
impl StrictKeyFileLoader<'_> {
    fn open(&self) -> Result<std::fs::File, SignerError> {
        use std::os::unix::fs::MetadataExt as _;

        let path = self.path;
        let path_metadata =
            std::fs::symlink_metadata(path).map_err(|source| SignerError::InspectPermissions {
                path: path.to_path_buf(),
                source,
            })?;
        self.validate_metadata(&path_metadata)?;
        let file = std::fs::File::open(path).map_err(|source| SignerError::ReadKey {
            path: path.to_path_buf(),
            source,
        })?;
        let opened_metadata =
            file.metadata()
                .map_err(|source| SignerError::InspectPermissions {
                    path: path.to_path_buf(),
                    source,
                })?;
        self.validate_metadata(&opened_metadata)?;
        if path_metadata.dev() != opened_metadata.dev()
            || path_metadata.ino() != opened_metadata.ino()
        {
            return Err(SignerError::UnsafeKeyFile {
                path: path.to_path_buf(),
                reason: "key path changed while opening",
            });
        }
        Ok(file)
    }

    fn validate_metadata(&self, metadata: &std::fs::Metadata) -> Result<(), SignerError> {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        const MIN_ENCODED_KEY_BYTES: u64 = 64;
        const MAX_ENCODED_KEY_BYTES: u64 = 128;
        let unsafe_reason = if !metadata.file_type().is_file() {
            Some("key is not a regular file")
        } else if metadata.uid() != self.expected_owner_uid {
            Some("key has the wrong owner")
        } else if metadata.permissions().mode() & 0o777 != 0o600 {
            Some("key mode is not 0600")
        } else if metadata.nlink() != 1 {
            Some("key has an unexpected hard-link count")
        } else {
            None
        };
        if let Some(reason) = unsafe_reason {
            return Err(SignerError::UnsafeKeyFile {
                path: self.path.to_path_buf(),
                reason,
            });
        }
        if !(MIN_ENCODED_KEY_BYTES..=MAX_ENCODED_KEY_BYTES).contains(&metadata.len()) {
            return Err(SignerError::NonCanonicalKeyFile {
                path: self.path.to_path_buf(),
            });
        }
        Ok(())
    }
}

#[cfg(unix)]
fn read_canonical_key(
    path: &Path,
    file: &mut std::fs::File,
) -> Result<zeroize::Zeroizing<String>, SignerError> {
    use std::io::Read as _;

    let mut encoded = zeroize::Zeroizing::new(String::new());
    file.read_to_string(&mut encoded)
        .map_err(|source| SignerError::ReadKey {
            path: path.to_path_buf(),
            source,
        })?;
    let hex = encoded.trim_ascii();
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SignerError::NonCanonicalKeyFile {
            path: path.to_path_buf(),
        });
    }
    Ok(encoded)
}

#[cfg(unix)]
fn ensure_safe_key_file_permissions(path: &Path) -> Result<(), SignerError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(path).map_err(|source| SignerError::InspectPermissions {
        path: path.to_path_buf(),
        source,
    })?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(SignerError::UnsafeFilePermissions {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_safe_key_file_permissions(_path: &Path) -> Result<(), SignerError> {
    Ok(())
}
