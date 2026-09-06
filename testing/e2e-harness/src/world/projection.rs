//! Backend-neutral projection fixture. Runtime processes consume only the generated TOML.

use std::fs;
use std::io::Write;
use std::thread::sleep;
use std::time::{Duration, Instant};

use eyre::{bail, eyre, Result, WrapErr};
use outbe_compressed_entities::{decode_stored_tribute_v1, WwdEntityId};
use outbe_offchain_storage::{
    Key, MongoStorageConfig, Namespace, RocksDbConfig, ScanEntry, ScanRequest, StorageBackend,
    StorageConfig, StorageProvider, StorageReader, StorageReaderHandle,
};

use super::mongodb::MongoDb;
use crate::env::{Environment, ProjectionBackend};
use crate::internal::config::Config;
use crate::ocomp_evidence::sha256_hex;

const COLLECTIONS: [&str; 3] = ["tributes", "tributes_by_owner", "tributes_by_day"];

#[derive(Debug)]
pub struct ProjectionFixture {
    cfg: Config,
    mongo: Option<MongoDb>,
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

/// Create once, privately. A restart validates and reuses the operator's exact file.
pub(crate) fn ensure_node_config(cfg: &Config, index: usize) -> Result<()> {
    let path = cfg.projection_storage_config(index);
    if path.exists() {
        StorageConfig::load(&path)?;
        return Ok(());
    }
    fs::create_dir_all(cfg.validator_dir(index))?;
    let backend = match cfg.projection_backend {
        ProjectionBackend::RocksDb => StorageBackend::RocksDb(RocksDbConfig {
            path: cfg.validator_dir(index).join("data/offchain"),
            secondary_path: cfg.validator_dir(index).join("ocomp/rocksdb-secondary"),
        }),
        ProjectionBackend::MongoDb => StorageBackend::MongoDb(MongoStorageConfig {
            uri: cfg.projection_mongodb_uri.clone(),
            database: cfg.validator_projection_database(index),
        }),
    };
    let document = StorageConfig {
        start_block: 1,
        backend,
    }
    .to_toml()?;
    let mut file = tempfile::NamedTempFile::new_in(cfg.validator_dir(index))?;
    file.write_all(document.as_bytes())?;
    file.as_file().sync_all()?;
    match file.persist_noclobber(&path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            StorageConfig::load(&path)?;
        }
        Err(error) => return Err(error.error.into()),
    }
    Ok(())
}

/// Recovered follower argv may already carry the flag from its original node launch.
pub(crate) fn configure_node_command(
    cfg: &Config,
    index: usize,
    command: &mut std::process::Command,
) -> Result<()> {
    ensure_node_config(cfg, index)?;
    let expected = cfg.projection_storage_config(index);
    let args = command.get_args().collect::<Vec<_>>();
    let mut occurrences = 0;
    for (position, argument) in args.iter().enumerate() {
        let text = argument.to_string_lossy();
        let path = if text == "--projection.storage-config" {
            Some(std::path::PathBuf::from(
                args.get(position + 1)
                    .ok_or_else(|| eyre!("storage-config missing value"))?,
            ))
        } else {
            text.strip_prefix("--projection.storage-config=")
                .map(std::path::PathBuf::from)
        };
        if let Some(path) = path {
            occurrences += 1;
            if path != expected {
                bail!("node role transition changed its projection storage config");
            }
        }
    }
    if occurrences > 1 {
        bail!("duplicate projection storage config arguments");
    }
    if occurrences == 0 {
        command.arg("--projection.storage-config").arg(expected);
    }
    Ok(())
}

fn session(cfg: &Config, index: usize) -> Result<StorageReaderHandle> {
    let config = StorageConfig::load(cfg.projection_storage_config(index))?;
    Ok(StorageProvider::new(config)?
        .read_source("e2e-observer")?
        .open_session()?)
}

impl ProjectionFixture {
    pub(crate) fn connect_or_start(cfg: &mut Config) -> Result<Self> {
        let mongo = match cfg.projection_backend {
            ProjectionBackend::RocksDb => None,
            ProjectionBackend::MongoDb => Some(MongoDb::connect_or_start(cfg)?),
        };
        Ok(Self {
            cfg: cfg.clone(),
            mongo,
        })
    }

    pub(crate) fn teardown_managed_for_run(env: &Environment) {
        if env.projection_backend == ProjectionBackend::MongoDb {
            MongoDb::teardown_managed_for_run(env);
        }
    }

    pub fn pause_managed(&self) -> Result<()> {
        self.mongo
            .as_ref()
            .ok_or_else(|| eyre!("MongoDB outage scenario requires --projection-backend mongodb"))?
            .pause_managed()
    }

    pub fn resume_managed(&self) -> Result<()> {
        self.mongo
            .as_ref()
            .ok_or_else(|| eyre!("MongoDB outage scenario requires --projection-backend mongodb"))?
            .resume_managed()
    }

