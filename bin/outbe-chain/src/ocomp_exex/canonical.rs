use super::LocalVoteEligibilityV1;
use super::RequestLocatorV1;

use eyre::bail;

use outbe_ocomp::embedded::EmbeddedJobGenerationV1;
use outbe_ocomp::embedded::EmbeddedJobStateV1;

use outbe_ocomp::embedded::EmbeddedTerminalReasonV1;

use outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1;

use outbe_ocomp_protocol::state::OcompJobStatus;

pub(super) fn advance_vote_eligibility(
    current: LocalVoteEligibilityV1,
    observed: outbe_node::ocomp::retention::OcompSnapshotEligibilityV1,
) -> eyre::Result<LocalVoteEligibilityV1> {
    if current != LocalVoteEligibilityV1::Pending {
        return Ok(current);
    }
    match observed {
        outbe_node::ocomp::retention::OcompSnapshotEligibilityV1::Eligible => {
            Ok(LocalVoteEligibilityV1::Eligible)
        }
        outbe_node::ocomp::retention::OcompSnapshotEligibilityV1::NotMember => {
            Ok(LocalVoteEligibilityV1::NotMember)
        }
        outbe_node::ocomp::retention::OcompSnapshotEligibilityV1::Unavailable { .. } => {
            Ok(LocalVoteEligibilityV1::Pending)
        }
        outbe_node::ocomp::retention::OcompSnapshotEligibilityV1::Corrupt { detail } => {
            bail!("pinned OCOMP vote membership is corrupt: {detail}");
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CanonicalJobDispositionV1 {
    AwaitingFinality,
    FinalizedAwaitingOpen,
    VotingOpen,
    Completed,
    Closed {
        reason: EmbeddedTerminalReasonV1,
        has_finalized_job: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AsyncOutcomeProjectionV1 {
    Active,
    CheckpointPruned,
}

pub(super) fn classify_async_outcome_projection(
    runtime_generation: Option<EmbeddedJobGenerationV1>,
    reducer_generation: Option<EmbeddedJobGenerationV1>,
) -> eyre::Result<AsyncOutcomeProjectionV1> {
    match (runtime_generation, reducer_generation) {
        (Some(runtime), Some(reducer)) if runtime == reducer => {
            Ok(AsyncOutcomeProjectionV1::Active)
        }
        (None, None) => Ok(AsyncOutcomeProjectionV1::CheckpointPruned),
        (Some(_), Some(_)) => {
            bail!("OCOMP runtime and reducer generations disagree");
        }
        (Some(_), None) | (None, Some(_)) => {
            bail!("OCOMP runtime and reducer projections disagree");
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LocalResultRestorePolicyV1 {
    Never,
    BeforeCompute,
    AfterCanonicalCompleted,
}

pub(super) fn local_result_restore_policy(
    disposition: CanonicalJobDispositionV1,
    policy: EmbeddedNodePolicyV1,
    discovered: bool,
    eligibility_became_available: bool,
    compute_started: bool,
) -> LocalResultRestorePolicyV1 {
    match disposition {
        CanonicalJobDispositionV1::VotingOpen
            if discovered || eligibility_became_available || !compute_started =>
        {
            LocalResultRestorePolicyV1::BeforeCompute
        }
        CanonicalJobDispositionV1::Completed
            if policy == EmbeddedNodePolicyV1::FullNode && (discovered || !compute_started) =>
        {
            LocalResultRestorePolicyV1::AfterCanonicalCompleted
        }
        CanonicalJobDispositionV1::AwaitingFinality
        | CanonicalJobDispositionV1::FinalizedAwaitingOpen
        | CanonicalJobDispositionV1::VotingOpen
        | CanonicalJobDispositionV1::Completed
        | CanonicalJobDispositionV1::Closed { .. } => LocalResultRestorePolicyV1::Never,
    }
}

pub(super) fn classify_canonical_job(
    status: OcompJobStatus,
    has_finalized_job: bool,
) -> eyre::Result<CanonicalJobDispositionV1> {
    Ok(match status {
        OcompJobStatus::AwaitingFinality if !has_finalized_job => {
            CanonicalJobDispositionV1::AwaitingFinality
        }
        OcompJobStatus::AwaitingFinality => CanonicalJobDispositionV1::FinalizedAwaitingOpen,
        OcompJobStatus::VotingOpen if has_finalized_job => CanonicalJobDispositionV1::VotingOpen,
        OcompJobStatus::Completed if has_finalized_job => CanonicalJobDispositionV1::Completed,
        OcompJobStatus::Expired => CanonicalJobDispositionV1::Closed {
            reason: EmbeddedTerminalReasonV1::Expired,
            has_finalized_job,
        },
        OcompJobStatus::Failed => CanonicalJobDispositionV1::Closed {
            reason: EmbeddedTerminalReasonV1::Failed,
            has_finalized_job,
        },
        OcompJobStatus::VotingOpen | OcompJobStatus::Completed => {
            bail!("OCOMP canonical status/finalized payload shape is invalid");
        }
    })
}

pub(super) fn same_locator(left: RequestLocatorV1, right: RequestLocatorV1) -> bool {
    left.intent_id == right.intent_id
        && left.wwd == right.wwd
        && left.pending_nonce == right.pending_nonce
        && left.attempt == right.attempt
        && left.activation_preconditions_hash == right.activation_preconditions_hash
        && left.block_number == right.block_number
        && left.block_hash == right.block_hash
        && left.state_root == right.state_root
        && left.before_request == right.before_request
}

pub(super) fn request_projection_is_closed(
    export_acknowledged: bool,
    state: Option<EmbeddedJobStateV1>,
    terminal_reason: Option<EmbeddedTerminalReasonV1>,
) -> bool {
    if terminal_reason == Some(EmbeddedTerminalReasonV1::Expired) {
        return true;
    }
    export_acknowledged
        && !matches!(
            state,
            Some(
                EmbeddedJobStateV1::Computing
                    | EmbeddedJobStateV1::WaitAtDeadline
                    | EmbeddedJobStateV1::LocalReady
            )
        )
}

pub(super) fn released_export_authority_for_status(
    status: OcompJobStatus,
    export: Option<outbe_node::ocomp::retention::ExportAuthorityV1>,
) -> eyre::Result<Option<outbe_node::ocomp::retention::ExportAuthorityV1>> {
    if export.is_none() && status != OcompJobStatus::Expired {
        bail!("closed non-expired OCOMP job has no released export authority");
    }
    Ok(export)
}

pub(super) fn ignored_compute_result_reason(
    projection: AsyncOutcomeProjectionV1,
    terminal_reason: Option<EmbeddedTerminalReasonV1>,
) -> Option<&'static str> {
    if projection == AsyncOutcomeProjectionV1::CheckpointPruned {
        Some("checkpoint_pruned")
    } else if terminal_reason == Some(EmbeddedTerminalReasonV1::Expired) {
        Some("expired")
    } else {
        None
    }
}
