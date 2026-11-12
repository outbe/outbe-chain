use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
};

use eyre::{bail, ensure, Context, Result};
use outbe_snapshot::manifest::{EntryKind, NativeRoot, SnapshotManifestV1};
use sha2::{Digest, Sha256};

use crate::{support::process, validator::config::OperatorConfig};

const MAX_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;

struct ExpectedMember<'a> {
    output: PathBuf,
    kind: EntryKind,
    size: u64,
    sha256: Option<&'a str>,
    mode: u32,
}

pub fn restore(path: &Path, config: &OperatorConfig, resume: bool) -> Result<u64> {
    let manifest = read_manifest(path)?;
    ensure!(
        manifest.chain_id == config.network.chain_id,
        "snapshot chain id {} does not match {}",
        manifest.chain_id,
        config.network.chain_id
    );
    if resume {
        clear_destinations(&manifest)?;
    } else {
        ensure_empty_destinations(&manifest)?;
    }
    extract(path, &manifest)?;
    process::run(
        "chown",
        &[
            "-R",
            "outbe:outbe",
            "/opt/outbe-chain/validator-0/data",
            "/opt/outbe-chain/validator-0/consensus",
        ],
    )?;
    Ok(manifest.progress.finalized.number)
}

fn read_manifest(path: &Path) -> Result<SnapshotManifestV1> {
    let mut archive = tar::Archive::new(open(path)?);
    let mut raw = None;
    for entry in archive
        .entries()
        .wrap_err("failed to read snapshot archive")?
    {
        let mut entry = entry.wrap_err("failed to read snapshot entry")?;
        if entry.path()?.as_ref() != Path::new("manifest.json") {
            continue;
        }
        ensure!(raw.is_none(), "snapshot contains duplicate manifest.json");
        ensure!(
            entry.header().entry_type().is_file() && entry.size() <= MAX_MANIFEST_BYTES,
            "invalid snapshot manifest entry"
        );
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .wrap_err("failed to read snapshot manifest")?;
        raw = Some(bytes);
    }
    let raw = raw.ok_or_else(|| eyre::eyre!("snapshot is missing manifest.json"))?;
    SnapshotManifestV1::from_bytes(&raw).wrap_err("invalid snapshot manifest")
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
    let mut expected = expected_members(manifest)?;
    let mut archive = tar::Archive::new(open(path)?);
    for entry in archive
        .entries()
        .wrap_err("failed to read snapshot archive")?
    {
        let mut entry = entry.wrap_err("failed to read snapshot entry")?;
        let path = entry.path()?.into_owned();
        let path_text = path
            .to_str()
            .ok_or_else(|| eyre::eyre!("snapshot contains a non-UTF-8 path"))?;
        if matches!(path_text, "manifest.json" | "signature.json") {
            continue;
        }
        let member = expected
            .remove(path_text)
            .ok_or_else(|| eyre::eyre!("undeclared snapshot entry: {path_text}"))?;
        ensure!(
            entry.size() == member.size,
            "snapshot size mismatch: {path_text}"
        );
        match member.kind {
            EntryKind::Directory => {
                ensure!(
                    entry.header().entry_type().is_dir(),
                    "invalid snapshot directory: {path_text}"
                );
                fs::create_dir_all(&member.output)?;
                fs::set_permissions(
                    &member.output,
                    fs::Permissions::from_mode(member.mode & 0o777),
                )?;
            }
            EntryKind::File => {
                ensure!(
                    entry.header().entry_type().is_file(),
                    "invalid snapshot file: {path_text}"
                );
                if let Some(parent) = member.output.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&member.output)
                    .wrap_err_with(|| format!("failed to restore {}", member.output.display()))?;
                let digest = copy_digest(&mut entry, &mut file)?;
                ensure!(
                    member.sha256 == Some(digest.as_str()),
                    "snapshot checksum mismatch: {path_text}"
                );
                fs::set_permissions(
                    &member.output,
                    fs::Permissions::from_mode(member.mode & 0o777),
                )?;
            }
        }
    }
    if let Some(path) = expected.keys().next() {
        bail!("snapshot is missing declared entry: {path}");
    }
    Ok(())
}

fn expected_members(manifest: &SnapshotManifestV1) -> Result<BTreeMap<String, ExpectedMember<'_>>> {
    let mut expected = BTreeMap::new();
    for domain in &manifest.domains {
        let root = destination(domain.native_root);
        let wrapper = format!("payload/{}", domain.id);
        expected.insert(
            wrapper,
            ExpectedMember {
                output: root.clone(),
                kind: EntryKind::Directory,
                size: 0,
                sha256: None,
                mode: domain.mode,
            },
        );
        for entry in &domain.entries {
            if entry.path.is_empty() && entry.kind == EntryKind::Directory {
                continue;
            }
            let relative = safe_relative_path(&entry.path)?;
            let archive_path = format!("payload/{}/{}", domain.id, entry.path);
            ensure!(
                expected
                    .insert(
                        archive_path.clone(),
                        ExpectedMember {
                            output: root.join(relative),
                            kind: entry.kind,
                            size: entry.size,
                            sha256: entry.sha256.as_deref(),
                            mode: entry.mode,
                        },
                    )
                    .is_none(),
                "duplicate snapshot entry: {archive_path}"
            );
        }
    }
    Ok(expected)
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
