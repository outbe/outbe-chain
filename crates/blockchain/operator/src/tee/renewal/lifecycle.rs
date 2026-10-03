//! Private renewal lifecycle stage.
use super::*;
use super::{identity::*, preparation::*, submission::*};

pub(super) async fn run_locked_renewal<
    R: RenewalRpc + Sync,
    E: RenewalEnclaveV1,
    N: RenewalNodeSignerV1,
>(
    preparation: &mut RenewalPreparation<'_, R, E, N>,
) -> Result<RenewalOutcomeV1> {
    let rpc = preparation.rpc;
    let config = preparation.config;
    // Both guards remain alive through journal replay, preparation and submission.
    let upgrade_guard = super::super::UpgradeJournalGuardV1::acquire(&config.node_data_dir)?;
    let promoted_policy = promoted_policy(&upgrade_guard, config)?;
    let _upgrade_guard = upgrade_guard;
    let journal = RenewalJournalGuard::acquire(&config.node_data_dir)?;
    let view = read_finalized_bound_renewal_view_v1(rpc, &config.selector).await?;
    validate_identity(config, &view)?;
    let snapshot = journal.load()?;
    let predecessor_replaced = snapshot
        .as_ref()
        .is_some_and(|snapshot| finished_predecessor_replaced(snapshot, &view, promoted_policy));
    if let Some(snapshot) = snapshot.filter(|_| !predecessor_replaced) {
        if let Some(outcome) = replay_journal(rpc, &journal, snapshot, &view).await? {
            return Ok(outcome);
        }
    }
    if let Some(outcome) = not_due(&view)? {
        return Ok(outcome);
    }
    let attempt = prepare_attempt(preparation, &view).await?;
    journal.store(RenewalJournalSnapshotV1::new(
        RenewalJournalStateV1::Prepared {
            attempt: attempt.clone(),
        },
    ))?;
    submit_attempt(
        rpc,
        &journal,
        RenewalSubmission {
            attempt,
            finalized_height: view.schedule.finalized_height,
            replayed: false,
        },
    )
    .await
}

pub(super) fn promoted_policy(
    guard: &super::super::UpgradeJournalGuardV1,
    config: &RenewalServiceConfigV1,
) -> Result<Option<B256>> {
    let mut promoted_policy = None;
    if let Some(upgrade) = guard.load()? {
        if let super::super::UpgradeJournalStateV1::Promoted { context, .. } = &upgrade.lifecycle {
            // Promotion installs B's committed manifest. The completed journal
            // remains as recovery evidence; it must not freeze B's renewals.
            // A caller still holding A's manifest may not use this exception.
            if config
                .manifest
                .authorization_hash()
                .map_err(|e| eyre::eyre!(e))?
                != context.candidate_manifest_hash
            {
                eyre::bail!("renewal manifest does not match the promoted enclave");
            }
            promoted_policy = Some(context.successor_policy_hash);
        }
        if matches!(
            upgrade.lifecycle,
            super::super::UpgradeJournalStateV1::KeyProvisioned { .. }
                | super::super::UpgradeJournalStateV1::CandidateKeyReady { .. }
                | super::super::UpgradeJournalStateV1::SubmissionPrepared { .. }
                | super::super::UpgradeJournalStateV1::Submitted { .. }
                | super::super::UpgradeJournalStateV1::Finalized { .. }
                | super::super::UpgradeJournalStateV1::TerminalMissedCutoff { .. }
        ) {
            eyre::bail!(
                "renewal is blocked while an enclave upgrade is journaled to preserve exact transition counters"
            );
        }
    }
    Ok(promoted_policy)
}

pub(super) fn finished_predecessor_replaced(
    snapshot: &RenewalJournalSnapshotV1,
    view: &FinalizedRenewalChainViewV1,
    promoted_policy: Option<B256>,
) -> bool {
    let previous = match &snapshot.lifecycle {
        RenewalJournalStateV1::Finalized {
            finalized_binding, ..
        } => finalized_binding,
        RenewalJournalStateV1::Abandoned { attempt, .. } => &attempt.source,
        _ => return false,
    };
    promoted_policy == Some(view.binding.policy_hash)
        && previous.node_id_hash == view.binding.node_id_hash
        && previous.enclave_id != view.binding.enclave_id
        && replacement_counters_match(previous, &view.binding)
}

pub(super) fn replacement_counters_match(
    previous: &RenewalBindingV1,
    current: &RenewalBindingV1,
) -> bool {
    previous.binding_version.checked_add(1) == Some(current.binding_version)
        && previous.transition_nonce.checked_add(1) == Some(current.transition_nonce)
}

