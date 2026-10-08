//! Projection datasource fixture. Runtime processes consume only the generated TOML.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use eyre::{bail, eyre, Result, WrapErr};
use outbe_compressed_entities::{decode_stored_tribute_v1, decode_stored_tribute_v2, WwdEntityId};
use outbe_offchain_storage::StorageProvider;
mod mongo;
use outbe_offchain_storage::{
    Key, Namespace, RocksDbConfig, ScanEntry, ScanRequest, StorageBackend, StorageConfig,
    StorageReader, StorageReaderHandle,
};

#[cfg(test)]
use crate::env::Environment;
use crate::internal::config::Config;
use crate::ocomp_evidence::sha256_hex;

const COLLECTIONS: [&str; 3] = ["tributes", "tributes_by_owner", "tributes_by_day"];

#[derive(Debug)]
pub struct ProjectionFixture {
    cfg: Config,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedTribute {
    pub raw_id: WwdEntityId,
    pub stored_body: Vec<u8>,
}

/// Exact logical keys, values and metadata, independent of the physical backend codec.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TributeProjectionSnapshot {
    pub records: [ScanEntry; 3],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TributeProjectionDigests {
    pub primary_sha256: String,
    pub owner_index_sha256: String,
    pub worldwide_day_index_sha256: String,
}

impl TributeProjectionSnapshot {
    pub fn evidence_digests(&self) -> Result<TributeProjectionDigests> {
        let [primary, owner, day] = &self.records;
        Ok(TributeProjectionDigests {
            primary_sha256: record_sha256(COLLECTIONS[0], primary)?,
            owner_index_sha256: record_sha256(COLLECTIONS[1], owner)?,
            worldwide_day_index_sha256: record_sha256(COLLECTIONS[2], day)?,
        })
    }
}

fn record_sha256(namespace: &str, record: &ScanEntry) -> Result<String> {
    // An explicit versioned tuple preserves absent vs present metadata and canonical key order.
    let metadata = record
        .metadata
        .as_ref()
        .map(|metadata| metadata.iter().collect::<Vec<_>>());
    Ok(sha256_hex(&serde_json::to_vec(&(
        "outbe-projection-record-v1",
        namespace,
        hex::encode(record.key.as_bytes()),
        hex::encode(record.value.as_bytes()),
        metadata,
    ))?))
}

fn session(cfg: &Config, index: usize) -> Result<StorageReaderHandle> {
    let config = storage_config(cfg, index)?;
    Ok(StorageProvider::new(config)?
        .with_partition_routing(outbe_offchain_data::entity_partition_routing()?)
        .read_source("e2e-observer")?
        .open_session()?)
}

pub(crate) fn projection_fixture(cfg: &Config) -> ProjectionFixture {
    ProjectionFixture { cfg: cfg.clone() }
}

impl ProjectionFixture {
    /// The caller must stop all nodes before deliberate re-bootstrap.
    pub fn reset_projection_state(&self) -> Result<()> {
        let mut targets = Vec::new();
        let mut databases = Vec::new();
        for index in 0..self.cfg.validators {
            let path = self.cfg.projection_storage_config(index);
            if !path.exists() {
                continue;
            }
            match storage_config(&self.cfg, index)?.backend {
                StorageBackend::MongoDb(config) => {
                    databases.push(config);
                }
                StorageBackend::RocksDb(config) => {
                    targets.extend(reset_targets(
                        &self.cfg,
                        [config.path, config.secondary_path],
                    )?);
                }
            }
        }
        for config in databases {
            mongo::reset(&self.cfg, &config)?;
        }
        for path in targets {
            fs::remove_dir_all(path)?;
        }
        Ok(())
    }

    fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(Config) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let cfg = self.cfg.clone();
        std::thread::spawn(move || operation(cfg))
            .join()
            .map_err(|_| eyre!("projection fixture worker panicked"))?
    }

    pub fn wait_for_tribute_projection(&self, tx_hash: &str, tries: u32) -> Result<()> {
        self.wait_for_tribute_projection_on_nodes(tx_hash, tries, self.cfg.validators)
    }