    /// The caller must stop all nodes before deliberate re-bootstrap.
    pub fn reset_projection_state(&self) -> Result<()> {
        if let Some(mongo) = &self.mongo {
            return mongo.reset_projection_state();
        }
        for index in 0..self.cfg.validators {
            let path = self.cfg.projection_storage_config(index);
            if !path.exists() {
                continue;
            }
            if let StorageBackend::RocksDb(config) = StorageConfig::load(path)?.backend {
                for path in [config.path, config.secondary_path] {
                    if path.exists() {
                        let canonical = path.canonicalize()?;
                        if !canonical.starts_with(self.cfg.dir.canonicalize()?) {
                            bail!("refusing to reset storage outside this scenario");
                        }
                        fs::remove_dir_all(path)?;
                    }
                }
            }
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
                    let canonical = snapshot(session(&cfg, 0)?.as_ref(), &tx_hash)?;
                    for index in 1..validators {
                        let observed = snapshot(session(&cfg, index)?.as_ref(), &tx_hash)?;
                        if canonical != observed { bail!("validator-{index}: projection differs from validator-0"); }
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
        self.run(move |cfg| {
            let record = primary(session(&cfg, validator)?.as_ref(), &tx_hash)?;
            Ok(ProjectedTribute {
                raw_id: WwdEntityId::try_from(record.key.as_bytes())?,
                stored_body: record.value.as_bytes().to_vec(),
            })
        })
    }

    pub fn tribute_projection_snapshot(
        &self,
        validator: usize,
        tx_hash: &str,
    ) -> Result<TributeProjectionSnapshot> {
        let tx_hash = tx_hash.to_owned();
        self.run(move |cfg| snapshot(session(&cfg, validator)?.as_ref(), &tx_hash))
    }

    pub fn assert_no_tribute_projection(&self) -> Result<()> {
        self.run(|cfg| {
            for index in 0..cfg.validators {
                let reader = session(&cfg, index)?;
                for name in COLLECTIONS {
                    let page = reader
                        .scan_prefix(Namespace::new(name)?, ScanRequest::new(&[], None, 1)?)?;
                    if !page.entries.is_empty() {
                        bail!("validator-{index}.{name}: expected no records");
                    }
                }
            }
            Ok(())
        })
    }
}

fn primary(reader: &dyn StorageReader, tx_hash: &str) -> Result<ScanEntry> {
    let namespace = Namespace::new(COLLECTIONS[0])?;
    let mut after = None;
    let mut found = None;
    loop {
        let page = reader.scan_prefix(
            namespace.clone(),
            ScanRequest::new(&[], after.as_ref(), 256)?,
        )?;
        for entry in page.entries {
            if entry
                .metadata
                .as_ref()
                .and_then(|m| m.get("tx_hash"))
                .is_some_and(|tx| tx.eq_ignore_ascii_case(tx_hash))
                && found.replace(entry).is_some()
            {
                bail!("multiple Tribute records for transaction {tx_hash}");
            }
        }
        after = page.next_after;
        if after.is_none() {
            break;
        }
    }
    found.ok_or_else(|| eyre!("no Tribute for transaction {tx_hash}"))
}

fn snapshot(reader: &dyn StorageReader, tx_hash: &str) -> Result<TributeProjectionSnapshot> {
    let primary = primary(reader, tx_hash)?;
    let raw_id = WwdEntityId::try_from(primary.key.as_bytes())?;
    let body =
        decode_stored_tribute_v1(primary.value.as_bytes()).wrap_err("decode projected Tribute")?;
    if body.tribute_id != raw_id {
        bail!("Tribute primary key does not match its body");
    }
    let owner_key = [body.owner.as_slice(), raw_id.as_slice()].concat();
    let day_key = [
        body.worldwide_day.value().to_be_bytes().as_slice(),
        raw_id.as_slice(),
    ]
    .concat();
    let index = |name: &str, key: Vec<u8>| -> Result<ScanEntry> {
        let key = Key::new(key)?;
        let record = reader
            .get_record(Namespace::new(name)?, &key)?
            .ok_or_else(|| eyre!("missing {name} index"))?;
        if !record.value.as_bytes().is_empty() {
            bail!("{name} index value must be empty");
        }
        Ok(ScanEntry {
            key,
            value: record.value,
            metadata: record.metadata,
        })
    };
    Ok(TributeProjectionSnapshot {
        records: [
            primary,
            index(COLLECTIONS[1], owner_key)?,
            index(COLLECTIONS[2], day_key)?,
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_offchain_storage::{
        AtomicWriteBatch, AtomicWriteOperation, MemoryStorage, StorageMetadata, StorageWriter,
        Value,
    };
    use std::collections::BTreeMap;

    #[test]
    fn transaction_lookup_pages_and_rejects_duplicate_matches() {
        let storage = MemoryStorage::default();
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
        cfg.dir = root.path().to_path_buf();
        let mut command = std::process::Command::new("node");
        configure_node_command(&cfg, 4, &mut command).unwrap();
        configure_node_command(&cfg, 4, &mut command).unwrap();
        assert_eq!(command.get_args().count(), 2);
        assert!(configure_node_command(&cfg, 5, &mut command).is_err());
    }

    #[test]
    fn node_config_is_shared_with_readers_and_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = Config::resolve(&Environment::default());
        cfg.dir = root.path().to_path_buf();
        ensure_node_config(&cfg, 4).unwrap();
        let path = cfg.projection_storage_config(4);
        let original = fs::read(&path).unwrap();
        cfg.projection_backend = ProjectionBackend::MongoDb;
        ensure_node_config(&cfg, 4).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        let config = StorageConfig::load(&path).unwrap();
        let writer = StorageProvider::new(config).unwrap().open_writer().unwrap();
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
    }
}
