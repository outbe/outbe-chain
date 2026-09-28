//! E2E-only Byzantine proposer hook. It is compiled only when both the E2E
//! marker and test protocol overrides are enabled. The environment flag arms
//! one forged committee pre-announce per successor epoch on each node.

use std::sync::atomic::{AtomicU64, Ordering};

use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;
use tracing::warn;

use crate::dkg_manager::{BoundaryRequirement, OdkoOutcome};

const BYZANTINE_PREANNOUNCE_ENV: &str = "OUTBE_TEST_BYZANTINE_PREANNOUNCE";

// Every validator is armed in this scenario. Once each has tried a forged
// proposal, later rounds must carry the genuine pre-announce so handoff can
// still finalize. Successor epochs start at 1, making 0 a safe initial value.
static LAST_FORGED_EPOCH: AtomicU64 = AtomicU64::new(0);

pub(super) fn override_artifact(
    requirement: BoundaryRequirement,
    planned: Option<ConsensusHeaderArtifact>,
) -> Option<ConsensusHeaderArtifact> {
    if requirement != BoundaryRequirement::NoPending
        || std::env::var_os(BYZANTINE_PREANNOUNCE_ENV).is_none()
    {
        return planned;
    }
    let Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, outcome }) = planned.as_ref()
    else {
        return planned;
    };
    let Ok(mut forged) = OdkoOutcome::decode(outcome.as_ref()) else {
        return planned;
    };
    if LAST_FORGED_EPOCH.fetch_max(*epoch, Ordering::AcqRel) >= *epoch {
        return planned;
    }
    // Rudis validates the entire pre-announce against its own pending DKG
    // outcome. Toggling this canonical ODKO flag preserves structural decoding
    // and the successor epoch while making those bytes differ from local DKG.
    forged.is_full_dkg = !forged.is_full_dkg;
    warn!(
        epoch,
        "byzantine test hook: proposing a forged committee pre-announce"
    );
    Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
        epoch: *epoch,
        outcome: forged.encode(),
    })
}
