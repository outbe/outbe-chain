use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use alloy_primitives::{Address, U256};
use eyre::{ensure, Context, Result};

use crate::{
    support::{process, progress::Progress},
    validator::{
        bundle::{self, OperatorManifest},
        config::OperatorConfig,
        install,
        journal::{self, Journal},
        path_text,
        rpc::RpcClient,
        validator_address, FEEDER_FUNDING_WEI,
    },
};

pub async fn run(bundle_path: &Path, config_path: &Path) -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let mut progress = Progress::new();
    let root = Path::new(process::ROOT);
    let prepared = root.exists() && journal::path().is_file();
    let existing_empty_root = if prepared {
        false
    } else {
        validate_unprepared_root(root)?
    };
    let staging_parent = if existing_empty_root {
        root
    } else {
        root.parent().unwrap_or_else(|| Path::new("/opt"))
    };

    progress.stage("verifying every operator bundle file");
    let extracted = bundle::verify_and_extract(bundle_path, staging_parent)?;
    verify_bootstrap_binary(extracted.directory.path())?;

    progress.stage("validating operator configuration and remote network");
    let (config, config_sha256) = OperatorConfig::read(config_path)?;
    verify_bundle_network(extracted.directory.path(), &config)?;
    let remote = RpcClient::new(config.network.certified_rpc.clone())?;
    ensure!(
        remote.chain_id().await? == config.network.chain_id,
        "certified RPC chain id does not match operator config"
    );
    let minimum_stake = remote.min_stake().await?;
    if prepared {
        let mut journal = Journal::load()?;
        ensure!(
            journal.bundle_sha256 == extracted.archive_sha256
                && journal.config_sha256 == config_sha256,
            "host was prepared with a different bundle or config"
        );
        let installed = OperatorManifest::read(&root.join(bundle::MANIFEST_NAME))?;
        installed.verify_directory(root)?;
        if !journal.installed {
            install::run()?;
            journal.installed = true;
            journal.save()?;
        }
        let validator = validator_address(&root.join("keys/evm-key.hex"))?;
        let hyperlane = validator_address(&root.join("keys/hyperlane-key.hex"))?;
        progress.done("host is already prepared with this exact bundle and config");
        print_summary(
            validator,
            hyperlane,
            minimum_stake,
            &journal.config.network.certified_rpc,
            journal.config.hyperlane.signer_funding_wei.parse()?,
        );
        return Ok(());
    }
    progress.stage("generating host-local validator identities");
    let staging = extracted.directory.path();
    let keys = staging.join("keys");
    let keygen = staging.join("outbe-keygen");
    process::capture(
        &keygen,
        &[
            "validator",
            "--output-dir",
            path_text(&keys)?,
            "--chain-id",
            &config.network.chain_id.to_string(),
        ],
    )?;
    let validator = validator_address(&keys.join("evm-key.hex"))?;
    let hyperlane = validator_address(&keys.join("hyperlane-key.hex"))?;
    config.write_runtime_config(staging, validator)?;
    fs::set_permissions(staging, fs::Permissions::from_mode(0o755))?;

    progress.stage("installing systemd services without starting them");
    let bundle_sha256 = extracted.archive_sha256;
    publish_staging(extracted.directory, root, existing_empty_root)?;
    let mut journal = Journal::new(bundle_sha256, config_sha256, config);
    journal.save()?;
    install::run()?;
    journal.installed = true;
    journal.save()?;

    progress.done(&format!(
        "prepared validator {validator:#x}; no services started"
    ));
    print_summary(
        validator,
        hyperlane,
        minimum_stake,
        &journal.config.network.certified_rpc,
        journal.config.hyperlane.signer_funding_wei.parse()?,
    );
    Ok(())
}

fn validate_unprepared_root(root: &Path) -> Result<bool> {
    if !root.exists() {
        return Ok(false);
    }
    let metadata = fs::symlink_metadata(root)?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && fs::read_dir(root)?.next().is_none(),
        "{} exists without a lifecycle journal and is not an empty directory",
        root.display()
    );
    Ok(true)
}

