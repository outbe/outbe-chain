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

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CandidateFreeProgress {
    ProvisionKey,
    Finished(UpgradeSubmissionOutcomeV1),
}

pub(super) async fn advance_candidate_free<R: RenewalRpc + Sync>(
    rpc: &R,
    relay: &RelaySignerV1,
    node_data_dir: &Path,
    selector: &NodeBindingSelectorV1,
    replayed: bool,
) -> Result<CandidateFreeProgress> {
    loop {
        let snapshot =
            inspect_upgrade_journal_v1(node_data_dir)?.ok_or(UpgradeError::CandidateNotPrepared)?;
        if reset_if_expired(rpc, node_data_dir, selector, &snapshot).await? {
            continue;
        }
        match snapshot.lifecycle {
            UpgradeJournalStateV1::KeyProvisioned { .. } => {
                return Ok(CandidateFreeProgress::ProvisionKey);
            }
            UpgradeJournalStateV1::CandidateKeyReady { .. } => {
                prepare_upgrade_relay_v1(rpc, relay, node_data_dir, selector).await?;
            }
            UpgradeJournalStateV1::SubmissionPrepared { ref submission, .. } => {
                return Ok(CandidateFreeProgress::Finished(
                    submit_prepared(rpc, node_data_dir, selector, submission, replayed).await?,
                ));
            }
            UpgradeJournalStateV1::Submitted { ref submission, .. } => {
                return Ok(CandidateFreeProgress::Finished(
                    replay_submitted(rpc, node_data_dir, selector, submission).await?,
                ));
            }
            ref lifecycle => {
                return Ok(CandidateFreeProgress::Finished(terminal_outcome(
                    lifecycle,
                )?));
            }
        }
    }
}

pub(super) async fn run<R: RenewalRpc + Sync, N: UpgradeNodeSignerV1>(
    service: &mut UpgradeSubmissionService<'_, R, N>,
) -> Result<UpgradeSubmissionOutcomeV1> {
    let replayed = inspect_upgrade_journal_v1(service.node_data_dir)?.is_some();
    loop {
        match advance_candidate_free(
            service.rpc,
            service.relay,
            service.node_data_dir,
            service.selector,
            replayed,
        )
        .await?
        {
            CandidateFreeProgress::ProvisionKey => {
                preparation::prepare_candidate_key_ready_v1(service).await?;
            }
            CandidateFreeProgress::Finished(outcome) => return Ok(outcome),
        }
    }
}

async fn reset_if_expired<R: RegistryRpc + Sync>(
    rpc: &R,
    node_data_dir: &Path,
    selector: &NodeBindingSelectorV1,
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
    reset_expired_upgrade_submission_v1(rpc, node_data_dir, selector, snapshot).await
}

fn terminal_outcome(lifecycle: &UpgradeJournalStateV1) -> Result<UpgradeSubmissionOutcomeV1> {
    match lifecycle {
        UpgradeJournalStateV1::CandidatePrepared { .. } => {
            Err(UpgradeError::CandidateKeyNotProvisioned.into())
        }
        UpgradeJournalStateV1::Finalized {
            submission,
            finalized_height,
            ..
        } => Ok(UpgradeSubmissionOutcomeV1::Finalized {
            transaction_hash: last_transaction_hash(submission)?,
            finalized_height: *finalized_height,
        }),
        UpgradeJournalStateV1::Promoted {
            submission,
            finalized_height,
            ..
        } => Ok(UpgradeSubmissionOutcomeV1::Promoted {
            transaction_hash: last_transaction_hash(submission)?,
            finalized_height: *finalized_height,
        }),
        UpgradeJournalStateV1::TerminalMissedCutoff {
            finalized_height,
            activation_height,
            ..
        } => Err(UpgradeError::MissedActivationCutoff {
            activation_height: *activation_height,
            finalized_height: *finalized_height,
        }
        .into()),
        _ => Err(UpgradeError::NonTerminalSubmissionState.into()),
    }
}

