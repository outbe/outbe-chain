use std::{
    fs,
    path::{Path, PathBuf},
};

use eyre::{ensure, Context, Result};

use crate::{
    support::{process, progress::Progress},
    validator::{journal::Journal, maintenance, sync, validator_address, VALIDATOR_TARGET},
};

mod restore;

pub(crate) use restore::restore;

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
    let joining = is_joining_layout(Path::new(root), index);
    node_args_for_layout(root, index, joining)
}

fn is_joining_layout(root: &Path, index: usize) -> bool {
    index == 0 && root.join("keys/evm-key.hex").is_file()
}

fn node_args_for_layout(root: &str, index: usize, joining: bool) -> Vec<String> {
    let validator = format!("{root}/validator-{index}");
    let keys = if joining {
        format!("{root}/keys")
    } else {
        format!("{root}/keys/validator-{index}")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn founder_snapshot_uses_the_packaged_key_directory() {
        let args = node_args_for_layout("/opt/outbe-chain", 3, false);
        assert!(args
            .iter()
            .any(|value| value == "/opt/outbe-chain/keys/validator-3/evm-key.hex"));
        assert!(!args
            .iter()
            .any(|value| value == "/opt/outbe-chain/validator-3/evm-key.hex"));
    }

    #[test]
    fn joining_snapshot_uses_the_shared_key_directory() {
        let args = node_args_for_layout("/opt/outbe-chain", 0, true);
        assert!(args
            .iter()
            .any(|value| value == "/opt/outbe-chain/keys/evm-key.hex"));
    }

    #[test]
    fn joining_layout_requires_the_direct_validator_key() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("keys/validator-0")).unwrap();
        assert!(!is_joining_layout(root.path(), 0));
        fs::write(root.path().join("keys/evm-key.hex"), "key").unwrap();
        assert!(is_joining_layout(root.path(), 0));
        assert!(!is_joining_layout(root.path(), 1));
    }
}
