use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use alloy_primitives::{Address, U256};
use eyre::{ensure, Context, Result};

use crate::{
    support::{process, progress::Progress},
    validator::{
        bundle::{self, OperatorManifest},
        config::OperatorConfig,
        install,
        journal::Journal,
        path_text,
        rpc::RpcClient,
        validator_address,
    },
};

pub async fn run(bundle_path: &Path, config_path: &Path) -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let mut progress = Progress::new();
    progress.stage("verifying every operator bundle file");
    let extracted = bundle::verify_and_extract(bundle_path, Path::new("/opt"))?;
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
    let root = Path::new(process::ROOT);
    if root.exists() {
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
        progress.done("host is already prepared with this exact bundle and config");
        print_summary(
            validator,
            minimum_stake,
            &journal.config.network.certified_rpc,
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
    config.write_runtime_config(staging, validator)?;
    fs::set_permissions(staging, fs::Permissions::from_mode(0o755))?;

    progress.stage("installing systemd services without starting them");
    let bundle_sha256 = extracted.archive_sha256;
    let staging_path = extracted.directory.keep();
    fs::rename(&staging_path, root).wrap_err("failed to publish /opt/outbe-chain")?;
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
        minimum_stake,
        &journal.config.network.certified_rpc,
    );
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

fn print_summary(validator: Address, minimum_stake: U256, certified_rpc: &str) {
    let funding = minimum_stake + U256::from(1_000_000_000_000_000_000_u64);
    println!();
    println!("Validator files:");
    println!("  address:          {validator:#x}");
    println!("  BLS key:          /opt/outbe-chain/keys/signing-key.hex");
    println!("  EVM key:          /opt/outbe-chain/keys/evm-key.hex");
    println!("  Reth P2P key:     /opt/outbe-chain/keys/reth-p2p-secret.hex");
    println!("  Radicle identity: /opt/outbe-chain/keys/radicle");
    println!();
    println!("Fund this address before `outbe-up validator promote`:");
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
