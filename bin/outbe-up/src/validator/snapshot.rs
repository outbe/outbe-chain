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

use crate::{
    support::{process, progress::Progress},
    validator::{
        config::OperatorConfig, journal::Journal, maintenance, sync, validator_address,
        VALIDATOR_TARGET,
    },
};

const MAX_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;

pub async fn create(output: &Path, signing_key: Option<&Path>, skip_dkg_guard: bool) -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let journal = Journal::load()?;
    let was_active = process::systemctl_is_active(VALIDATOR_TARGET);
    if was_active {
        maintenance::local_guard(&journal, skip_dkg_guard).await?;
    }
    let enabled_on_boot = process::services_enabled();
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    ensure!(
        !output.exists(),
        "snapshot output already exists: {}",
        output.display()
    );
    ensure_snapshot_space(parent, 0)?;
    let signing_key = signing_key
        .map(Path::to_owned)
        .unwrap_or_else(|| Path::new(process::ROOT).join("keys/evm-key.hex"));

    if was_active {
        process::systemctl(&["stop", VALIDATOR_TARGET])?;
    }
    let operation = create_offline(output, &signing_key, &journal);
    let restart = if was_active {
        let mut progress = Progress::new();
        sync::start_and_wait(&journal, &mut progress, enabled_on_boot).await
    } else {
        Ok(())
    };
    operation?;
    restart.wrap_err("snapshot was created, but validator failed to restart")?;
    println!("snapshot created: {}", output.display());
    Ok(())
}

pub async fn validate(
    archive: &Path,
    expected_signer: Option<&str>,
    skip_dkg_guard: bool,
) -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let journal = Journal::load()?;
    ensure!(
        archive.is_file(),
        "snapshot does not exist: {}",
        archive.display()
    );
    let was_active = process::systemctl_is_active(VALIDATOR_TARGET);
    if was_active {
        maintenance::local_guard(&journal, skip_dkg_guard).await?;
        process::systemctl(&["stop", VALIDATOR_TARGET])?;
    }
    let enabled_on_boot = process::services_enabled();
    let operation = validate_offline(archive, expected_signer);
    let restart = if was_active {
        let mut progress = Progress::new();
        sync::start_and_wait(&journal, &mut progress, enabled_on_boot).await
    } else {
        Ok(())
    };
    operation?;
    restart.wrap_err("snapshot is valid, but validator failed to restart")?;
    println!("snapshot validated: {}", archive.display());
    Ok(())
}

fn create_offline(output: &Path, signing_key: &Path, journal: &Journal) -> Result<()> {
    let root = Path::new(process::ROOT);
    let creator = validator_address(&root.join("keys/evm-key.hex"))?;
    let mut args = vec![
        "snapshot".to_owned(),
        "create".to_owned(),
        "--output".to_owned(),
        path_string(output)?,
        "--signing-key".to_owned(),
        path_string(signing_key)?,
        "--creator".to_owned(),
        format!("{creator:#x}"),
    ];
    if let Some(source) = &journal.software_commit {
        args.extend(["--source".to_owned(), source.clone()]);
    }
    args.push("--".into());
    args.extend(node_args(0));
    run_owned(root.join("outbe-chain"), &args)
}

fn validate_offline(archive: &Path, expected_signer: Option<&str>) -> Result<()> {
    let root = Path::new(process::ROOT);
    let mut args = vec![
        "snapshot".to_owned(),
        "validate".to_owned(),
        "--archive".to_owned(),
        path_string(archive)?,
    ];
    if let Some(signer) = expected_signer {
        args.extend(["--expected-signer".to_owned(), signer.to_owned()]);
    }
    args.push("--".into());
    args.extend(node_args(0));
    run_owned(root.join("outbe-chain"), &args)
}

pub(crate) fn node_args(index: usize) -> Vec<String> {
    let root = process::ROOT;
    let validator = format!("{root}/validator-{index}");
    let joining = index == 0 && Path::new(root).join("keys").is_dir();
    let keys = if joining {
        format!("{root}/keys")
    } else {
        validator.clone()
    };
    let reth_config = if joining {
        format!("{root}/reth.toml")
    } else {
        format!("{validator}/reth.toml")
    };
    vec![
        "--config".into(),
        reth_config,
        "--chain".into(),
        format!("{root}/genesis.json"),
        "--datadir".into(),
        format!("{validator}/data"),
        "--consensus.storage-dir".into(),
        format!("{validator}/consensus"),
        "--consensus.keys-dir".into(),
        keys.clone(),
        "--p2p-secret-key".into(),
        format!("{keys}/reth-p2p-secret.hex"),
        "--consensus.signing-key".into(),
        format!("{keys}/signing-key.hex"),
        "--validator.evm-key".into(),
        format!("{keys}/evm-key.hex"),
    ]
}

pub(crate) fn ensure_snapshot_space(parent: &Path, index: usize) -> Result<()> {
    let root = Path::new(process::ROOT);
    let validator = root.join(format!("validator-{index}"));
    let required = [validator.join("data"), validator.join("consensus")]
        .into_iter()
        .filter(|path| path.exists())
        .map(|path| {
            process::capture("du", &["-sk", path_text(&path)?])?
                .split_whitespace()
                .next()
                .ok_or_else(|| eyre::eyre!("invalid du output"))?
                .parse::<u64>()
                .wrap_err("invalid du size")
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .sum::<u64>();
    let available = process::capture("df", &["-Pk", path_text(parent)?])?
        .lines()
        .last()
        .and_then(|line| line.split_whitespace().nth(3))
        .ok_or_else(|| eyre::eyre!("invalid df output"))?
        .parse::<u64>()?;
    let required_with_headroom = required.saturating_add(required / 10);
    ensure!(
        available >= required_with_headroom,
        "snapshot needs approximately {required_with_headroom} KiB, only {available} KiB is available"
    );
    Ok(())
}

fn run_owned(program: PathBuf, args: &[String]) -> Result<()> {
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    process::run(program, &refs)
}

fn path_string(path: &Path) -> Result<String> {
    Ok(path_text(path)?.to_owned())
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| eyre::eyre!("path is not UTF-8: {}", path.display()))
}

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