pub(super) async fn submit_prepared<R: RegistryRpc + RelayRpc + Sync>(
    rpc: &R,
    node_data_dir: &Path,
    selector: &NodeBindingSelectorV1,
    submission: &PreparedUpgradeSubmissionV1,
    replayed: bool,
) -> Result<UpgradeSubmissionOutcomeV1> {
    let transaction_hash = last_transaction_hash(submission)?;
    if finalized_transition_matches_v1(rpc, selector, node_data_dir, submission).await? {
        let finalized = read_finalized_bound_renewal_view_v1(rpc, selector).await?;
        record_upgrade_submitted_v1(node_data_dir, finalized.schedule.finalized_height)?;
        return Ok(UpgradeSubmissionOutcomeV1::AlreadySubmitted { transaction_hash });
    }
    let raw = submission
        .relay_variants
        .last()
        .ok_or(UpgradeError::MissingRelayBytes)?;
    let returned_hash = send_prepared(rpc, raw).await?;
    if returned_hash != raw.transaction_hash {
        return Err(UpgradeError::SubmittedTransitionHashMismatch.into());
    }
    let finalized = read_finalized_bound_renewal_view_v1(rpc, selector).await?;
    record_upgrade_submitted_v1(node_data_dir, finalized.schedule.finalized_height)?;
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
        Err(error) if transaction_is_already_known(&error) => {
            if exact_transition_receipt_exists(rpc, raw).await? {
                Ok(raw.transaction_hash)
            } else {
                Err(error)
                    .wrap_err("transition is already known without the exact transaction receipt")
            }
        }
        Err(error) => Err(error).wrap_err("submit exact measurement-transition transaction"),
    }
}

async fn exact_transition_receipt_exists(
    rpc: &(impl RelayRpc + Sync),
    raw: &RawRelayTransactionV1,
) -> Result<bool> {
    let expected_hash = format!("{:#x}", raw.transaction_hash);
    let Some(receipt) = rpc.transaction_receipt(&expected_hash).await? else {
        return Ok(false);
    };
    exact_transition_receipt_is_success(raw.transaction_hash, &receipt)?;
    Ok(true)
}

fn exact_transition_receipt_is_success(
    expected_hash: B256,
    receipt: &serde_json::Value,
) -> Result<()> {
    let expected = format!("{expected_hash:#x}");
    let observed_hash = receipt
        .get("transactionHash")
        .and_then(serde_json::Value::as_str)
        .ok_or(UpgradeError::MissingReceiptTransactionHash)?;
    if !observed_hash.eq_ignore_ascii_case(&expected) {
        return Err(UpgradeError::ReceiptTransactionHashMismatch.into());
    }
    let status = receipt
        .get("status")
        .and_then(serde_json::Value::as_str)
        .ok_or(UpgradeError::MissingReceiptStatus)?;
    if status != "0x1" {
        return Err(UpgradeError::NonSuccessReceiptStatus(status.to_owned()).into());
    }
    Ok(())
}

pub(super) async fn replay_submitted<R: RegistryRpc + RelayRpc + Sync>(
    rpc: &R,
    node_data_dir: &Path,
    selector: &NodeBindingSelectorV1,
    submission: &PreparedUpgradeSubmissionV1,
) -> Result<UpgradeSubmissionOutcomeV1> {
    let transaction_hash = last_transaction_hash(submission)?;
    if finalized_transition_matches_v1(rpc, selector, node_data_dir, submission).await? {
        return Ok(UpgradeSubmissionOutcomeV1::AlreadySubmitted { transaction_hash });
    }
    if let Some(receipt) = rpc
        .transaction_receipt(&format!("{transaction_hash:#x}"))
        .await?
    {
        exact_transition_receipt_is_success(transaction_hash, &receipt)?;
        return Ok(UpgradeSubmissionOutcomeV1::AlreadySubmitted { transaction_hash });
    }
    let raw = submission
        .relay_variants
        .last()
        .ok_or(UpgradeError::MissingRelayBytes)?;
    replay_exact(rpc, raw, transaction_hash).await?;
    Ok(UpgradeSubmissionOutcomeV1::AlreadySubmitted { transaction_hash })
}

async fn replay_exact(
    rpc: &(impl RelayRpc + Sync),
    raw: &RawRelayTransactionV1,
    transaction_hash: B256,
) -> Result<()> {
    match rpc.send_raw_transaction(&raw.raw_transaction).await {
        Ok(hash) if hash.parse::<B256>()? == transaction_hash => Ok(()),
        Ok(_) => Err(UpgradeError::ReplayTransactionHashMismatch.into()),
        Err(error) if transaction_is_already_known(&error) => Ok(()),
        Err(error) => Err(error).wrap_err("replay exact measurement-transition transaction"),
    }
}
