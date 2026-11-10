use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
};

use eyre::{bail, ensure, Context, Result};
use outbe_snapshot::manifest::{EntryKind, NativeRoot, SnapshotManifestV1};
use sha2::{Digest, Sha256};

use crate::{config::OperatorConfig, process};

pub fn restore(path: &Path, config: &OperatorConfig, resume: bool) -> Result<u64> {
    let index = outbe_snapshot::archive::read_archive_index(open(path)?, None)
        .wrap_err("snapshot integrity verification failed")?;
    ensure!(
        index.manifest.chain_id == config.network.chain_id,
        "snapshot chain id {} does not match {}",
        index.manifest.chain_id,
        config.network.chain_id
    );
    ensure!(
        index.manifest.genesis_hash == config.network.genesis_hash,
        "snapshot genesis hash does not match operator config"
    );
    if resume {
        clear_destinations(&index.manifest)?;
    } else {
        ensure_empty_destinations(&index.manifest)?;
    }
    extract(path, &index.manifest)?;
    process::run(
        "chown",
        &[
            "-R",
            "outbe:outbe",
            "/opt/outbe-chain/validator-0/data",
            "/opt/outbe-chain/validator-0/consensus",
        ],
    )?;
    Ok(index.manifest.progress.finalized.number)
}

fn clear_destinations(manifest: &SnapshotManifestV1) -> Result<()> {
    let mut roots = manifest
        .domains
        .iter()
        .map(|domain| destination(domain.native_root))
        .collect::<Vec<_>>();
    roots.sort_by_key(|path| path.components().count());
    roots.dedup();
    for root in &roots {
        if roots
            .iter()
            .any(|candidate| candidate != root && root.starts_with(candidate))
        {
            continue;
        }
        match fs::remove_dir_all(root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).wrap_err_with(|| format!("failed to clear {}", root.display()));
            }
        }
    }
    Ok(())
}

fn extract(path: &Path, manifest: &SnapshotManifestV1) -> Result<()> {
    let mut archive = tar::Archive::new(open(path)?);
    let mut entries = archive
        .entries()
        .wrap_err("failed to read snapshot archive")?;
    for name in ["manifest.json", "signature.json"] {
        let entry = entries
            .next()
            .ok_or_else(|| eyre::eyre!("snapshot is missing {name}"))??;
        ensure!(
            entry.path()?.as_ref() == Path::new(name),
            "invalid snapshot metadata order"
        );
    }
    for domain in &manifest.domains {
        let wrapper = entries
            .next()
            .ok_or_else(|| eyre::eyre!("snapshot is missing domain {}", domain.id))??;
        ensure!(
            wrapper.header().entry_type().is_dir()
                && wrapper.path()?.as_ref() == Path::new(&format!("payload/{}", domain.id)),
            "snapshot domain wrapper is not a directory"
        );
        drop(wrapper);
        let root = destination(domain.native_root);
        fs::create_dir_all(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(domain.mode & 0o777))?;
        for expected in &domain.entries {
            if expected.path.is_empty() && expected.kind == EntryKind::Directory {
                continue;
            }
            let relative = safe_relative_path(&expected.path)?;
            let mut entry = entries
                .next()
                .ok_or_else(|| eyre::eyre!("snapshot is missing {}", expected.path))??;
            ensure!(
                entry.path()?.as_ref()
                    == Path::new(&format!("payload/{}/{}", domain.id, expected.path)),
                "snapshot entry path does not match its manifest"
            );
            let output = root.join(relative);
            match expected.kind {
                EntryKind::Directory => {
                    ensure!(
                        entry.header().entry_type().is_dir(),
                        "invalid directory entry"
                    );
                    fs::create_dir_all(&output)?;
                    fs::set_permissions(
                        &output,
                        fs::Permissions::from_mode(expected.mode & 0o777),
                    )?;
                }
                EntryKind::File => {
                    ensure!(entry.header().entry_type().is_file(), "invalid file entry");
                    if let Some(parent) = output.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let mut file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&output)
                        .wrap_err_with(|| format!("failed to restore {}", output.display()))?;
                    let digest = copy_digest(&mut entry, &mut file)?;
                    ensure!(
                        expected.sha256.as_deref() == Some(digest.as_str()),
                        "snapshot checksum mismatch: {}",
                        expected.path
                    );
                    fs::set_permissions(
                        &output,
                        fs::Permissions::from_mode(expected.mode & 0o777),
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn ensure_empty_destinations(manifest: &SnapshotManifestV1) -> Result<()> {
    for root in manifest
        .domains
        .iter()
        .map(|domain| destination(domain.native_root))
    {
        if root.exists()
            && fs::read_dir(&root)
                .wrap_err_with(|| format!("failed to inspect {}", root.display()))?
                .next()
                .is_some()
        {
            bail!("snapshot destination is not empty: {}", root.display());
        }
    }
    Ok(())
}

fn destination(root: NativeRoot) -> PathBuf {
    let data = Path::new(process::ROOT).join("validator-0/data");
    match root {
        NativeRoot::Chain => data,
        NativeRoot::Consensus => Path::new(process::ROOT).join("validator-0/consensus"),
        NativeRoot::StaticFiles => data.join("static_files"),
        NativeRoot::ExecutionRocksDb => data.join("rocksdb"),
    }
}

fn open(path: &Path) -> Result<Box<dyn Read>> {
    let file = File::open(path)
        .wrap_err_with(|| format!("failed to open snapshot: {}", path.display()))?;
    if path.extension().is_some_and(|extension| extension == "zst") {
        return Ok(Box::new(
            zstd::Decoder::new(file).wrap_err("failed to open zstd snapshot")?,
        ));
    }
    Ok(Box::new(file))
}

fn safe_relative_path(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe snapshot path: {value}");
    }
    Ok(path.to_path_buf())
}

fn copy_digest(reader: &mut impl Read, writer: &mut impl Write) -> Result<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        writer.write_all(&buffer[..count])?;
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}
