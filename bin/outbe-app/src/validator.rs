use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use alloy_primitives::{keccak256, Address, U256};
use eyre::{bail, ensure, Context, Result};
use k256::ecdsa::SigningKey;
use rand::RngCore;
use zeroize::Zeroizing;

use crate::{
    bundle::{self, OperatorManifest},
    config::OperatorConfig,
    journal::Journal,
    process::{self, LifecycleLock},
    progress::Progress,
    rpc::RpcClient,
    snapshot,
    wait::{
        wait_for_rpc_chain, wait_for_signer, wait_for_sync, wait_for_verifier,
        wait_transaction_finalized,
    },
};

const LOCAL_RPC: &str = "http://127.0.0.1:8545";
const FULL_NODE: &str = "outbe-full-node.service";
const VALIDATOR_TARGET: &str = "outbe-validator.target";
const VALIDATOR_SERVICE: &str = "outbe-validator.service";
const ENCLAVE_SERVICE: &str = "outbe-enclave.service";
const RENEW_TIMER: &str = "outbe-tee-renew.timer";

pub async fn prepare(bundle_path: &Path, config_path: &Path) -> Result<()> {
    let _lock = LifecycleLock::acquire()?;
    let mut progress = Progress::new();
    progress.stage("validating operator configuration and remote network");
    let (config, config_sha256) = OperatorConfig::read(config_path)?;
    let remote = RpcClient::new(config.network.certified_rpc.clone())?;
    ensure!(
        remote.chain_id().await? == config.network.chain_id,
        "certified RPC chain id does not match operator config"
    );
    let remote_genesis = remote.block_number(0).await?.hash;
    ensure!(
        remote_genesis.strip_prefix("0x").unwrap_or(&remote_genesis) == config.network.genesis_hash,
        "certified RPC genesis hash does not match operator config"
    );
    let minimum_stake = remote.min_stake().await?;

    progress.stage("verifying every operator bundle file");
    let extracted = bundle::verify_and_extract(bundle_path, Path::new("/opt"))?;
    verify_bundle_network(extracted.directory.path(), &config)?;
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
            process::capture(root.join("deploy/validator/install.sh"), &[])?;
            journal.installed = true;
            journal.save()?;
        }
        let validator = validator_address(&root.join("keys/evm-key.hex"))?;
        progress.done("host is already prepared with this exact bundle and config");
        print_prepare_summary(
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
    let keys_text = path_text(&keys)?;
    process::capture(
        &keygen,
        &[
            "validator",
            "--output-dir",
            keys_text,
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
    process::capture(root.join("deploy/validator/install.sh"), &[])?;
    journal.installed = true;
    journal.save()?;

    progress.done(&format!(
        "prepared validator {validator:#x}; no services started"
    ));
    print_prepare_summary(
        validator,
        minimum_stake,
        &journal.config.network.certified_rpc,
    );
    Ok(())
}

pub async fn sync(snapshot_path: Option<&Path>) -> Result<()> {
    let _lock = LifecycleLock::acquire()?;
    let mut journal = Journal::load()?;
    let mut progress = Progress::new();
    ensure!(
        !process::systemctl_is_active(VALIDATOR_TARGET)
            && !process::systemctl_is_active(VALIDATOR_SERVICE),
        "validator stack is already active; refusing to restore or replace its data"
    );

    if let Some(path) = snapshot_path {
        let digest = bundle::digest_file(path)?;
        match journal.snapshot_sha256.as_deref() {
            Some(existing) if existing == digest => {
                progress.done("requested snapshot was already restored");
            }
            Some(_) => {
                bail!("a different snapshot was already restored on this host");
            }
            None => {
                ensure!(
                    !process::systemctl_is_active(FULL_NODE),
                    "stop the full node before restoring a snapshot"
                );
                let resume = match journal.snapshot_restore_in_progress.as_deref() {
                    Some(existing) => {
                        ensure!(
                            existing == digest,
                            "an interrupted restore used a different snapshot"
                        );
                        true
                    }
                    None => {
                        journal.snapshot_restore_in_progress = Some(digest.clone());
                        journal.save()?;
                        false
                    }
                };
                progress.stage("verifying and restoring the snapshot");
                let height = snapshot::restore(path, &journal.config, resume)?;
                journal.snapshot_sha256 = Some(digest);
                journal.snapshot_restore_in_progress = None;
                journal.snapshot_finalized_height = Some(height);
                journal.save()?;
                progress.done(&format!("snapshot restored at finalized height {height}"));
            }
        }
    }

    progress.stage("starting keyless full node");
    process::systemctl(&["enable", "--now", FULL_NODE])?;
    let local = RpcClient::new(LOCAL_RPC)?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    wait_for_rpc_chain(&local, journal.config.network.chain_id, &mut progress).await?;
    progress.stage("synchronizing keyless full node");
    let (height, peers) = wait_for_sync(&local, &remote, &mut progress).await?;
    progress.done(&format!(
        "full node synchronized at finalized height {height} with {peers} peers"
    ));
    Ok(())
}

pub async fn promote() -> Result<()> {
    let _lock = LifecycleLock::acquire()?;
    let mut journal = Journal::load()?;
    let mut progress = Progress::new();
    let local = RpcClient::new(LOCAL_RPC)?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    let validator = validator_address(&Path::new(process::ROOT).join("keys/evm-key.hex"))?;

    progress.stage("checking synchronized full node and validator state");
    ensure!(
        process::systemctl_is_active(FULL_NODE) || process::systemctl_is_active(VALIDATOR_SERVICE),
        "run `outbe-app validator sync` before promotion"
    );
    wait_for_rpc_chain(&local, journal.config.network.chain_id, &mut progress).await?;
    wait_for_sync(&local, &remote, &mut progress).await?;
    let minimum = remote.min_stake().await?;
    let mut record = remote.validator(validator).await?;
    if !record.registered {
        if let Some(hash) = journal.stake_transaction.as_deref() {
            progress.stage("resuming previously submitted validator registration");
            wait_transaction_finalized(&remote, hash, &mut progress).await?;
            record = remote.validator(validator).await?;
        }
    }
    if !record.registered {
        let balance = remote.balance(validator).await?;
        ensure!(
            balance >= minimum,
            "validator {validator:#x} needs at least {minimum} base units; balance is {balance}"
        );
    }

    progress.stage("starting enclave and completing TEE admission");
    process::systemctl(&["start", ENCLAVE_SERVICE])?;
    ensure_join_identity(&mut journal, &remote).await?;
    run_tee_join(&journal)?;
    journal.tee_join_complete = true;
    journal.save()?;

    if !record.registered {
        progress.stage("bonding the network minimum stake");
        let validator_arg = format!("{validator:#x}");
        let amount = minimum.to_string();
        let symmetric = format!("{}:30400", journal.config.network.external_ip);
        let output = run_outbe_cli(&[
            "staking",
            "stake",
            "--validator",
            &validator_arg,
            "--amount",
            &amount,
            "--keys-dir",
            "/opt/outbe-chain/keys",
            "--symmetric",
            &symmetric,
        ])?;
        let hash = transaction_hash(&output)?;
        journal.stake_transaction = Some(hash.clone());
        journal.save()?;
        wait_transaction_finalized(&remote, &hash, &mut progress).await?;
        record = remote.validator(validator).await?;
        ensure!(
            record.registered,
            "validator registration is not visible after finality"
        );
    }

    progress.stage("switching the same data directory to validator mode");
    process::systemctl(&["disable", "--now", FULL_NODE])?;
    process::systemctl(&["enable", "--now", VALIDATOR_TARGET])?;
    process::systemctl(&["enable", "--now", RENEW_TIMER])?;
    wait_for_rpc_chain(&local, journal.config.network.chain_id, &mut progress).await?;
    wait_for_verifier(&local, &remote, &mut progress).await?;

    record = remote.validator(validator).await?;
    if record.status == 1 && journal.confirm_ready_transaction.is_none() {
        progress.stage("confirming synchronized validator readiness");
        let output = run_outbe_cli(&["validator", "confirm-ready"])?;
        let hash = transaction_hash(&output)?;
        journal.confirm_ready_transaction = Some(hash.clone());
        journal.save()?;
        wait_transaction_finalized(&remote, &hash, &mut progress).await?;
    } else if let Some(hash) = journal.confirm_ready_transaction.as_deref() {
        wait_transaction_finalized(&remote, hash, &mut progress).await?;
    }

    progress.stage("waiting for DKG signer activation");
    let activation = wait_for_signer(&local, &remote, validator, &mut progress).await?;
    progress.done(&format!(
        "validator {validator:#x} is active signer; DKG activation height {activation}"
    ));
    Ok(())
}

fn verify_bundle_network(root: &Path, config: &OperatorConfig) -> Result<()> {
    let genesis = root.join("genesis.json");
    ensure!(
        bundle::digest_file(&genesis)? == config.network.genesis_sha256,
        "genesis.json checksum does not match operator config"
    );
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

pub(crate) fn validator_address(path: &Path) -> Result<Address> {
    let encoded = Zeroizing::new(
        fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read validator key: {}", path.display()))?,
    );
    let mut key = Zeroizing::new([0_u8; 32]);
    hex::decode_to_slice(
        encoded.trim().strip_prefix("0x").unwrap_or(encoded.trim()),
        key.as_mut(),
    )
    .wrap_err("validator EVM key is not 32-byte hex")?;
    let signing = SigningKey::from_slice(key.as_ref()).wrap_err("invalid validator EVM key")?;
    let public = signing.verifying_key().to_encoded_point(false);
    let raw = public
        .as_bytes()
        .get(1..)
        .filter(|bytes| bytes.len() == 64)
        .ok_or_else(|| eyre::eyre!("invalid validator public key"))?;
    Ok(Address::from_slice(&keccak256(raw)[12..]))
}

async fn ensure_join_identity(journal: &mut Journal, remote: &RpcClient) -> Result<()> {
    if journal.binding_id.is_none() {
        let mut binding = [0_u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut binding);
        if binding.iter().all(|byte| *byte == 0) {
            binding[31] = 1;
        }
        journal.binding_id = Some(hex::encode(binding));
    }
    if journal.valid_until.is_none() {
        let (finalized, policy) = remote.finalized_tee_lease_policy().await?;
        let duration = journal
            .config
            .tee
            .lease_duration_seconds
            .resolve(policy.minimum, policy.maximum)?;
        journal.valid_until = Some(
            finalized
                .timestamp
                .checked_add(duration)
                .ok_or_else(|| eyre::eyre!("TEE lease deadline overflow"))?,
        );
    }
    journal.save()
}

fn run_tee_join(journal: &Journal) -> Result<()> {
    let valid_until = journal
        .valid_until
        .expect("valid_until was saved")
        .to_string();
    run_outbe_cli(&[
        "tee",
        "join",
        "--enclave-socket",
        "127.0.0.1:17000",
        "--node-data-dir",
        "/opt/outbe-chain/validator-0/data",
        "--reth-p2p-secret-key",
        "/opt/outbe-chain/keys/reth-p2p-secret.hex",
        "--genesis",
        "/opt/outbe-chain/genesis.json",
        "--binding-id",
        journal.binding_id.as_deref().expect("binding id was saved"),
        "--valid-until",
        &valid_until,
        "--timeout-secs",
        "600",
    ])?;
    Ok(())
}

fn run_outbe_cli(args: &[&str]) -> Result<String> {
    let journal = Journal::load()?;
    let binary = Path::new(process::ROOT).join("outbe-cli");
    let binary = path_text(&binary)?;
    let mut full = vec![
        "--user",
        "outbe",
        "--",
        binary,
        "--rpc-url",
        journal.config.network.certified_rpc.as_str(),
        "--private-key-file",
        "/opt/outbe-chain/keys/evm-key.hex",
    ];
    full.extend_from_slice(args);
    process::capture("runuser", &full)
}

fn print_prepare_summary(validator: Address, minimum_stake: U256, certified_rpc: &str) {
    let funding = minimum_stake + U256::from(1_000_000_000_000_000_000_u64);
    println!();
    println!("Validator files:");
    println!("  address:          {validator:#x}");
    println!("  BLS key:          /opt/outbe-chain/keys/signing-key.hex");
    println!("  EVM key:          /opt/outbe-chain/keys/evm-key.hex");
    println!("  Reth P2P key:     /opt/outbe-chain/keys/reth-p2p-secret.hex");
    println!("  Radicle identity: /opt/outbe-chain/keys/radicle");
    println!();
    println!("Fund this address before `outbe-app validator promote`:");
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

fn transaction_hash(output: &str) -> Result<String> {
    output
        .split_whitespace()
        .find(|word| {
            word.len() == 66
                && word.starts_with("0x")
                && word[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .map(str::to_owned)
        .ok_or_else(|| eyre::eyre!("command did not return a transaction hash: {output}"))
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| eyre::eyre!("path is not valid UTF-8: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_transaction_hash_from_cli_output() {
        let hash = format!("0x{}", "a".repeat(64));
        assert_eq!(
            transaction_hash(&format!("Transaction sent: {hash}\n")).unwrap(),
            hash
        );
    }
}
