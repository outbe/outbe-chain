use std::{
    path::Path,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Result};

use crate::support::{process, progress::Progress};

use super::{
    journal::Journal,
    promote::{run_outbe_cli, transaction_hash},
    rpc::{RpcClient, ValidatorRecord},
    validator_address,
    wait::{
        wait_for_resident, wait_for_rpc_chain, wait_for_sync, wait_for_verifier,
        wait_transaction_finalized,
    },
    VALIDATOR_SERVICE, VALIDATOR_TARGET,
};

const STATUS_PENDING: u8 = 1;
const STATUS_ACTIVE: u8 = 2;
const STATUS_EXITING: u8 = 3;
const STATUS_UNBONDING: u8 = 4;
const STATUS_INACTIVE: u8 = 5;
const STATUS_JAILED: u8 = 6;

pub async fn unjail() -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let mut journal = Journal::load()?;
    let mut progress = Progress::new();
    let local = RpcClient::new(journal.config.network.local_rpc_url())?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    let validator = validator_address(&Path::new(process::ROOT).join("keys/evm-key.hex"))?;

    progress.stage("checking synchronized jailed validator");
    ensure!(
        process::systemctl_is_active(VALIDATOR_SERVICE),
        "validator service must be running before unjail"
    );
    wait_for_rpc_chain(&local, journal.config.network.chain_id, &mut progress).await?;
    wait_for_sync(&local, &remote, &mut progress).await?;
    wait_for_resident(&local, &mut progress).await?;
    let minimum = remote.min_stake().await?;
    let mut record = remote.validator(validator).await?;
    ensure!(record.registered, "validator is not registered");
    ensure!(
        record.stake >= minimum,
        "validator stake is below the network minimum"
    );

    if record.status == STATUS_ACTIVE {
        clear_unjail_transactions(&mut journal)?;
        progress.done("validator is already active");
        return Ok(());
    }
    ensure!(
        matches!(record.status, STATUS_JAILED | STATUS_PENDING),
        "validator status {} cannot be unjailed",
        record.status
    );
    if record.status == STATUS_JAILED {
        progress.stage("submitting validator unjail transaction");
        let hash = if let Some(hash) = journal.unjail_transaction.clone() {
            hash
        } else {
            let output = run_outbe_cli(&["staking", "unjail"])?;
            let hash = transaction_hash(&output)?;
            journal.unjail_transaction = Some(hash.clone());
            journal.save()?;
            hash
        };
        wait_transaction_finalized(&remote, &hash, &mut progress).await?;
        record = remote.validator(validator).await?;
    }
    ensure!(
        record.status == STATUS_PENDING,
        "unjail finalized, but validator status is {}",
        record.status
    );
    wait_for_verifier(&local, &remote, &mut progress).await?;
    progress.stage("confirming unjailed validator readiness");
    let hash = if let Some(hash) = journal.unjail_confirm_ready_transaction.clone() {
        hash
    } else {
        let output = run_outbe_cli(&["validator", "confirm-ready"])?;
        let hash = transaction_hash(&output)?;
        journal.unjail_confirm_ready_transaction = Some(hash.clone());
        journal.save()?;
        hash
    };
    wait_transaction_finalized(&remote, &hash, &mut progress).await?;
    clear_unjail_transactions(&mut journal)?;
    progress.done("validator unjailed and ready for the next DKG activation");
    Ok(())
}

