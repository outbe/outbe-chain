use crate::errors::BlsKeyError;
use eyre::{Result, WrapErr};
use std::path::{Path, PathBuf};

use super::{atomic_write_secret, KEYCHAIN_MARKER};

/// Parse an OS-keychain marker file.
///
/// Bytes that do not start with `KEYCHAIN_MARKER` are not a marker. A file
/// that starts with the marker but is not valid UTF-8, has fewer than three
/// lines, or has an empty service or account is rejected.
pub(super) fn parse_keychain_marker(raw: &[u8]) -> Result<Option<(String, String)>> {
    if !raw.starts_with(KEYCHAIN_MARKER.as_bytes()) {
        return Ok(None);
    }
    let Ok(text) = std::str::from_utf8(raw) else {
        return Err(BlsKeyError::InvalidKeychainMarker.into());
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 3 || lines[1].is_empty() || lines[2].is_empty() {
        return Err(BlsKeyError::InvalidKeychainMarker.into());
    }
    Ok(Some((lines[1].to_owned(), lines[2].to_owned())))
}

/// Lexically normalize `path` to an absolute path without touching the filesystem.
///
/// `std::fs::canonicalize` is the wrong tool here: the marker may not exist yet,
/// and symlink resolution would change the account identity.
fn lexical_absolute_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .wrap_err("failed to resolve the working directory for a keychain path")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    Ok(normalized)
}

fn path_identity_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

fn keychain_path_tag(path: &Path) -> Result<String> {
    let normalized = lexical_absolute_path(path)?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &path_identity_bytes(&normalized));
    let encoded = hex::encode(digest);
    Ok(encoded[..16].to_owned())
}

#[cfg(test)]
struct TestKeychain {
    entries: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<(String, String), String>>>,
    fail_next_delete: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
thread_local! {
    static TEST_KEYCHAIN: std::cell::RefCell<Option<std::sync::Arc<TestKeychain>>> =
        const { std::cell::RefCell::new(None) };
}

/// Installs the in-memory keychain for the current test thread.
///
/// Drop restores the override to `None`, so a later test on this thread does
/// not see these entries and does not touch the OS keychain while the guard lives.
#[cfg(test)]
#[must_use]
pub(crate) struct TestKeychainGuard {
    state: std::sync::Arc<TestKeychain>,
}

#[cfg(test)]
pub(crate) fn install_test_keychain() -> TestKeychainGuard {
    let state = std::sync::Arc::new(TestKeychain {
        entries: std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
        fail_next_delete: std::sync::atomic::AtomicBool::new(false),
    });
    TEST_KEYCHAIN.with(|slot| *slot.borrow_mut() = Some(std::sync::Arc::clone(&state)));
    TestKeychainGuard { state }
}

#[cfg(test)]
impl Drop for TestKeychainGuard {
    fn drop(&mut self) {
        TEST_KEYCHAIN.with(|slot| *slot.borrow_mut() = None);
    }
}

#[cfg(test)]
impl TestKeychainGuard {
    /// The next keychain delete returns an error, then the flag clears.
    pub(crate) fn fail_next_delete(&self) {
        self.state
            .fail_next_delete
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn len(&self) -> usize {
        self.lock_entries().len()
    }

    pub(crate) fn contains_account(&self, service: &str, account: &str) -> bool {
        self.lock_entries()
            .contains_key(&(service.to_owned(), account.to_owned()))
    }

    fn lock_entries(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::BTreeMap<(String, String), String>> {
        self.state
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
fn active_test_keychain() -> Option<std::sync::Arc<TestKeychain>> {
    TEST_KEYCHAIN.with(|slot| slot.borrow().clone())
}

pub(super) fn keychain_get_password(service: &str, account: &str) -> Result<String> {
    #[cfg(test)]
    if let Some(chain) = active_test_keychain() {
        return chain
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(service.to_owned(), account.to_owned()))
            .cloned()
            .ok_or_else(|| BlsKeyError::KeychainRead("NoEntry".to_owned()).into());
    }
    let entry = keyring::Entry::new(service, account)
        .map_err(|e| BlsKeyError::KeychainEntry(format!("{e}")))?;
    entry
        .get_password()
        .map_err(|e| BlsKeyError::KeychainRead(format!("{e}")).into())
}

pub(super) fn keychain_set_password(service: &str, account: &str, password: &str) -> Result<()> {
    #[cfg(test)]
    if let Some(chain) = active_test_keychain() {
        chain
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                (service.to_owned(), account.to_owned()),
                password.to_owned(),
            );
        return Ok(());
    }
    let entry = keyring::Entry::new(service, account)
        .map_err(|e| BlsKeyError::KeychainEntry(format!("{e}")))?;
    entry
        .set_password(password)
        .map_err(|e| BlsKeyError::KeychainStore(format!("{e}")))?;
    Ok(())
}

/// Deletes one credential.
///
/// `Ok(true)` means the entry was removed. `Ok(false)` means the store reported
/// [`keyring::Error::NoEntry`]. Any other keychain failure is returned.
pub(super) fn keychain_delete_credential(service: &str, account: &str) -> Result<bool> {
    #[cfg(test)]
    if let Some(chain) = active_test_keychain() {
        if chain
            .fail_next_delete
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            return Err(BlsKeyError::KeychainCleanup("injected delete failure".to_owned()).into());
        }
        let removed = chain
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(service.to_owned(), account.to_owned()))
            .is_some();
        return Ok(removed);
    }
    let entry = keyring::Entry::new(service, account)
        .map_err(|e| BlsKeyError::KeychainEntry(format!("{e}")))?;
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(error) => Err(BlsKeyError::KeychainCleanup(format!("{error}")).into()),
    }
}

