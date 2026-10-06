//! Saved upgrade submission, replay and terminal checkpoint outcomes.
use super::*;

pub(super) struct UpgradeSubmissionService<'a, R, N> {
    pub(super) rpc: &'a R,
    pub(super) relay: &'a RelaySignerV1,
    pub(super) candidate: &'a mut ReplacementCandidateEnclaveV1,
    pub(super) node_signer: &'a N,
    pub(super) node_data_dir: &'a Path,
    pub(super) selector: &'a NodeBindingSelectorV1,
    pub(super) binding_id: B256,
    pub(super) requested_valid_until: u64,
}

pub(super) async fn run<R: RenewalRpc + Sync, N: UpgradeNodeSignerV1>(
    service: &mut UpgradeSubmissionService<'_, R, N>,
) -> Result<UpgradeSubmissionOutcomeV1> {
    let replayed = inspect_upgrade_journal_v1(service.node_data_dir)?.is_some();
    loop {
        let snapshot = inspect_upgrade_journal_v1(service.node_data_dir)?
            .ok_or_else(|| eyre::eyre!("upgrade candidate is not prepared"))?;
        if reset_if_expired(service, &snapshot).await? {
            continue;
        }
        match snapshot.lifecycle {
            UpgradeJournalStateV1::KeyProvisioned { .. } => {
                preparation::prepare_candidate_key_ready_v1(service).await?;
            }
            UpgradeJournalStateV1::CandidateKeyReady { .. } => {
                prepare_upgrade_relay_v1(
                    service.rpc,
                    service.relay,
                    service.node_data_dir,
                    service.selector,
                )
                .await?;
            }
            UpgradeJournalStateV1::SubmissionPrepared { ref submission, .. } => {
                return submit_prepared(service, submission, replayed).await;
            }
            UpgradeJournalStateV1::Submitted { ref submission, .. } => {
                return replay_submitted(service, submission).await;
            }
            ref lifecycle => return terminal_outcome(lifecycle),
        }
    }
}

async fn reset_if_expired<R: RegistryRpc + Sync, N>(
    service: &UpgradeSubmissionService<'_, R, N>,
    snapshot: &UpgradeJournalSnapshotV1,
) -> Result<bool> {
    if !matches!(
        snapshot.lifecycle,
        UpgradeJournalStateV1::CandidateKeyReady { .. }
            | UpgradeJournalStateV1::SubmissionPrepared { .. }
            | UpgradeJournalStateV1::Submitted { .. }
    ) {
        return Ok(false);
    }
    reset_expired_upgrade_submission_v1(
        service.rpc,
        service.node_data_dir,
        service.selector,
        snapshot,
    )
    .await
}

fn terminal_outcome(lifecycle: &UpgradeJournalStateV1) -> Result<UpgradeSubmissionOutcomeV1> {
    match lifecycle {
        UpgradeJournalStateV1::CandidatePrepared { .. } => eyre::bail!("candidate key is not provisioned; run upgrade-provision"),
        UpgradeJournalStateV1::Finalized { submission, finalized_height, .. } =>
            Ok(UpgradeSubmissionOutcomeV1::Finalized { transaction_hash: last_transaction_hash(submission)?, finalized_height: *finalized_height }),
        UpgradeJournalStateV1::Promoted { submission, finalized_height, .. } =>
            Ok(UpgradeSubmissionOutcomeV1::Promoted { transaction_hash: last_transaction_hash(submission)?, finalized_height: *finalized_height }),
        UpgradeJournalStateV1::TerminalMissedCutoff { finalized_height, activation_height, .. } =>
            eyre::bail!("upgrade missed successor activation cutoff {activation_height} at finalized height {finalized_height}"),
        _ => eyre::bail!("upgrade checkpoint is not a terminal submission state"),
    }
}

async fn submit_prepared<R: RegistryRpc + RelayRpc + Sync, N>(
    service: &UpgradeSubmissionService<'_, R, N>,
    submission: &PreparedUpgradeSubmissionV1,
    replayed: bool,
) -> Result<UpgradeSubmissionOutcomeV1> {
    let transaction_hash = last_transaction_hash(submission)?;
    if finalized_transition_matches_v1(
        service.rpc,
        service.selector,
        service.node_data_dir,
        submission,
    )
    .await?
    {
        let finalized = read_finalized_bound_renewal_view_v1(service.rpc, service.selector).await?;
        record_upgrade_submitted_v1(service.node_data_dir, finalized.schedule.finalized_height)?;
        return Ok(UpgradeSubmissionOutcomeV1::AlreadySubmitted { transaction_hash });
    }
    let raw = submission
        .relay_variants
        .last()
        .ok_or_else(|| eyre::eyre!("upgrade submission has no relay bytes"))?;
    let returned_hash = send_prepared(service.rpc, raw).await?;
    if returned_hash != raw.transaction_hash {
        eyre::bail!("RPC returned a transaction hash different from the signed transition bytes");
    }
    let finalized = read_finalized_bound_renewal_view_v1(service.rpc, service.selector).await?;
    record_upgrade_submitted_v1(service.node_data_dir, finalized.schedule.finalized_height)?;
    Ok(UpgradeSubmissionOutcomeV1::Submitted {
        transaction_hash: returned_hash,
        replayed,
    })
}

async fn send_prepared(rpc: &(impl RelayRpc + Sync), raw: &RawRelayTransactionV1) -> Result<B256> {
    match rpc.send_raw_transaction(&raw.raw_transaction).await {
        Ok(returned) => returned
            .parse::<B256>()
            .wrap_err("parse transition transaction hash"),
        Err(error) if transaction_is_already_known(&error) => Ok(raw.transaction_hash),
        Err(error) => Err(error).wrap_err("submit exact measurement-transition transaction"),
    }
}

async fn replay_submitted<R: RegistryRpc + RelayRpc + Sync, N>(
    service: &UpgradeSubmissionService<'_, R, N>,
    submission: &PreparedUpgradeSubmissionV1,
) -> Result<UpgradeSubmissionOutcomeV1> {
    let transaction_hash = last_transaction_hash(submission)?;
    if !finalized_transition_matches_v1(
        service.rpc,
        service.selector,
        service.node_data_dir,
        submission,
    )
    .await?
        && service
            .rpc
            .transaction_receipt(&format!("{transaction_hash:#x}"))
            .await?
            .is_none()
    {
        let raw = submission
            .relay_variants
            .last()
            .ok_or_else(|| eyre::eyre!("upgrade submission has no relay bytes"))?;
        replay_exact(service.rpc, raw, transaction_hash).await?;
    }
    Ok(UpgradeSubmissionOutcomeV1::AlreadySubmitted { transaction_hash })
}

async fn replay_exact(
    rpc: &(impl RelayRpc + Sync),
    raw: &RawRelayTransactionV1,
    transaction_hash: B256,
) -> Result<()> {
    match rpc.send_raw_transaction(&raw.raw_transaction).await {
        Ok(hash) if hash.parse::<B256>()? == transaction_hash => Ok(()),
        Ok(_) => eyre::bail!("RPC returned a different replay transaction hash"),
        Err(error) if transaction_is_already_known(&error) => Ok(()),
        Err(error) => Err(error).wrap_err("replay exact measurement-transition transaction"),
    }
}
