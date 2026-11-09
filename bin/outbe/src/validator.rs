use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    time::Duration,
};

use alloy_primitives::{keccak256, Address};
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
    rpc::{ConsensusStatus, RpcClient, ValidatorRecord},
    snapshot,
};

const LOCAL_RPC: &str = "http://127.0.0.1:8545";
const FULL_NODE: &str = "outbe-full-node.service";
const VALIDATOR_TARGET: &str = "outbe-validator.target";
const VALIDATOR_SERVICE: &str = "outbe-validator.service";
const ENCLAVE_SERVICE: &str = "outbe-enclave.service";
const RADICLE_SERVICE: &str = "outbe-radicle.service";
const FEEDER_SERVICE: &str = "outbe-feeder.service";
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
            process::run(root.join("deploy/validator/install.sh"), &[])?;
            journal.installed = true;
            journal.save()?;
        }
        progress.done("host is already prepared with this exact bundle and config");
        return Ok(());
    }

    progress.stage("generating host-local validator identities");
    let staging = extracted.directory.path();
    let keys = staging.join("keys");
    let keygen = staging.join("outbe-keygen");
    let keys_text = path_text(&keys)?;
    process::run(
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
    write_runtime_config(staging, &config, validator)?;
    fs::set_permissions(staging, fs::Permissions::from_mode(0o755))?;

    progress.stage("installing systemd services without starting them");
    let bundle_sha256 = extracted.archive_sha256;
    let staging_path = extracted.directory.keep();
    fs::rename(&staging_path, root).wrap_err("failed to publish /opt/outbe-chain")?;
    let mut journal = Journal::new(bundle_sha256, config_sha256, config);
    journal.save()?;
    process::run(root.join("deploy/validator/install.sh"), &[])?;
    journal.installed = true;
    journal.save()?;

    progress.done(&format!(
        "prepared validator {validator:#x}; no services started"
    ));
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
            Some(_) => bail!("a different snapshot was already restored on this host"),
            None => {
                ensure!(
                    !process::systemctl_is_active(FULL_NODE),
                    "stop the full node before restoring a snapshot"
                );
                progress.stage("verifying and restoring the snapshot");
                let height = snapshot::restore(path, &journal.config)?;
                journal.snapshot_sha256 = Some(digest);
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

    progress.stage("checking synchronized full node and validator balance");
    ensure!(
        process::systemctl_is_active(FULL_NODE) || process::systemctl_is_active(VALIDATOR_SERVICE),
        "run `outbe validator sync` before promotion"
    );
    wait_for_rpc_chain(&local, journal.config.network.chain_id, &mut progress).await?;
    wait_for_sync(&local, &remote, &mut progress).await?;
    let minimum = remote.min_stake().await?;
    let balance = remote.balance(validator).await?;
    ensure!(
        balance >= minimum,
        "validator {validator:#x} needs at least {minimum} base units; balance is {balance}"
    );

    progress.stage("starting enclave and completing TEE admission");
    process::systemctl(&["start", ENCLAVE_SERVICE])?;
    ensure_join_identity(&mut journal, &remote).await?;
    run_tee_join(&journal)?;
    journal.tee_join_complete = true;
    journal.save()?;

    let mut record = remote.validator(validator).await?;
    if !record.registered {
        if let Some(hash) = journal.stake_transaction.as_deref() {
            progress.stage("resuming previously submitted validator registration");
            wait_transaction_finalized(&remote, hash, &mut progress).await?;
            record = remote.validator(validator).await?;
        }
    }
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

    let activation = wait_for_signer(&local, &remote, validator, &mut progress).await?;
    progress.done(&format!(
        "validator {validator:#x} is active signer; DKG activation height {activation}"
    ));
    Ok(())
}

pub async fn status(watch: bool) -> Result<()> {
    process::require_root()?;
    let journal = Journal::load()?;
    let local = RpcClient::new(LOCAL_RPC)?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    let validator = validator_address(&Path::new(process::ROOT).join("keys/evm-key.hex"))?;
    let mut progress = Progress::new();
    loop {
        let line = status_line(&local, &remote, validator).await;
        if watch {
            progress.update(&line);
            tokio::time::sleep(Duration::from_secs(5)).await;
        } else {
            println!("{line}");
            return Ok(());
        }
    }
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

fn write_runtime_config(root: &Path, config: &OperatorConfig, validator: Address) -> Result<()> {
    for value in config
        .network
        .bootnodes
        .iter()
        .chain(config.network.consensus_peers.iter())
        .chain(std::iter::once(&config.network.certified_rpc))
    {
        ensure!(
            !value
                .chars()
                .any(|character| matches!(character, '"' | '\n' | '\r')),
            "operator config contains a value that cannot be written to validator.env"
        );
    }
    let extra = if config.network.use_local_defaults {
        "--consensus.use-local-defaults"
    } else {
        ""
    };
    let env = format!(
        concat!(
            "OUTBE_EXTERNAL_IP=\"{}\"\n",
            "OUTBE_CERTIFIED_RPC=\"{}\"\n",
            "OUTBE_VALIDATOR_ADDRESS=\"{:#x}\"\n",
            "OUTBE_BOOTNODES=\"{}\"\n",
            "OUTBE_CONSENSUS_PEERS=\"{}\"\n",
            "OUTBE_CONSENSUS_EXTRA_ARGS=\"{}\"\n",
            "OUTBE_ENCLAVE_RUNTIME=\"{}\"\n",
            "OUTBE_CHAIN_ID=\"{}\"\n"
        ),
        config.network.external_ip,
        config.network.certified_rpc,
        validator,
        config.network.bootnodes.join(","),
        config.network.consensus_peers.join(","),
        extra,
        config.tee.runtime.as_env(),
        config.network.chain_id,
    );
    write_private(&root.join("validator.env"), env.as_bytes())?;
    write_private(
        &root.join("feeder-public.toml"),
        config.feeder.public_toml.as_bytes(),
    )
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn validator_address(path: &Path) -> Result<Address> {
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
        let timestamp = remote.finalized_block().await?.timestamp;
        journal.valid_until = Some(
            timestamp
                .checked_add(journal.config.tee.lease_duration_seconds)
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
    let mut full = vec![
        "--rpc-url",
        journal.config.network.certified_rpc.as_str(),
        "--private-key-file",
        "/opt/outbe-chain/keys/evm-key.hex",
    ];
    full.extend_from_slice(args);
    process::capture(Path::new(process::ROOT).join("outbe-cli"), &full)
}

async fn wait_for_rpc_chain(
    rpc: &RpcClient,
    expected_chain: u64,
    progress: &mut Progress,
) -> Result<()> {
    loop {
        match rpc.chain_id().await {
            Ok(chain) => {
                ensure!(
                    chain == expected_chain,
                    "local RPC is on chain {chain}, expected {expected_chain}"
                );
                return Ok(());
            }
            Err(error) => progress.update(&format!("waiting for local RPC: {error}")),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn wait_for_sync(
    local: &RpcClient,
    remote: &RpcClient,
    progress: &mut Progress,
) -> Result<(u64, u64)> {
    loop {
        let remote_block = remote.finalized_block().await?;
        let local_block = local.finalized_block().await?;
        let peers = local.peer_count().await.unwrap_or(0);
        let remaining = remote_block.number.saturating_sub(local_block.number);
        progress.update(&format!(
            "sync local={} remote={} remaining={} peers={}",
            local_block.number, remote_block.number, remaining, peers
        ));
        if peers > 0 && local_block.number >= remote_block.number {
            let local_at_remote = local.block_number(remote_block.number).await?;
            if local_at_remote.hash == remote_block.hash {
                return Ok((remote_block.number, peers));
            }
            bail!(
                "local and certified RPC finalized hashes disagree at height {}",
                remote_block.number
            );
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn wait_for_verifier(
    local: &RpcClient,
    remote: &RpcClient,
    progress: &mut Progress,
) -> Result<()> {
    loop {
        wait_for_sync(local, remote, progress).await?;
        let status = local.consensus_status().await?;
        progress.update(&format!(
            "promotion mode={} peers={} shares={}",
            status.mode, status.connected_peers, status.has_threshold_shares
        ));
        if status.is_validator && matches!(status.mode.as_str(), "verifier" | "signer") {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn wait_for_signer(
    local: &RpcClient,
    remote: &RpcClient,
    validator: Address,
    progress: &mut Progress,
) -> Result<u64> {
    loop {
        let status = local.consensus_status().await?;
        let record = remote.validator(validator).await?;
        let remaining = status
            .next_planned_activation_height
            .saturating_sub(status.last_finalized_block);
        progress.update(&format!(
            "DKG mode={} shares={} peers={} activation={} remaining={}",
            status.mode,
            status.has_threshold_shares,
            status.connected_peers,
            status.next_planned_activation_height,
            remaining
        ));
        if status.mode == "signer"
            && status.is_active
            && status.has_threshold_shares
            && status.connected_peers > 0
            && status
                .enclave
                .as_ref()
                .is_some_and(|enclave| enclave.ready && enclave.offer_key_ready)
            && record.status == 2
            && record.has_bls_share
        {
            wait_for_sync(local, remote, progress).await?;
            return Ok(status.next_planned_activation_height);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn wait_transaction_finalized(
    remote: &RpcClient,
    hash: &str,
    progress: &mut Progress,
) -> Result<()> {
    loop {
        if let Some(block) = remote.transaction_block(hash).await? {
            let finalized = remote.finalized_block().await?.number;
            progress.update(&format!(
                "transaction {hash} block={block} finalized={finalized}"
            ));
            if finalized >= block {
                return Ok(());
            }
        } else {
            progress.update(&format!("waiting for transaction {hash}"));
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn status_line(local: &RpcClient, remote: &RpcClient, validator: Address) -> String {
    let remote_height = remote
        .finalized_block()
        .await
        .ok()
        .map(|block| block.number);
    let local_height = local.finalized_block().await.ok().map(|block| block.number);
    let consensus = local.consensus_status().await.ok();
    let record = remote.validator(validator).await.ok();
    format_status(
        local_height,
        remote_height,
        consensus.as_ref(),
        record.as_ref(),
    )
}

fn format_status(
    local: Option<u64>,
    remote: Option<u64>,
    consensus: Option<&ConsensusStatus>,
    validator: Option<&ValidatorRecord>,
) -> String {
    let services = format!(
        "fullnode={} validator={} enclave={} radicle={} feeder={} renew={}",
        state(FULL_NODE),
        state(VALIDATOR_SERVICE),
        state(ENCLAVE_SERVICE),
        state(RADICLE_SERVICE),
        state(FEEDER_SERVICE),
        state(RENEW_TIMER),
    );
    let chain = format!(
        "local={} remote={}",
        local.map_or_else(|| "?".into(), |value| value.to_string()),
        remote.map_or_else(|| "?".into(), |value| value.to_string())
    );
    let mode = consensus.map_or_else(
        || "mode=? peers=? shares=? enclave=?".to_owned(),
        |value| {
            let enclave = value
                .enclave
                .as_ref()
                .is_some_and(|enclave| enclave.ready && enclave.offer_key_ready);
            format!(
                "mode={} peers={} shares={} enclave={}",
                value.mode, value.connected_peers, value.has_threshold_shares, enclave
            )
        },
    );
    let lifecycle = validator.map_or_else(
        || "validator=?".to_owned(),
        |value| {
            format!(
                "validator_status={} bls_share={}",
                value.status, value.has_bls_share
            )
        },
    );
    format!("{services} | {chain} | {mode} | {lifecycle}")
}

fn state(unit: &str) -> &'static str {
    if process::systemctl_is_active(unit) {
        "active"
    } else {
        "inactive"
    }
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
