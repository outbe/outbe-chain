use std::path::Path;

use eyre::{ensure, Result};
use rand::RngCore;

use crate::{
    support::{process, progress::Progress},
    validator::{
        journal::Journal,
        path_text,
        rpc::RpcClient,
        validator_address,
        wait::{
            wait_for_rpc_chain, wait_for_signer, wait_for_sync, wait_for_verifier,
            wait_transaction_finalized,
        },
        ENCLAVE_SERVICE, FULL_NODE, LOCAL_RPC, RENEW_TIMER, VALIDATOR_SERVICE, VALIDATOR_TARGET,
    },
};

pub async fn run() -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let mut journal = Journal::load()?;
    let mut progress = Progress::new();
    let local = RpcClient::new(LOCAL_RPC)?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    let validator = validator_address(&Path::new(process::ROOT).join("keys/evm-key.hex"))?;

    progress.stage("checking synchronized full node and validator state");
    ensure!(
        process::systemctl_is_active(FULL_NODE) || process::systemctl_is_active(VALIDATOR_SERVICE),
        "run `outbe-up validator sync` before promotion"
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
    let mut full = vec![
        "--user",
        "outbe",
        "--",
        path_text(&binary)?,
        "--rpc-url",
        journal.config.network.certified_rpc.as_str(),
        "--private-key-file",
        "/opt/outbe-chain/keys/evm-key.hex",
    ];
    full.extend_from_slice(args);
    process::capture("runuser", &full)
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