pub(super) async fn replay_journal(
    rpc: &(impl RenewalRpc + Sync),
    journal: &RenewalJournalGuard,
    snapshot: RenewalJournalSnapshotV1,
    view: &FinalizedRenewalChainViewV1,
) -> Result<Option<RenewalOutcomeV1>> {
    match snapshot.lifecycle {
        RenewalJournalStateV1::Prepared { attempt }
        | RenewalJournalStateV1::Submitted { attempt, .. } => {
            replay_pending(rpc, journal, attempt, view).await.map(Some)
        }
        RenewalJournalStateV1::Finalized {
            attempt,
            finalized_binding,
            ..
        } => {
            if view.binding != finalized_binding && !target_matches(&view.binding, &attempt)? {
                eyre::bail!("finalized Registry binding diverged from the renewal journal");
            }
            not_due(view)
        }
        RenewalJournalStateV1::Abandoned { attempt, .. } => {
            ensure_source_or_conflict(&view.binding, &attempt.source)?;
            Ok(None)
        }
    }
}

pub(super) async fn replay_pending(
    rpc: &(impl RenewalRpc + Sync),
    journal: &RenewalJournalGuard,
    attempt: PreparedRenewalV1,
    view: &FinalizedRenewalChainViewV1,
) -> Result<RenewalOutcomeV1> {
    if target_matches(&view.binding, &attempt)? {
        return finalize(journal, attempt, view);
    }
    ensure_source_or_conflict(&view.binding, &attempt.source)?;
    if let Some(reason) = permanent_staleness(&attempt, view.schedule.finalized_timestamp) {
        journal.store(RenewalJournalSnapshotV1::new(
            RenewalJournalStateV1::Abandoned {
                attempt,
                abandoned_at_finalized_height: view.schedule.finalized_height,
                reason: reason.clone(),
            },
        ))?;
        return Ok(RenewalOutcomeV1::Abandoned {
            finalized_height: view.schedule.finalized_height,
            reason,
        });
    }
    submit_attempt(
        rpc,
        journal,
        RenewalSubmission {
            attempt,
            finalized_height: view.schedule.finalized_height,
            replayed: true,
        },
    )
    .await
}

pub(super) fn not_due(view: &FinalizedRenewalChainViewV1) -> Result<Option<RenewalOutcomeV1>> {
    if !renewal_is_open(
        &view.binding,
        view.schedule.finalized_timestamp,
        view.policy.maximum_lease,
    )? {
        return Ok(Some(RenewalOutcomeV1::NotDue {
            finalized_height: view.schedule.finalized_height,
            opens_at_timestamp: renewal_opens_at(&view.binding, view.policy.maximum_lease)?,
        }));
    }
    Ok(None)
}

pub(super) fn finalize(
    journal: &RenewalJournalGuard,
    attempt: PreparedRenewalV1,
    view: &FinalizedRenewalChainViewV1,
) -> Result<RenewalOutcomeV1> {
    journal.store(RenewalJournalSnapshotV1::new(
        RenewalJournalStateV1::Finalized {
            attempt: Box::new(attempt),
            finalized_binding: view.binding.clone(),
            finalized_height: view.schedule.finalized_height,
            finalized_hash: view.schedule.finalized_hash,
        },
    ))?;
    Ok(RenewalOutcomeV1::Finalized {
        finalized_height: view.schedule.finalized_height,
        valid_until: view.binding.valid_until,
    })
}

pub(super) fn permanent_staleness(
    attempt: &PreparedRenewalV1,
    finalized_timestamp: u64,
) -> Option<String> {
    if finalized_timestamp >= attempt.collateral_valid_until {
        return Some("finalized consensus time reached the signed collateral expiration".into());
    }
    if finalized_timestamp >= attempt.requested_valid_until {
        return Some("finalized consensus time reached the requested lease expiration".into());
    }
    None
}

pub(super) fn renewal_opens_at(binding: &RenewalBindingV1, lease_period: u64) -> Result<u64> {
    if lease_period == 0 || !lease_period.is_multiple_of(2) {
        eyre::bail!("finalized Registry lease period is not a positive even duration");
    }
    Ok(binding.valid_until.saturating_sub(lease_period / 2))
}

pub(super) fn renewal_is_open(
    binding: &RenewalBindingV1,
    finalized_timestamp: u64,
    lease_period: u64,
) -> Result<bool> {
    if finalized_timestamp >= binding.valid_until {
        eyre::bail!("finalized enclave lease expired; run tee join to recover");
    }
    Ok(finalized_timestamp >= renewal_opens_at(binding, lease_period)?)
}

pub(super) fn next_renewal_deadline(binding: &RenewalBindingV1, lease_period: u64) -> Option<u64> {
    binding.valid_until.checked_add(lease_period)
}