pub async fn exit() -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let mut journal = Journal::load()?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    let validator = validator_address(&Path::new(process::ROOT).join("keys/evm-key.hex"))?;
    let mut progress = Progress::new();
    progress.stage("checking validator exit state");
    let mut record = remote.validator(validator).await?;
    ensure!(record.registered, "validator is not registered");

    if record.status == STATUS_ACTIVE {
        progress.stage("deactivating validator");
        let hash = if let Some(hash) = journal.deactivate_transaction.clone() {
            hash
        } else {
            let output = run_outbe_cli(&["validator", "deactivate"])?;
            let hash = transaction_hash(&output)?;
            journal.deactivate_transaction = Some(hash.clone());
            journal.save()?;
            hash
        };
        wait_transaction_finalized(&remote, &hash, &mut progress).await?;
        record = wait_until_out_of_consensus(&remote, validator, &mut progress).await?;
    }

    if remote.is_consensus_participant(validator).await? {
        record = wait_until_out_of_consensus(&remote, validator, &mut progress).await?;
    }

    ensure!(
        matches!(
            record.status,
            STATUS_PENDING | STATUS_EXITING | STATUS_UNBONDING | STATUS_INACTIVE | STATUS_JAILED
        ),
        "validator status {} cannot exit",
        record.status
    );
    if !matches!(record.status, STATUS_UNBONDING | STATUS_INACTIVE) {
        progress.stage("starting stake unbonding");
        let hash = if let Some(hash) = journal.unstake_transaction.clone() {
            hash
        } else {
            let amount = record.stake.to_string();
            let output = run_outbe_cli(&["staking", "unstake", "--amount", &amount])?;
            let hash = transaction_hash(&output)?;
            journal.unstake_transaction = Some(hash.clone());
            journal.save()?;
            hash
        };
        wait_transaction_finalized(&remote, &hash, &mut progress).await?;
        record = remote.validator(validator).await?;
        ensure!(
            matches!(record.status, STATUS_UNBONDING | STATUS_INACTIVE),
            "unstake finalized, but validator status is {}",
            record.status
        );
    }

    process::clear_services_enabled()?;
    process::systemctl(&["disable", "--now", VALIDATOR_TARGET])?;
    progress.done("validator exited consensus and services are stopped; claim after unbonding");
    Ok(())
}

pub async fn claim() -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let mut journal = Journal::load()?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    let mut progress = Progress::new();
    progress.stage("claiming completed unbonding entries");
    let hash = if let Some(hash) = journal.claim_transaction.clone() {
        hash
    } else {
        let output = run_outbe_cli(&["staking", "claim"])?;
        let hash = transaction_hash(&output)?;
        journal.claim_transaction = Some(hash.clone());
        journal.save()?;
        hash
    };
    wait_transaction_finalized(&remote, &hash, &mut progress).await?;
    journal.claim_transaction = None;
    journal.save()?;
    progress.done("unbonded stake claimed");
    Ok(())
}

fn clear_unjail_transactions(journal: &mut Journal) -> Result<()> {
    let had_unjail = journal.unjail_transaction.take().is_some();
    let had_confirm_ready = journal.unjail_confirm_ready_transaction.take().is_some();
    if had_unjail || had_confirm_ready {
        journal.save()?;
    }
    Ok(())
}

pub async fn exit_status() -> Result<()> {
    process::require_root()?;
    let journal = Journal::load()?;
    let remote = RpcClient::new(journal.config.network.certified_rpc.clone())?;
    let validator = validator_address(&Path::new(process::ROOT).join("keys/evm-key.hex"))?;
    let record = remote.validator(validator).await?;
    println!("Validator   {validator:#x}");
    println!("Status      {}", status_name(&record));
    println!("Stake       {}", record.stake);
    println!(
        "Services    {}",
        if process::systemctl_is_active(VALIDATOR_TARGET) {
            "active"
        } else {
            "stopped"
        }
    );
    println!(
        "Transactions deactivate={} unstake={} claim={}",
        journal.deactivate_transaction.as_deref().unwrap_or("none"),
        journal.unstake_transaction.as_deref().unwrap_or("none"),
        journal.claim_transaction.as_deref().unwrap_or("none")
    );
    Ok(())
}

async fn wait_until_out_of_consensus(
    remote: &RpcClient,
    validator: alloy_primitives::Address,
    progress: &mut Progress,
) -> Result<ValidatorRecord> {
    let deadline = Instant::now() + Duration::from_secs(60 * 60);
    loop {
        let record = remote.validator(validator).await?;
        if record.status != STATUS_ACTIVE && !remote.is_consensus_participant(validator).await? {
            return Ok(record);
        }
        if Instant::now() >= deadline {
            bail!("validator remained in the active consensus set for one hour after deactivation");
        }
        progress.update("waiting for validator exclusion from the active consensus set");
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

fn status_name(record: &ValidatorRecord) -> &'static str {
    match record.status {
        STATUS_PENDING => "pending",
        STATUS_ACTIVE => "active",
        STATUS_EXITING => "exiting",
        STATUS_UNBONDING => "unbonding",
        STATUS_INACTIVE => "inactive",
        STATUS_JAILED => "jailed",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;

    use super::*;

    #[test]
    fn lifecycle_status_names_are_stable() {
        let mut record = ValidatorRecord {
            registered: true,
            status: STATUS_ACTIVE,
            stake: U256::from(1),
            has_bls_share: true,
        };
        assert_eq!(status_name(&record), "active");
        record.status = STATUS_UNBONDING;
        assert_eq!(status_name(&record), "unbonding");
    }
}
