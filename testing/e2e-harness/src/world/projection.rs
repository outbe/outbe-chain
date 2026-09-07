//! RocksDB projection fixture. Runtime processes consume only the generated TOML.

use std::fs;
use std::io::Write;
use std::thread::sleep;
use std::time::{Duration, Instant};

use eyre::{bail, eyre, Result, WrapErr};
use outbe_compressed_entities::{decode_stored_tribute_v1, WwdEntityId};
use outbe_offchain_storage::{
    Key, Namespace, RocksDbConfig, ScanEntry, ScanRequest, StorageBackend, StorageConfig,
    StorageProvider, StorageReader, StorageReaderHandle,
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

/// Create once, privately. A restart validates and reuses the operator's exact file.
pub(crate) fn ensure_node_config(cfg: &Config, index: usize) -> Result<()> {
    let path = cfg.projection_storage_config(index);
    reject_symlink_path(&path)?;
    if path.exists() {
        rocksdb_config(cfg, index)?;
        return Ok(());
    }
    fs::create_dir_all(cfg.validator_dir(index))?;
    let directory = cfg.validator_dir(index).canonicalize()?;
    reject_symlink_path(&directory.join("data/offchain"))?;
    reject_symlink_path(&directory.join("ocomp/rocksdb-secondary"))?;
    let backend = StorageBackend::RocksDb(RocksDbConfig {
        path: directory.join("data/offchain"),
        secondary_path: directory.join("ocomp/rocksdb-secondary"),
    });
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
            rocksdb_config(cfg, index)?;
        }
        Err(error) => return Err(error.error.into()),
    }
    rocksdb_config(cfg, index)?;
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

/// Validate again at each consumer: a persisted Mongo config must never start a service
/// or silently redirect a restarted node/exporter to another storage identity.
pub(crate) fn rocksdb_config(cfg: &Config, index: usize) -> Result<StorageConfig> {
    reject_symlink_path(&cfg.projection_storage_config(index))?;
    let config = StorageConfig::load(cfg.projection_storage_config(index))?;
    let StorageBackend::RocksDb(ref rocks) = config.backend else {
        bail!("E2E requires RocksDB storage for validator-{index}");
    };
    let expected = cfg.validator_dir(index).canonicalize()?;
    reject_symlink_path(&rocks.path)?;
    reject_symlink_path(&rocks.secondary_path)?;
    if config.start_block != 1
        || rocks.path != expected.join("data/offchain")
        || rocks.secondary_path != expected.join("ocomp/rocksdb-secondary")
    {
        bail!("validator-{index}: E2E RocksDB storage identity changed");
    }
    Ok(config)
}

fn reject_symlink_path(path: &std::path::Path) -> Result<()> {
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "E2E storage path must not traverse symlink {}",
                    ancestor.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn session(cfg: &Config, index: usize) -> Result<StorageReaderHandle> {
    let config = rocksdb_config(cfg, index)?;
    Ok(StorageProvider::new(config)?
        .read_source("e2e-observer")?
        .open_session()?)
}

impl ProjectionFixture {
    pub(crate) fn new(cfg: &Config) -> Self {
        Self { cfg: cfg.clone() }
    }

    /// The caller must stop all nodes before deliberate re-bootstrap.
    pub fn reset_projection_state(&self) -> Result<()> {
        let mut targets = Vec::new();
        for index in 0..self.cfg.validators {
            let path = self.cfg.projection_storage_config(index);
            if !path.exists() {
                continue;
            }
            if let StorageBackend::RocksDb(config) = rocksdb_config(&self.cfg, index)?.backend {
                for path in [config.path, config.secondary_path] {
                    if path.exists() {
                        let canonical = path.canonicalize()?;
                        if !canonical.starts_with(self.cfg.dir.canonicalize()?) {
                            bail!("refusing to reset storage outside this scenario");
                        }
                        targets.push(path);
                    }
                }
            }
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
        cfg.dir = root.path().to_path_buf();
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
        cfg.dir = root.path().to_path_buf();
        ensure_node_config(&cfg, 0).unwrap();
        let path = cfg.projection_storage_config(0);
        let incompatible = "version = 1\nbackend = \"mongodb\"\n[mongodb]\nuri = \"mongodb://127.0.0.1:1\"\ndatabase = \"forbidden\"\n";
        fs::write(&path, incompatible).unwrap();
        assert!(ensure_node_config(&cfg, 0)
            .unwrap_err()
            .to_string()
            .contains("requires RocksDB"));
        assert!(session(&cfg, 0).is_err());
        assert!(ProjectionFixture::new(&cfg)
            .reset_projection_state()
            .is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), incompatible);
    }

    #[test]
    fn persisted_storage_identity_and_start_block_cannot_change() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = Config::resolve(&Environment::default());
        cfg.dir = root.path().to_path_buf();
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
        cfg.dir = root.path().to_path_buf();
        ensure_node_config(&cfg, 0).unwrap();
        let data = cfg.validator_dir(0).join("data");
        fs::create_dir_all(&data).unwrap();
        std::os::unix::fs::symlink(external.path(), data.join("offchain")).unwrap();
        fs::write(external.path().join("sentinel"), "preserved").unwrap();
        assert!(ensure_node_config(&cfg, 0).is_err());
        assert!(session(&cfg, 0).is_err());
        assert!(ProjectionFixture::new(&cfg)
            .reset_projection_state()
            .is_err());
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
        cfg.dir = root.path().to_path_buf();
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
        cfg.dir = root.path().to_path_buf();
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
        assert!(ProjectionFixture::new(&cfg)
            .reset_projection_state()
            .is_err());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "preserved");
    }

    #[test]
    fn node_config_is_shared_with_readers_and_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = Config::resolve(&Environment::default());
        cfg.dir = root.path().to_path_buf();
        ensure_node_config(&cfg, 4).unwrap();
        let path = cfg.projection_storage_config(4);
        let original = fs::read(&path).unwrap();
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