    pub fn wait_for_tribute_projection_on_nodes(
        &self,
        tx_hash: &str,
        tries: u32,
        validators: usize,
    ) -> Result<()> {
        let tx_hash = tx_hash.to_owned();
        self.run(move |cfg| {
            let started = Instant::now();
            let mut last = eyre!("projection did not appear");
            for _ in 0..tries {
                let result = (|| -> Result<()> {
                    let canonical = snapshot_across(&tribute_readers(&cfg, 0)?, &tx_hash)?;
                    for index in 1..validators {
                        let observed = snapshot_across(&tribute_readers(&cfg, index)?, &tx_hash)?;
                        if canonical != observed {
                            bail!("validator-{index}: projection differs from validator-0");
                        }
                    }
                    Ok(())
                })();
                match result {
                    Ok(()) => {
                        eprintln!("E2E_TRIBUTE_TIMELINE stage=projection-visible wait_elapsed_ms={} tx={tx_hash} nodes={validators}", started.elapsed().as_millis());
                        return Ok(());
                    }
                    Err(error) => last = error,
                }
                sleep(Duration::from_millis(500));
            }
            Err(last)
        })
    }

    pub fn projected_tribute(&self, validator: usize, tx_hash: &str) -> Result<ProjectedTribute> {
        let tx_hash = tx_hash.to_owned();
        self.run(move |cfg| projected_from_readers(&tribute_readers(&cfg, validator)?, &tx_hash))
    }

    pub fn tribute_projection_snapshot(
        &self,
        validator: usize,
        tx_hash: &str,
    ) -> Result<TributeProjectionSnapshot> {
        let tx_hash = tx_hash.to_owned();
        self.run(move |cfg| snapshot_across(&tribute_readers(&cfg, validator)?, &tx_hash))
    }

    pub fn assert_no_tribute_projection(&self) -> Result<()> {
        self.run(|cfg| {
            for index in 0..cfg.validators {
                for reader in tribute_readers(&cfg, index)? {
                    for name in COLLECTIONS {
                        let page = reader
                            .scan_prefix(Namespace::new(name)?, ScanRequest::new(&[], None, 1)?)?;
                        if !page.entries.is_empty() {
                            bail!("validator-{index}.{name}: expected no records");
                        }
                    }
                }
            }
            Ok(())
        })
    }
}

/// Read one Tribute from all entity partitions under an off-chain root.
pub fn observe_offchain_tribute(offchain_root: &Path, tx_hash: &str) -> Result<ProjectedTribute> {
    let scratch = tempfile::tempdir()?;
    let readers = open_tribute_readers(offchain_root, scratch.path())?;
    projected_from_readers(&readers, tx_hash)
}

fn reset_targets(cfg: &Config, paths: [PathBuf; 2]) -> Result<Vec<PathBuf>> {
    let mut targets = Vec::new();
    for path in paths {
        if !path.exists() {
            continue;
        }
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(cfg.dir.canonicalize()?) {
            bail!("refusing to reset storage outside this scenario");
        }
        targets.push(path);
    }
    Ok(targets)
}

