use super::*;
use outbe_node::tee_remote_session::LocalFinalizedSuccessorStatusV1;
use outbe_operator::tee::UpgradeContextV1;

pub(super) fn policy_matches(
    context: &UpgradeContextV1,
    status: &LocalFinalizedSuccessorStatusV1,
) -> bool {
    if let Some(policy) = &status.staged_policy {
        let policy_hash = match policy.policy_hash() {
            Ok(hash) => hash,
            Err(error) => {
                tracing::error!(error = %error, "hash node-local staged successor policy failed");
                return false;
            }
        };
        if policy_hash != context.successor_policy_hash
            || policy.activation_height != context.activation_height
        {
            tracing::error!(
                "journaled enclave upgrade no longer matches the finalized staged successor"
            );
            return false;
        }
    }
    true
}

pub(super) fn check_deadline(
    io: &impl UpgradePromotionIo,
    context: &UpgradeContextV1,
    status: &LocalFinalizedSuccessorStatusV1,
    config: &UpgradePromotionWorkerConfigV1,
) -> CycleOutcome {
    let UpgradePromotionWorkerConfigV1 {
        warning_blocks,
        critical_blocks,
        ..
    } = *config;
    if status.view.block_number >= context.activation_height {
        if !status.strict_upgrade.proposal_id.is_zero()
            && status.strict_upgrade.successor_policy_hash == context.successor_policy_hash
        {
            tracing::warn!(activation_height = context.activation_height,
                    "enclave upgrade deadline missed; owner may still complete upgrade-submit and unjail");
            return CycleOutcome::Wait;
        }
        match io.record_missed_cutoff(status.view.block_number, context.activation_height) {
            Ok(_) => tracing::error!(
                finalized_height = status.view.block_number,
                activation_height = context.activation_height,
                "enclave upgrade missed its finalized successor activation cutoff"
            ),
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "record missed enclave-upgrade cutoff failed")
            }
        }
        return CycleOutcome::Stop;
    }
    let remaining = context
        .activation_height
        .saturating_sub(status.view.block_number);
    if remaining <= critical_blocks {
        tracing::error!(
            finalized_height = status.view.block_number,
            activation_height = context.activation_height,
            remaining_blocks = remaining,
            "enclave upgrade is inside its critical finalized activation margin"
        );
    } else if remaining <= warning_blocks {
        tracing::warn!(
            finalized_height = status.view.block_number,
            activation_height = context.activation_height,
            remaining_blocks = remaining,
            "enclave upgrade is inside its warning finalized activation margin"
        );
    }
    CycleOutcome::Wait
}

pub(super) fn observe_pending(
    io: &impl UpgradePromotionIo,
    snapshot: &outbe_operator::tee::UpgradeJournalSnapshotV1,
    config: &UpgradePromotionWorkerConfigV1,
) -> CycleOutcome {
    let context = snapshot.lifecycle.context();
    let status = match io.successor_status() {
        Ok(status) => status,
        Err(error) => {
            tracing::error!(error = %error, "read node-local finalized successor status failed");
            return CycleOutcome::Wait;
        }
    };
    if !policy_matches(context, &status) {
        return CycleOutcome::Stop;
    }
    if matches!(snapshot.lifecycle, UpgradeJournalStateV1::Submitted { .. }) {
        let outcome = promotion::reconcile_submitted(io, &config.promoted);
        if let Some(outcome) = outcome {
            return outcome;
        }
    }
    check_deadline(io, context, &status, config)
}
