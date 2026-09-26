//! Test-only byzantine proposer, compiled only with `test-protocol-overrides`
//! (e2e builds). With `OUTBE_TEST_BYZANTINE_PREANNOUNCE` set, a leader whose
//! parent ancestry already committed the round epoch's boundary carries a
//! forged `CommitteePreAnnounce` for the next epoch: the committed boundary's
//! own outcome relabelled as the successor's. It decodes and passes a
//! follower's structural checks, so only the validators' admission rule stands
//! between it and a finalized block.

use commonware_consensus::types::Epoch;
use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;
use tracing::warn;

use crate::dkg_manager::{BoundaryRequirement, Mailbox, OdkoOutcome};

pub(super) const BYZANTINE_PREANNOUNCE_ENV: &str = "OUTBE_TEST_BYZANTINE_PREANNOUNCE";

pub(super) async fn override_artifact(
    dkg_manager: &Mailbox,
    requirement: BoundaryRequirement,
    round_epoch: Epoch,
    planned: Option<ConsensusHeaderArtifact>,
) -> Option<ConsensusHeaderArtifact> {
    if requirement != BoundaryRequirement::AlreadyCommitted
        || std::env::var_os(BYZANTINE_PREANNOUNCE_ENV).is_none()
    {
        return planned;
    }
    let Some(committed) = dkg_manager.pending_boundary_artifact(round_epoch).await else {
        return planned;
    };
    let (Ok(mut outcome), Some(next)) = (
        OdkoOutcome::decode(committed.outcome.as_ref()),
        round_epoch.get().checked_add(1),
    ) else {
        return planned;
    };
    outcome.epoch = Epoch::new(next);
    warn!(
        epoch = next,
        "byzantine test hook: proposing a forged committee pre-announce"
    );
    Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
        epoch: next,
        outcome: outcome.encode(),
    })
}
