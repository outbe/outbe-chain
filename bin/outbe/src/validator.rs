use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

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
    progress::{Progress, WatchDisplay},
    rpc::{Block, ConsensusStatus, EpochInfo, RpcClient, ValidatorRecord},
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

struct StatusSnapshot<'a> {
    local: Option<&'a Block>,
    remote: Option<&'a Block>,
    execution_peers: Option<u64>,
    consensus: Option<&'a ConsensusStatus>,
    validator: Option<&'a ValidatorRecord>,
    local_epoch: Option<&'a EpochInfo>,
    remote_epoch: Option<&'a EpochInfo>,
    block_seconds: Option<f64>,
}

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
        "run `outbe validator sync` before promotion"
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
    if !watch {
        println!("{}", status_line(&local, &remote, validator).await);
        return Ok(());
    }

    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    let display = WatchDisplay::new();
    loop {
        let line = status_line(&local, &remote, validator).await;
        display.render(&line);
        tokio::select! {
            result = &mut interrupt => {
                result.wrap_err("failed to listen for Ctrl-C")?;
                return Ok(());
            }
            () = tokio::time::sleep(Duration::from_secs(5)) => {}
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
        let block_seconds = estimate_block_seconds(remote, &remote_block).await;
        progress.update(&format!(
            "sync h={}/{} left={}{} p2p={}",
            local_block.number,
            remote_block.number,
            remaining,
            eta_field(remaining, block_seconds),
            peers
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
            "validator bootstrap mode={} signers={} shares={}",
            status.mode,
            status.connected_peers,
            yes_no(status.has_threshold_shares)
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
        let remote_block = remote.finalized_block().await?;
        let remaining = status
            .next_planned_activation_height
            .saturating_sub(remote_block.number);
        let block_seconds = estimate_block_seconds(remote, &remote_block).await;
        progress.update(&format!(
            "DKG mode={} shares={} signers={} activation={} left={}{}",
            status.mode,
            yes_no(status.has_threshold_shares),
            status.connected_peers,
            status.next_planned_activation_height,
            remaining,
            eta_field(remaining, block_seconds)
        ));
        if status.is_ready_signer(&record) {
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
    let (remote_block, local_block, execution_peers, consensus, record, local_epoch, remote_epoch) = tokio::join!(
        remote.finalized_block(),
        local.finalized_block(),
        local.peer_count(),
        local.consensus_status(),
        remote.validator(validator),
        local.epoch_info(),
        remote.epoch_info(),
    );
    let remote_block = remote_block.ok();
    let block_seconds = match remote_block.as_ref() {
        Some(block) => estimate_block_seconds(remote, block).await,
        None => None,
    };
    format_status(StatusSnapshot {
        local: local_block.as_ref().ok(),
        remote: remote_block.as_ref(),
        execution_peers: execution_peers.ok(),
        consensus: consensus.as_ref().ok(),
        validator: record.as_ref().ok(),
        local_epoch: local_epoch.as_ref().ok(),
        remote_epoch: remote_epoch.as_ref().ok(),
        block_seconds,
    })
}

fn format_status(snapshot: StatusSnapshot<'_>) -> String {
    let fullnode = process::systemctl_is_active(FULL_NODE);
    let validator_service = process::systemctl_is_active(VALIDATOR_SERVICE);
    let chain = chain_status(snapshot.local, snapshot.remote, snapshot.block_seconds);

    match (fullnode, validator_service) {
        (true, true) => format!(
            "Mode        conflict\nChain       {chain}\nServices    full-node active | validator active"
        ),
        (true, false) => {
            let peers = snapshot
                .execution_peers
                .map_or_else(|| "?".to_owned(), |value| value.to_string());
            let epoch = epoch_status(snapshot.local_epoch, snapshot.remote_epoch);
            format!(
                "Mode        full node\nChain       {chain}\nNetwork     execution peers {peers}\nEpochs      {epoch}\nServices    full-node active | validator inactive"
            )
        }
        (false, true) => {
            let lifecycle = snapshot
                .validator
                .map_or_else(|| "unknown".to_owned(), validator_lifecycle);
            let consensus = validator_consensus_status(
                snapshot.consensus,
                snapshot.remote,
                snapshot.block_seconds,
            );
            let services = validator_services_status();
            let bls = snapshot
                .validator
                .map_or("?", |record| yes_no(record.has_bls_share));
            format!(
                "Mode        validator ({lifecycle})\nChain       {chain}\n{consensus}\nOn-chain    BLS share {bls}\nServices    {services}"
            )
        }
        (false, false) => {
            format!("Mode        stopped\nChain       {chain}\nServices    node services inactive")
        }
    }
}

fn chain_status(local: Option<&Block>, remote: Option<&Block>, seconds: Option<f64>) -> String {
    let heights = format!(
        "local {} | remote {}",
        local.map_or_else(|| "?".into(), |block| block.number.to_string()),
        remote.map_or_else(|| "?".into(), |block| block.number.to_string())
    );
    let (Some(local), Some(remote)) = (local, remote) else {
        return heights;
    };
    let remaining = remote.number.saturating_sub(local.number);
    if remaining == 0 {
        format!("{heights} | synchronized")
    } else {
        format!(
            "{heights} | {remaining} blocks behind{}",
            eta_suffix(remaining, seconds)
        )
    }
}

fn epoch_status(local: Option<&EpochInfo>, remote: Option<&EpochInfo>) -> String {
    match (local, remote) {
        (Some(local), Some(remote)) if local.epoch_number == remote.epoch_number => {
            format!("{} | reconstruction complete", local.epoch_number)
        }
        (Some(local), Some(remote)) => {
            let remaining = remote.epoch_number.saturating_sub(local.epoch_number);
            format!(
                "local {} | remote {} | reconstructing ({remaining} remaining)",
                local.epoch_number, remote.epoch_number
            )
        }
        (Some(local), None) => format!("local {} | remote unavailable", local.epoch_number),
        (None, Some(remote)) => format!("local unavailable | remote {}", remote.epoch_number),
        (None, None) => "unavailable".to_owned(),
    }
}

fn validator_consensus_status(
    status: Option<&ConsensusStatus>,
    remote: Option<&Block>,
    seconds: Option<f64>,
) -> String {
    let Some(status) = status else {
        return "Consensus   unavailable".to_owned();
    };
    let mut value = format!(
        "Consensus   {} | peers {} | threshold shares {}",
        status.mode,
        status.connected_peers,
        yes_no(status.has_threshold_shares)
    );
    if status.mode != "signer" && status.next_planned_activation_height > 0 {
        let finalized = remote.map_or(status.last_finalized_block, |block| block.number);
        let remaining = status
            .next_planned_activation_height
            .saturating_sub(finalized);
        value.push_str(&format!(
            "\nDKG         activation {} | {remaining} blocks remaining{}",
            status.next_planned_activation_height,
            eta_suffix(remaining, seconds)
        ));
    }
    value
}

fn validator_services_status() -> String {
    [
        ("enclave", ENCLAVE_SERVICE),
        ("radicle", RADICLE_SERVICE),
        ("feeder", FEEDER_SERVICE),
        ("renew", RENEW_TIMER),
    ]
    .into_iter()
    .map(|(name, unit)| {
        let state = if process::systemctl_is_active(unit) {
            "active"
        } else {
            "inactive"
        };
        format!("{name} {state}")
    })
    .collect::<Vec<_>>()
    .join(" | ")
}

async fn estimate_block_seconds(remote: &RpcClient, head: &Block) -> Option<f64> {
    let window = head.number.min(120);
    if window == 0 {
        return None;
    }
    let previous = remote.block_number(head.number - window).await.ok()?;
    let elapsed = head.timestamp.checked_sub(previous.timestamp)?;
    (elapsed > 0).then_some(elapsed as f64 / window as f64)
}

fn eta_suffix(blocks: u64, block_seconds: Option<f64>) -> String {
    let Some(block_seconds) = block_seconds else {
        return String::new();
    };
    let seconds = (blocks as f64 * block_seconds).round() as u64;
    format!(", ~{}", human_duration(seconds))
}

fn eta_field(blocks: u64, block_seconds: Option<f64>) -> String {
    let Some(block_seconds) = block_seconds else {
        return String::new();
    };
    let seconds = (blocks as f64 * block_seconds).round() as u64;
    format!(" eta=~{}", human_duration(seconds))
}

fn human_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds.div_ceil(60))
    } else {
        let hours = seconds / 3_600;
        let minutes = (seconds % 3_600).div_ceil(60);
        if minutes == 0 || minutes == 60 {
            format!("{}h", hours + u64::from(minutes == 60))
        } else {
            format!("{hours}h {minutes}m")
        }
    }
}

fn validator_lifecycle(record: &ValidatorRecord) -> String {
    match record.status {
        1 => "pending".to_owned(),
        2 => "active".to_owned(),
        3 => "exiting".to_owned(),
        4 => "unbonding".to_owned(),
        5 => "inactive".to_owned(),
        6 => "jailed".to_owned(),
        status => format!("status-{status}"),
    }
}

const fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
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
    println!("Fund this address before `outbe validator promote`:");
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
