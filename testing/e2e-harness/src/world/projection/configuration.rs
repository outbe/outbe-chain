use super::*;

/// Create once, privately. A restart validates and reuses the operator's exact file.
pub(crate) fn ensure_node_config(cfg: &Config, index: usize) -> Result<()> {
    let path = cfg.projection_storage_config(index);
    reject_symlink_path(&path)?;
    if path.exists() {
        storage_config(cfg, index)?;
        if cfg.projection_backend == crate::env::ProjectionBackend::Mongodb {
            mongo::ensure(cfg)?;
        }
        return Ok(());
    }
    fs::create_dir_all(cfg.validator_dir(index))?;
    let directory = cfg.validator_dir(index).canonicalize()?;
    reject_symlink_path(&directory.join("data/offchain"))?;
    reject_symlink_path(&directory.join("ocomp/rocksdb-secondary"))?;
    let backend = match cfg.projection_backend {
        crate::env::ProjectionBackend::Rocksdb => StorageBackend::RocksDb(RocksDbConfig {
            path: directory.join("data/offchain"),
            secondary_path: directory.join("ocomp/rocksdb-secondary"),
        }),
        crate::env::ProjectionBackend::Mongodb => {
            mongo::ensure(cfg)?;
            StorageBackend::MongoDb(mongo::config(cfg, index)?)
        }
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
            storage_config(cfg, index)?;
        }
        Err(error) => return Err(error.error.into()),
    }
    storage_config(cfg, index)?;
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

/// Validate the exact scenario-owned storage identity at each consumer.
/// This read-only check starts no service and changes no configuration.
pub(crate) fn storage_config(cfg: &Config, index: usize) -> Result<StorageConfig> {
    reject_symlink_path(&cfg.projection_storage_config(index))?;
    let config = StorageConfig::load(cfg.projection_storage_config(index))?;
    if cfg.projection_backend == crate::env::ProjectionBackend::Mongodb {
        if config.start_block != 1
            || config.backend != StorageBackend::MongoDb(mongo::config(cfg, index)?)
        {
            bail!("validator-{index}: E2E MongoDB storage identity changed");
        }
        return Ok(config);
    }
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

pub(super) fn reject_symlink_path(path: &std::path::Path) -> Result<()> {
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
