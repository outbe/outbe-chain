use std::{error::Error as StdError, fmt};

use thiserror::Error;

/// Provisional maximum UTF-8 byte length of a namespace.
pub const MAX_NAMESPACE_BYTES: usize = 63;
/// Provisional maximum byte length of a key.
pub const MAX_KEY_BYTES: usize = 1_024;
/// Provisional maximum byte length of a value.
pub const MAX_VALUE_BYTES: usize = 8 * 1024 * 1024;
/// Provisional maximum requested entries in one scan page.
pub const MAX_SCAN_ENTRIES: usize = 1_024;
/// Provisional maximum sum of value bytes in one scan page.
pub const MAX_SCAN_PAGE_VALUE_BYTES: usize = 8 * 1024 * 1024;

/// A validated collection/keyspace identifier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Namespace(Box<str>);

impl Namespace {
    /// Validates a code-defined namespace.
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, StorageError> {
        let value = value.into();
        validate_namespace(&value)?;
        Ok(Self(value))
    }

    /// Returns the exact backend namespace name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<&str> for Namespace {
    type Error = StorageError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A validated, non-empty opaque storage key.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Key(Vec<u8>);

impl Key {
    /// Validates an opaque key.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, StorageError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(StorageError::invalid_argument("key must not be empty"));
        }
        if bytes.len() > MAX_KEY_BYTES {
            return Err(StorageError::invalid_argument(format!(
                "key exceeds {MAX_KEY_BYTES} bytes"
            )));
        }
        Ok(Self(bytes))
    }

    /// Returns the opaque key bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl TryFrom<Vec<u8>> for Key {
    type Error = StorageError;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<const N: usize> TryFrom<[u8; N]> for Key {
    type Error = StorageError;

    fn try_from(value: [u8; N]) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// A validated opaque storage value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Value(Vec<u8>);

impl Value {
    /// Validates an opaque value. Empty values are valid.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, StorageError> {
        let bytes = bytes.into();
        if bytes.len() > MAX_VALUE_BYTES {
            return Err(StorageError::invalid_argument(format!(
                "value exceeds {MAX_VALUE_BYTES} bytes"
            )));
        }
        Ok(Self(bytes))
    }

    /// Returns the opaque value bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl TryFrom<Vec<u8>> for Value {
    type Error = StorageError;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<const N: usize> TryFrom<[u8; N]> for Value {
    type Error = StorageError;

    fn try_from(value: [u8; N]) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// One ordered prefix-scan request.
#[derive(Clone, Copy, Debug)]
pub struct ScanRequest<'a> {
    prefix: &'a [u8],
    after: Option<&'a Key>,
    limit: usize,
}

impl<'a> ScanRequest<'a> {
    /// Validates a bounded scan request.
    pub fn new(
        prefix: &'a [u8],
        after: Option<&'a Key>,
        limit: usize,
    ) -> Result<Self, StorageError> {
        let request = Self {
            prefix,
            after,
            limit,
        };
        request.validate()?;
        Ok(request)
    }

    pub(crate) fn validate(self) -> Result<(), StorageError> {
        if self.prefix.len() > MAX_KEY_BYTES {
            return Err(StorageError::invalid_argument(format!(
                "scan prefix exceeds {MAX_KEY_BYTES} bytes"
            )));
        }
        if self.limit == 0 || self.limit > MAX_SCAN_ENTRIES {
            return Err(StorageError::invalid_argument(format!(
                "scan limit must be between 1 and {MAX_SCAN_ENTRIES}"
            )));
        }
        if let Some(after) = self.after {
            if !after.as_bytes().starts_with(self.prefix) {
                return Err(StorageError::invalid_argument(
                    "scan cursor does not match the requested prefix",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn prefix(self) -> &'a [u8] {
        self.prefix
    }

    pub(crate) fn after(self) -> Option<&'a Key> {
        self.after
    }

    pub(crate) fn limit(self) -> usize {
        self.limit
    }
}

/// One key/value record returned by a scan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanEntry {
    /// The record key.
    pub key: Key,
    /// The record value.
    pub value: Value,
}

/// A complete bounded scan page.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanPage {
    /// Ordered records in this page.
    pub entries: Vec<ScanEntry>,
    /// Exclusive cursor to pass to the next request, if more records exist.
    pub next_after: Option<Key>,
}

/// Stable backend-neutral error classes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageErrorKind {
    /// A namespace, key, prefix, cursor, or limit is invalid.
    InvalidArgument,
    /// The backend cannot currently serve the operation.
    Unavailable,
    /// Stored data violates an adapter representation or invariant.
    Corruption,
    /// Another backend failure.
    Backend,
}

/// Backend-neutral storage failure.
#[derive(Debug, Error)]
pub enum StorageError {
    /// A namespace, key, prefix, cursor, or limit is invalid.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// The backend cannot currently serve the operation.
    #[error("storage backend unavailable")]
    Unavailable {
        /// Backend diagnostic retained without exposing backend result types.
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
    /// Stored data violates an adapter representation or invariant.
    #[error("storage corruption: {0}")]
    Corruption(String),
    /// Another backend failure.
    #[error("storage backend failure")]
    Backend {
        /// Backend diagnostic retained without exposing backend result types.
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
}

impl StorageError {
    /// Returns the stable class of this error.
    #[must_use]
    pub const fn kind(&self) -> StorageErrorKind {
        match self {
            Self::InvalidArgument(_) => StorageErrorKind::InvalidArgument,
            Self::Unavailable { .. } => StorageErrorKind::Unavailable,
            Self::Corruption(_) => StorageErrorKind::Corruption,
            Self::Backend { .. } => StorageErrorKind::Backend,
        }
    }

    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self::InvalidArgument(message.into())
    }

    pub(crate) fn unavailable(source: impl StdError + Send + Sync + 'static) -> Self {
        Self::Unavailable {
            source: Box::new(source),
        }
    }

    pub(crate) fn backend(source: impl StdError + Send + Sync + 'static) -> Self {
        Self::Backend {
            source: Box::new(source),
        }
    }
}

fn validate_namespace(value: &str) -> Result<(), StorageError> {
    if value.is_empty() {
        return Err(StorageError::invalid_argument(
            "namespace must not be empty",
        ));
    }
    if value.len() > MAX_NAMESPACE_BYTES {
        return Err(StorageError::invalid_argument(format!(
            "namespace exceeds {MAX_NAMESPACE_BYTES} bytes"
        )));
    }
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return Err(StorageError::invalid_argument(
            "namespace must not be empty",
        ));
    };
    if !first.is_ascii_lowercase() {
        return Err(StorageError::invalid_argument(
            "namespace must start with an ASCII lowercase letter",
        ));
    }
    if !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_') {
        return Err(StorageError::invalid_argument(
            "namespace may contain only ASCII lowercase letters, digits, and underscores",
        ));
    }
    if value.starts_with("system.") {
        return Err(StorageError::invalid_argument("reserved MongoDB namespace"));
    }
    Ok(())
}
