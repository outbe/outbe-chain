use super::*;
use alloy_eips::BlockNumHash;
use outbe_operator::tee::UpgradeContextV1;

pub(super) fn recover_finalized(
    io: &impl UpgradePromotionIo,
    context: &UpgradeContextV1,
    checkpoint: BlockNumHash,
    promoted: &tokio::sync::Notify,
) -> CycleOutcome {
    let BlockNumHash {
        number: finalized_height,
        hash: finalized_hash,
    } = checkpoint;
    let Some((committed, committed_hash)) = committed_manifest_and_hash(io) else {
        return CycleOutcome::Stop;
    };
    if committed_hash == context.candidate_manifest_hash {
        if let Err(error) = io.record_promoted() {
            tracing::error!(error = %format!("{error:#}"), "record recovered upgrade promotion failed");
            return CycleOutcome::Stop;
        }
        info!(finalized_height, %finalized_hash, "reconciled already-promoted enclave candidate");
        return CycleOutcome::Stop;
    }
    match io.replacement_authorization(&committed.node_id) {
        Ok(authorized) => {
            if !promote_and_record(
                io,
                &authorized.authorization,
                "record finalized enclave promotion failed",
            ) {
                return CycleOutcome::Stop;
            }
            info!(finalized_height, %finalized_hash, "finalized enclave candidate promoted; execution restart required");
            promoted.notify_one();
            CycleOutcome::Stop
        }
        Err(error) => {
            tracing::error!(error = %error, "reconstruct finalized candidate promotion authority failed");
            CycleOutcome::Stop
        }
    }
}

fn promote_and_record<I: UpgradePromotionIo>(
    io: &I,
    authorization: &I::Authorization,
    checkpoint_error: &'static str,
) -> bool {
    if let Err(error) = io.promote_candidate(authorization) {
        tracing::error!(error = %error, "promote finalized enclave candidate failed");
        return false;
    }
    if let Err(error) = io.record_promoted() {
        tracing::error!(error = %format!("{error:#}"), "{checkpoint_error}");
        return false;
    }
    true
}

pub(super) fn reconcile_submitted(
    io: &impl UpgradePromotionIo,
    promoted: &tokio::sync::Notify,
) -> Option<CycleOutcome> {
    let committed = match io.committed_manifest() {
        Ok(committed) => committed,
        Err(error) => {
            tracing::error!(error = %error, "load active manifest for finalized upgrade check failed");
            return Some(CycleOutcome::Stop);
        }
    };
    match io.replacement_authorization(&committed.node_id) {
        Ok(authorized) => {
            // A matching finalized B proves that transition execution
            // happened before the Registry cutoff, even when finality
            // advanced across H in one step.
            if let Err(error) =
                io.record_finalized(authorized.view.block_number, authorized.view.block_hash)
            {
                tracing::error!(error = %format!("{error:#}"), "record finalized enclave transition failed");
                return Some(CycleOutcome::Stop);
            }
            if !promote_and_record(
                io,
                &authorized.authorization,
                "record enclave candidate promotion failed",
            ) {
                return Some(CycleOutcome::Stop);
            }
            info!(
                finalized_height = authorized.view.block_number,
                finalized_hash = %authorized.view.block_hash,
                "finalized enclave candidate promoted; execution restart required"
            );
            promoted.notify_one();
            Some(CycleOutcome::Stop)
        }
        Err(
            outbe_node::tee_remote_session::LocalRegistryAdmissionError::ReplacementBindingMissing,
        ) => None,
        Err(error) => {
            tracing::error!(error = %error, "finalized enclave transition authorization failed");
            Some(CycleOutcome::Stop)
        }
    }
}

fn committed_manifest_and_hash(
    io: &impl UpgradePromotionIo,
) -> Option<(
    outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
    alloy_primitives::B256,
)> {
    let committed = match io.committed_manifest() {
        Ok(committed) => committed,
        Err(error) => {
            tracing::error!(error = %error, "load committed manifest during upgrade recovery failed");
            return None;
        }
    };
    let committed_hash = match committed.authorization_hash() {
        Ok(hash) => hash,
        Err(error) => {
            tracing::error!(error = %error, "hash committed manifest during upgrade recovery failed");
            return None;
        }
    };
    Some((committed, committed_hash))
}

pub(super) fn resume_checkpoint(
    io: &impl UpgradePromotionIo,
    snapshot: outbe_operator::tee::UpgradeJournalSnapshotV1,
    config: &UpgradePromotionWorkerConfigV1,
) -> CycleOutcome {
    match &snapshot.lifecycle {
        UpgradeJournalStateV1::Promoted { .. } => CycleOutcome::Wait,
        UpgradeJournalStateV1::TerminalMissedCutoff {
            finalized_height,
            activation_height,
            ..
        } => {
            tracing::error!(
                finalized_height,
                activation_height,
                "enclave upgrade is terminal after missing successor activation cutoff"
            );
            CycleOutcome::Stop
        }
        UpgradeJournalStateV1::Finalized {
            context,
            finalized_height,
            finalized_hash,
            ..
        } => recover_finalized(
            io,
            context,
            BlockNumHash::new(*finalized_height, *finalized_hash),
            &config.promoted,
        ),
        _ => monitor::observe_pending(io, &snapshot, config),
    }
}
