use std::path::Path;

use eyre::{ensure, Result};

use crate::{
    support::{process, progress::Progress},
    validator::{
        journal::Journal,
        path_text,
        rpc::RpcClient,
        validator_address,
        wait::{wait_for_rpc_chain, wait_for_sync, wait_for_verifier, wait_transaction_finalized},
        VALIDATOR_SERVICE,
    },
};

const ORACLE_DELEGATE_ROLE: u8 = 1;

pub async fn run() -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let mut journal = Journal::load()?;
    let mut progress = Progress::new();
    let local = RpcClient::new(journal.config.network.local_rpc_url())?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    let keys = Path::new(process::ROOT).join("keys");
    let validator = validator_address(&keys.join("evm-key.hex"))?;
    let feeder = validator_address(&keys.join("feeder-key.hex"))?;

    progress.stage("checking synchronized resident validator and validator state");
    ensure!(
        process::systemctl_is_active(VALIDATOR_SERVICE),
        "run `outbe-up validator sync` before promotion"
    );
    wait_for_rpc_chain(&local, journal.config.network.chain_id, &mut progress).await?;
    wait_for_sync(&local, &remote, &mut progress).await?;
    crate::validator::wait::wait_for_resident(&local, &mut progress).await?;
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

    ensure_feeder_delegate(&mut journal, &remote, validator, feeder, &mut progress).await?;

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

    progress.done(&format!(
        "validator {validator:#x} is promoted; DKG activation continues in the background"
    ));
    Ok(())
}

async fn ensure_feeder_delegate(
    journal: &mut Journal,
    remote: &RpcClient,
    validator: alloy_primitives::Address,
    feeder: alloy_primitives::Address,
    progress: &mut Progress,
) -> Result<()> {
    if remote
        .validator_delegate(validator, ORACLE_DELEGATE_ROLE)
        .await?
        == feeder
    {
        return Ok(());
    }
    progress.stage("delegating oracle votes to the feeder signer");
    let hash = if let Some(hash) = journal.feeder_delegate_transaction.clone() {
        hash
    } else {
        let feeder_arg = format!("{feeder:#x}");
        let output = run_outbe_cli(&["validator", "delegate", "oracle", &feeder_arg])?;
        let hash = transaction_hash(&output)?;
        journal.feeder_delegate_transaction = Some(hash.clone());
        journal.save()?;
        hash
    };
    wait_transaction_finalized(remote, &hash, progress).await?;
    ensure!(
        remote
            .validator_delegate(validator, ORACLE_DELEGATE_ROLE)
            .await?
            == feeder,
        "oracle feeder delegation is not visible after finality"
    );
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