impl Drop for ProjectionFixture {
    fn drop(&mut self) {
        mongo::stop(&self.cfg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_offchain_storage::{
        AtomicWriteBatch, AtomicWriteOperation, RocksDbStorage, StorageMetadata, StorageWriter,
        Value,
    };
    use std::collections::BTreeMap;

    #[test]
    fn transaction_lookup_pages_and_rejects_duplicate_matches() {
        let root = tempfile::tempdir().unwrap();
        let storage = RocksDbStorage::open(root.path().join("primary")).unwrap();
        let ns = Namespace::new("tributes").unwrap();
        let mut batch = AtomicWriteBatch::new();
        for index in 0_u32..300 {
            batch.push(AtomicWriteOperation::Put {
                namespace: ns.clone(),
                key: Key::new(index.to_be_bytes()).unwrap(),
                record: outbe_offchain_storage::StoredValue::with_metadata(
                    Value::new([7]).unwrap(),
                    StorageMetadata::new(BTreeMap::from([(
                        "tx_hash".into(),
                        format!("tx-{index}"),
                    )]))
                    .unwrap(),
                ),
            });
        }
        storage.apply_atomic(&batch).unwrap();
        let found = primary(&storage, "tx-299").unwrap();
        assert_eq!(found.key.as_bytes(), 299_u32.to_be_bytes());
        assert!(primary(&storage, "absent").is_err());
        storage
            .apply_atomic(&AtomicWriteBatch::from_operations(vec![
                AtomicWriteOperation::Put {
                    namespace: ns,
                    key: Key::new(301_u32.to_be_bytes()).unwrap(),
                    record: outbe_offchain_storage::StoredValue {
                        value: found.value,
                        metadata: found.metadata,
                    },
                },
            ]))
            .unwrap();
        assert!(primary(&storage, "tx-299")
            .unwrap_err()
            .to_string()
            .contains("multiple"));
    }

    #[test]
    fn record_evidence_commits_namespace_key_body_and_metadata() {
        let baseline = ScanEntry {
            key: Key::new([1]).unwrap(),
            value: Value::new([2]).unwrap(),
            metadata: None,
        };
        let hash = record_sha256("tributes", &baseline).unwrap();
        assert_ne!(hash, record_sha256("tributes_by_owner", &baseline).unwrap());
        let mut changed = baseline.clone();
        changed.key = Key::new([3]).unwrap();
        assert_ne!(hash, record_sha256("tributes", &changed).unwrap());
        changed = baseline.clone();
        changed.value = Value::new([3]).unwrap();
        assert_ne!(hash, record_sha256("tributes", &changed).unwrap());
        changed = baseline;
        changed.metadata = Some(StorageMetadata::default());
        assert_ne!(hash, record_sha256("tributes", &changed).unwrap());
    }

    #[test]
    fn node_role_transition_keeps_one_storage_config_argument() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = Config::resolve(&Environment::default());
        cfg.dir = root.path().canonicalize().unwrap();
        let mut command = std::process::Command::new("node");
        configure_node_command(&cfg, 4, &mut command).unwrap();
        configure_node_command(&cfg, 4, &mut command).unwrap();
        assert_eq!(command.get_args().count(), 2);
        assert!(configure_node_command(&cfg, 5, &mut command).is_err());
    }

    #[test]
    fn persisted_mongo_config_is_rejected_without_overwrite_or_connection() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = Config::resolve(&Environment::default());
        cfg.dir = root.path().canonicalize().unwrap();
        ensure_node_config(&cfg, 0).unwrap();
        let path = cfg.projection_storage_config(0);
        let incompatible = "version = 1\nbackend = \"mongodb\"\n[mongodb]\nuri = \"mongodb://127.0.0.1:1\"\ndatabase = \"forbidden\"\n";
        fs::write(&path, incompatible).unwrap();
        assert!(ensure_node_config(&cfg, 0)
            .unwrap_err()
            .to_string()
            .contains("requires RocksDB"));
        assert!(session(&cfg, 0).is_err());
        assert!(projection_fixture(&cfg).reset_projection_state().is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), incompatible);
    }

    #[test]
    fn persisted_storage_identity_and_start_block_cannot_change() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = Config::resolve(&Environment::default());
        cfg.dir = root.path().canonicalize().unwrap();
        ensure_node_config(&cfg, 0).unwrap();
        let path = cfg.projection_storage_config(0);
        let original = StorageConfig::load(&path).unwrap();
        for field in 0..3 {
            let mut config = original.clone();
            let StorageBackend::RocksDb(ref mut rocks) = config.backend else {
                unreachable!()
            };
            match field {
                0 => config.start_block = 2,
                1 => rocks.path = cfg.validator_dir(1).join("data/offchain"),
                _ => rocks.secondary_path = cfg.validator_dir(1).join("ocomp/rocksdb-secondary"),
            }
            let document = config.to_toml().unwrap();
            fs::write(&path, &document).unwrap();
            assert!(ensure_node_config(&cfg, 0).is_err());
            assert!(session(&cfg, 0).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), document);
        }
        fs::write(&path, "not valid storage TOML").unwrap();
        assert!(ensure_node_config(&cfg, 0).is_err());
    }

    #[test]
    fn storage_symlink_cannot_alias_another_database() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let mut cfg = Config::resolve(&Environment::default());
        cfg.dir = root.path().canonicalize().unwrap();
        ensure_node_config(&cfg, 0).unwrap();
        let data = cfg.validator_dir(0).join("data");
        fs::create_dir_all(&data).unwrap();
        std::os::unix::fs::symlink(external.path(), data.join("offchain")).unwrap();
        fs::write(external.path().join("sentinel"), "preserved").unwrap();
        assert!(ensure_node_config(&cfg, 0).is_err());
        assert!(session(&cfg, 0).is_err());
        assert!(projection_fixture(&cfg).reset_projection_state().is_err());
        assert_eq!(
            fs::read_to_string(external.path().join("sentinel")).unwrap(),
            "preserved"
        );
    }

    #[test]
    fn first_launch_rejects_storage_symlink_before_publishing_config() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let mut cfg = Config::resolve(&Environment::default());
        cfg.dir = root.path().canonicalize().unwrap();
        let data = cfg.validator_dir(0).join("data");
        fs::create_dir_all(&data).unwrap();
        std::os::unix::fs::symlink(external.path(), data.join("offchain")).unwrap();
        let sentinel = external.path().join("sentinel");
        fs::write(&sentinel, "preserved").unwrap();
        let mut command = std::process::Command::new("node");
        assert!(configure_node_command(&cfg, 0, &mut command).is_err());
        assert_eq!(command.get_args().count(), 0);
        assert!(!cfg.projection_storage_config(0).exists());
        assert_eq!(fs::read_to_string(sentinel).unwrap(), "preserved");
        assert_eq!(fs::read_dir(external.path()).unwrap().count(), 1);
    }

    #[test]
    fn reset_validates_every_owned_path_before_removing_any_storage() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = Config::resolve(&Environment::default());
        cfg.dir = root.path().canonicalize().unwrap();
        for index in 0..2 {
            ensure_node_config(&cfg, index).unwrap();
            fs::create_dir_all(cfg.validator_dir(index).join("data/offchain")).unwrap();
        }
        let sentinel = cfg.validator_dir(0).join("data/offchain/sentinel");
        fs::write(&sentinel, "preserved").unwrap();
        let path = cfg.projection_storage_config(1);
        let mut config = StorageConfig::load(&path).unwrap();
        config.start_block = 2;
        fs::write(path, config.to_toml().unwrap()).unwrap();
        assert!(projection_fixture(&cfg).reset_projection_state().is_err());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "preserved");
    }

    #[test]
    fn node_config_is_shared_with_readers_and_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = Config::resolve(&Environment::default());
        cfg.dir = root.path().canonicalize().unwrap();
        ensure_node_config(&cfg, 4).unwrap();
        let path = cfg.projection_storage_config(4);
        let original = fs::read(&path).unwrap();
        ensure_node_config(&cfg, 4).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        let config = StorageConfig::load(&path).unwrap();
        let writer = StorageProvider::new(config)
            .unwrap()
            .with_partition_routing(outbe_offchain_data::entity_partition_routing().unwrap())
            .open_writer()
            .unwrap();
        writer.ownership.activate().unwrap();
        let completion = writer.ownership.completion();
        let ns = Namespace::new("fixture").unwrap();
        let key = Key::new([1]).unwrap();
        writer
            .writer
            .put(ns.clone(), &key, &Value::new([7]).unwrap())
            .unwrap();
        assert_eq!(
            session(&cfg, 4)
                .unwrap()
                .get(ns, &key)
                .unwrap()
                .unwrap()
                .as_bytes(),
            &[7]
        );
        assert!(fs::read_dir(root.path())
            .unwrap()
            .all(|entry| entry.unwrap().file_name() == "validator-4"));
        drop(writer);
        completion
            .wait_timeout(std::time::Duration::from_secs(5))
            .unwrap();
    }
}

mod configuration;
mod tribute;
#[cfg(any(test, feature = "ocomp-integration"))]
pub(crate) use configuration::ensure_node_config;
use configuration::reject_symlink_path;
pub(crate) use configuration::{configure_node_command, storage_config};
use tribute::*;
