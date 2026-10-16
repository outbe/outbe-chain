//! One configuration document shared by a node and its snapshot exporter.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::StorageError;

/// Filesystem locations for one primary and its independent read sessions.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RocksDbConfig {
    pub path: PathBuf,
    pub secondary_path: PathBuf,
}

/// Backend selection. Runtime consumers receive capabilities, not this enum.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageBackend {
    RocksDb(RocksDbConfig),
}

/// Validated storage settings with paths resolved relative to the configuration file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageConfig {
    pub start_block: u64,
    pub backend: StorageBackend,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    backend: BackendName,
    #[serde(default = "first_block")]
    start_block: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    rocksdb: Option<RocksDbConfig>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum BackendName {
    Rocksdb,
}

const fn first_block() -> u64 {
    1
}

impl StorageConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path).map_err(|error| {
            StorageError::invalid_argument(format!(
                "cannot read storage configuration {}: {error}",
                path.display()
            ))
        })?;
        Self::from_toml(&source, path)
    }

    /// Parse using the file's location, never the working directory of its consumer.
    pub fn from_toml(source: &str, path: &Path) -> Result<Self, StorageError> {
        let document: Document = toml::from_str(source).map_err(|error: toml::de::Error| {
            StorageError::invalid_argument(format!(
                "invalid storage TOML at byte range {:?}",
                error.span()
            ))
        })?;
        if document.version != 1 {
            return Err(StorageError::invalid_argument(
                "unsupported storage configuration version",
            ));
        }
        let config_path = absolute_path(path)?;
        let base = config_path.parent().ok_or_else(|| {
            StorageError::invalid_argument("storage configuration must have a parent directory")
        })?;
        let backend = match (document.backend, document.rocksdb) {
            (BackendName::Rocksdb, Some(mut config)) => {
                if config.path.as_os_str().is_empty()
                    || config.secondary_path.as_os_str().is_empty()
                {
                    return Err(StorageError::invalid_argument(
                        "RocksDB paths must not be empty",
                    ));
                }
                config.path = normalize_path(&base.join(config.path));
                config.secondary_path = normalize_path(&base.join(config.secondary_path));
                StorageBackend::RocksDb(config)
            }
            _ => {
                return Err(StorageError::invalid_argument(
                    "storage configuration requires exactly the selected backend section",
                ))
            }
        };
        let config = Self {
            start_block: document.start_block,
            backend,
        };
        config.validate()?;
        Ok(config)
    }

    /// Serialize settings for launch generators and isolated test fixtures.
    pub fn to_toml(&self) -> Result<String, StorageError> {
        self.validate()?;
        let StorageBackend::RocksDb(config) = &self.backend;
        toml::to_string_pretty(&Document {
            version: 1,
            backend: BackendName::Rocksdb,
            start_block: self.start_block,
            rocksdb: Some(config.clone()),
        })
        .map_err(|_| StorageError::invalid_argument("cannot encode storage configuration"))
    }

    #[must_use]
    pub const fn backend_name(&self) -> &'static str {
        match self.backend {
            StorageBackend::RocksDb(_) => "rocksdb",
        }
    }

    pub(crate) fn validate(&self) -> Result<(), StorageError> {
        match &self.backend {
            StorageBackend::RocksDb(config) => {
                if !config.path.is_absolute() || !config.secondary_path.is_absolute() {
                    return Err(StorageError::invalid_argument(
                        "resolved RocksDB paths must be absolute",
                    ));
                }
                let primary = normalize_path(&config.path);
                let secondary = normalize_path(&config.secondary_path);
                if primary.starts_with(&secondary) || secondary.starts_with(&primary) {
                    return Err(StorageError::invalid_argument(
                        "RocksDB primary and secondary directories must not overlap",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, StorageError> {
    if path.is_absolute() {
        Ok(normalize_path(path))
    } else {
        Ok(normalize_path(
            &std::env::current_dir()
                .map_err(StorageError::backend)?
                .join(path),
        ))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            part => result.push(part.as_os_str()),
        }
    }
    result
}