fn publish_staging(
    staging: tempfile::TempDir,
    root: &Path,
    existing_empty_root: bool,
) -> Result<()> {
    if !existing_empty_root {
        let staging_path = staging.keep();
        fs::rename(&staging_path, root).wrap_err("failed to publish /opt/outbe-chain")?;
        return Ok(());
    }

    fs::set_permissions(root, fs::Permissions::from_mode(0o755))?;
    let staging_root = staging.path().to_path_buf();
    let mut entries = fs::read_dir(&staging_root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<PathBuf>>>()?;
    entries.sort();
    let mut published = Vec::new();
    for source in entries {
        let name = source
            .file_name()
            .ok_or_else(|| eyre::eyre!("invalid staging entry: {}", source.display()))?;
        let destination = root.join(name);
        if destination.exists() {
            for (source, destination) in published.iter().rev() {
                let _ = fs::rename(destination, source);
            }
            eyre::bail!(
                "cannot publish {}: {} already exists",
                source.display(),
                destination.display()
            );
        }
        if let Err(error) = fs::rename(&source, &destination) {
            for (source, destination) in published.iter().rev() {
                let _ = fs::rename(destination, source);
            }
            return Err(error).wrap_err_with(|| {
                format!(
                    "failed to publish {} into {}",
                    source.display(),
                    root.display()
                )
            });
        }
        published.push((source, destination));
    }
    Ok(())
}

fn verify_bootstrap_binary(root: &Path) -> Result<()> {
    let current = std::env::current_exe().wrap_err("failed to locate the running outbe-up")?;
    ensure!(
        bundle::digest_file(&current)? == bundle::digest_file(&root.join("outbe-up"))?,
        "the running outbe-up does not match the operator bundle; run the outbe-up shipped with this bundle"
    );
    Ok(())
}

fn verify_bundle_network(root: &Path, config: &OperatorConfig) -> Result<()> {
    let genesis = root.join("genesis.json");
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&genesis)?)?;
    let chain_id = value
        .pointer("/config/chainId")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| eyre::eyre!("genesis.json has no numeric config.chainId"))?;
    ensure!(
        chain_id == config.network.chain_id,
        "genesis.json chain id does not match operator config"
    );
    Ok(())
}

fn print_summary(
    validator: Address,
    hyperlane: Address,
    minimum_stake: U256,
    certified_rpc: &str,
    hyperlane_funding: U256,
) {
    let funding = minimum_stake
        + hyperlane_funding
        + U256::from(FEEDER_FUNDING_WEI)
        + U256::from(1_000_000_000_000_000_000_u64);
    println!();
    println!("Validator files:");
    println!("  address:          {validator:#x}");
    println!("  BLS key:          /opt/outbe-chain/keys/signing-key.hex");
    println!("  EVM key:          /opt/outbe-chain/keys/evm-key.hex");
    println!("  Feeder key:       /opt/outbe-chain/keys/feeder-key.hex");
    println!("  Hyperlane key:    /opt/outbe-chain/keys/hyperlane-key.hex");
    println!("  Hyperlane signer: {hyperlane:#x}");
    println!("  Reth P2P key:     /opt/outbe-chain/keys/reth-p2p-secret.hex");
    println!("  Radicle identity: /opt/outbe-chain/keys/radicle");
    println!();
    println!("Fund this address before `outbe-up validator sync` (registration gas and stake):");
    println!("VALIDATOR_ADDR='{validator:#x}'");
    println!("CERTIFIED_RPC='{certified_rpc}'");
    println!("MINIMUM_STAKE_WEI='{minimum_stake}'");
    println!("FUNDING_AMOUNT_WEI='{funding}'");
    println!("printf 'Funder private key: '");
    println!("read -r -s FUNDER_PRIVATE_KEY");
    println!("printf '\\n'");
    println!("cast send \"$VALIDATOR_ADDR\" \\");
    println!("  --value \"$FUNDING_AMOUNT_WEI\" \\");
    println!("  --rpc-url \"$CERTIFIED_RPC\" \\");
    println!("  --private-key \"$FUNDER_PRIVATE_KEY\"");
    println!("unset FUNDER_PRIVATE_KEY");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_an_existing_empty_install_directory() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("outbe-chain");
        fs::create_dir(&root).unwrap();
        assert!(validate_unprepared_root(&root).unwrap());
        assert!(root.is_dir());
    }

    #[test]
    fn rejects_an_unjournaled_nonempty_install_directory() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("outbe-chain");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("unexpected"), b"data").unwrap();
        assert!(validate_unprepared_root(&root).is_err());
    }

    #[test]
    fn rejects_an_unjournaled_symlink_install_directory() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("target");
        let root = parent.path().join("outbe-chain");
        fs::create_dir(&target).unwrap();
        symlink(&target, &root).unwrap();

        assert!(validate_unprepared_root(&root).is_err());
    }

    #[test]
    fn publishes_into_an_existing_empty_install_directory() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("outbe-chain");
        fs::create_dir(&root).unwrap();
        let staging = tempfile::tempdir_in(&root).unwrap();
        fs::write(staging.path().join("file"), b"data").unwrap();

        publish_staging(staging, &root, true).unwrap();

        assert_eq!(fs::read(root.join("file")).unwrap(), b"data");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }

    #[test]
    fn rolls_back_a_partial_publish() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("outbe-chain");
        fs::create_dir(&root).unwrap();
        let staging = tempfile::tempdir_in(&root).unwrap();
        fs::write(staging.path().join("a"), b"first").unwrap();
        fs::write(staging.path().join("b"), b"second").unwrap();
        fs::write(root.join("b"), b"collision").unwrap();

        assert!(publish_staging(staging, &root, true).is_err());
        assert!(!root.join("a").exists());
        assert_eq!(fs::read(root.join("b")).unwrap(), b"collision");
    }
}