/// Save `data` to the OS keychain and write a marker file at `path`.
///
/// The account is `{stem}-{path_tag}-{sha256(data)}`. `path_tag` is the first
/// 16 hex characters of SHA-256 over the lexically normalized absolute path.
/// A resave deletes the previous account only when that account uses the same
/// service and the same path prefix. A legacy `{stem}-{sha256}` account does
/// not match that prefix and is left in place.
///
/// Callers must be the single writer of `path`. Two concurrent saves of one
/// path can delete a credential that the surviving marker still names. This
/// function does not take a lock, and a path tag is not exclusive ownership of
/// a content-addressed account shared by another path.
pub(super) fn save_to_keychain(path: &Path, data: &[u8]) -> Result<()> {
    let service = "outbe-chain";
    let account_prefix = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("unknown");
    let path_tag = keychain_path_tag(path)?;
    let owner_prefix = format!("{account_prefix}-{path_tag}-");
    // A content-addressed account keeps the old marker + old secret valid until
    // the new marker is atomically installed. Reusing one account would expose
    // a cross-backend crash window where the marker still names a secret whose
    // contents have already changed.
    let account = format!(
        "{owner_prefix}{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, data))
    );

    let previous = read_previous_keychain_marker(path)?;

    keychain_set_password(service, &account, &hex::encode(data))?;
    let marker = format!("{KEYCHAIN_MARKER}{service}\n{account}\n");
    atomic_write_secret(path, marker.as_bytes(), "keychain marker")?;

    if let Some((prev_service, prev_account)) = previous {
        if prev_service == service
            && prev_account.starts_with(&owner_prefix)
            && prev_account != account
        {
            if let Err(err) = keychain_delete_credential(&prev_service, &prev_account) {
                tracing::warn!(
                    service = %prev_service,
                    account = %prev_account,
                    %err,
                    "failed to prune superseded keychain credential"
                );
            }
        }
    }
    Ok(())
}

fn read_previous_keychain_marker(path: &Path) -> Result<Option<(String, String)>> {
    match std::fs::read(path) {
        Ok(raw) => match parse_keychain_marker(&raw) {
            Ok(marker) => Ok(marker),
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    %err,
                    "ignoring malformed prior keychain marker during save"
                );
                Ok(None)
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .wrap_err_with(|| format!("failed to read prior keychain marker: {}", path.display())),
    }
}
