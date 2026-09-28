//! Header-artifact admission: the one place that decides which consensus header
//! artifact a block at `(parent, round)` must carry (proposer) and which carried
//! artifact is admissible (verifier).
//!
//! Both paths resolve the same [`ResolvedBoundary`] from the parent ancestry and
//! the locally pending DKG boundary, then apply one rule table:
//!
//! | Boundary requirement       | Carried artifact                 | Verdict                                              |
//! |----------------------------|----------------------------------|------------------------------------------------------|
//! | MustEmit                   | `BoundaryOutcome` == pending     | admit                                                |
//! | MustEmit                   | anything else / none             | reject                                               |
//! | AlreadyCommitted/NoPending | `BoundaryOutcome`                | reject (duplicate)                                   |
//! | AlreadyCommitted/NoPending | `CommitteePreAnnounce{e, o}`     | admit iff the local pending boundary is for          |
//! |                            |                                  | `round.epoch + 1` and equals `(e, o)` exactly        |
//! | AlreadyCommitted/NoPending | `DealerLog`                      | admit iff it verifies against the round-epoch ceremony |
//! | AlreadyCommitted/NoPending | none                             | admit                                                |
//!
//! A proposer in the same local state emits exactly what the table admits:
//! the pending boundary under `MustEmit`, nothing under `AlreadyCommitted`, and
//! under `NoPending` the successor pre-announce, else a dealer log, else nothing
//! (block 1 forfeits instead, because it must carry the genesis boundary).

use std::fmt;

use commonware_consensus::types::Epoch;
use outbe_primitives::{consensus::DkgBoundaryArtifact, reshare_artifact::ConsensusHeaderArtifact};

use super::{AncestryReader, BoundaryRequirement, BoundaryRequirementError, Mailbox};
use crate::block::ConsensusBlock;

/// The header artifact a proposer carries for `(parent, round)`, plus the
/// boundary requirement it was derived from (callers record it as a metric).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPlan {
    pub requirement: BoundaryRequirement,
    pub artifact: Option<ConsensusHeaderArtifact>,
}

/// Why a proposer forfeits its slot instead of proposing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalForfeit {
    /// Parent ancestry could not be resolved, or carries a conflicting boundary.
    Boundary(BoundaryRequirementError),
    /// Block 1 without a pending genesis boundary: it must carry one.
    GenesisBoundaryNotReady,
}

impl From<BoundaryRequirementError> for ProposalForfeit {
    fn from(error: BoundaryRequirementError) -> Self {
        Self::Boundary(error)
    }
}

/// Why a carried header artifact was not admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactAdmissionError {
    /// Parent ancestry could not be read; the verifier does not vote.
    Unavailable(String),
    /// The carried artifact violates the admission table; the verifier votes no.
    Rejected(String),
}

impl ArtifactAdmissionError {
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

impl fmt::Display for ArtifactAdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) | Self::Rejected(message) => f.write_str(message),
        }
    }
}

impl From<BoundaryRequirementError> for ArtifactAdmissionError {
    fn from(error: BoundaryRequirementError) -> Self {
        match error {
            BoundaryRequirementError::Unavailable(message) => Self::Unavailable(message),
            BoundaryRequirementError::Conflict(message) => Self::Rejected(message),
        }
    }
}

/// The boundary requirement with the pending artifact carried where it is
/// required, so `MustEmit` without an artifact cannot be represented.
enum ResolvedBoundary {
    NoPending,
    AlreadyCommitted,
    MustEmit(Box<DkgBoundaryArtifact>),
}

impl ResolvedBoundary {
    fn requirement(&self) -> BoundaryRequirement {
        match self {
            Self::NoPending => BoundaryRequirement::NoPending,
            Self::AlreadyCommitted => BoundaryRequirement::AlreadyCommitted,
            Self::MustEmit(_) => BoundaryRequirement::MustEmit,
        }
    }
}

impl Mailbox {
    /// Decide the header artifact a proposer emits on top of `parent` in a round
    /// of `round_epoch`, for a block at `proposed_height`.
    pub async fn plan_header_artifact<R: AncestryReader>(
        &self,
        parent: Option<&ConsensusBlock>,
        round_epoch: Epoch,
        proposed_height: u64,
        ancestry: &R,
    ) -> Result<ArtifactPlan, ProposalForfeit> {
        let resolved = self
            .resolve_for_round(parent, round_epoch, ancestry)
            .await?;
        let requirement = resolved.requirement();
        let artifact = match resolved {
            ResolvedBoundary::MustEmit(boundary) => {
                Some(ConsensusHeaderArtifact::BoundaryOutcome(*boundary))
            }
            ResolvedBoundary::AlreadyCommitted => None,
            ResolvedBoundary::NoPending if proposed_height == 1 => {
                return Err(ProposalForfeit::GenesisBoundaryNotReady)
            }
            ResolvedBoundary::NoPending => match self.pending_next_epoch_artifact(round_epoch) {
                Some(boundary) => Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
                    epoch: boundary.epoch,
                    outcome: boundary.outcome,
                }),
                None => self
                    .get_dealer_log(round_epoch)
                    .await
                    .map(ConsensusHeaderArtifact::DealerLog),
            },
        };
        Ok(ArtifactPlan {
            requirement,
            artifact,
        })
    }

    /// Admit or reject the header artifact `carried` by a block on top of
    /// `parent` in a round of `round_epoch`. Returns the boundary requirement the
    /// verdict was derived from.
    pub async fn admit_header_artifact<R: AncestryReader>(
        &self,
        parent: Option<&ConsensusBlock>,
        round_epoch: Epoch,
        carried: Option<&ConsensusHeaderArtifact>,
        ancestry: &R,
    ) -> Result<BoundaryRequirement, ArtifactAdmissionError> {
        let resolved = self
            .resolve_for_round(parent, round_epoch, ancestry)
            .await?;
        let requirement = resolved.requirement();
        match (resolved, carried) {
            (
                ResolvedBoundary::MustEmit(expected),
                Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)),
            ) => {
                if *boundary != *expected {
                    return Err(rejected(
                        "block BoundaryOutcome does not match pending DKG boundary",
                    ));
                }
                self.verify_pending_boundary_artifact(round_epoch, boundary)
                    .await
                    .map_err(|error| rejected(error.to_string()))?;
            }
            (
                ResolvedBoundary::MustEmit(_),
                None
                | Some(
                    ConsensusHeaderArtifact::DealerLog(_)
                    | ConsensusHeaderArtifact::CommitteePreAnnounce { .. },
                ),
            ) => return Err(rejected("block omitted pending DKG BoundaryOutcome")),
            (
                ResolvedBoundary::AlreadyCommitted,
                Some(ConsensusHeaderArtifact::BoundaryOutcome(_)),
            ) => {
                crate::metrics::record_dkg_boundary_duplicate_rejected();
                return Err(rejected(
                    "duplicate DKG BoundaryOutcome after parent ancestry already committed it",
                ));
            }
            (ResolvedBoundary::NoPending, Some(ConsensusHeaderArtifact::BoundaryOutcome(_))) => {
                crate::metrics::record_dkg_boundary_duplicate_rejected();
                return Err(rejected(
                    "block carried DKG BoundaryOutcome without pending boundary",
                ));
            }
            (
                ResolvedBoundary::AlreadyCommitted | ResolvedBoundary::NoPending,
                Some(ConsensusHeaderArtifact::DealerLog(bytes)),
            ) => {
                self.verify_dealer_log_sync(round_epoch, bytes.as_ref())
                    .map_err(|error| rejected(error.to_string()))?;
            }
            (
                ResolvedBoundary::AlreadyCommitted | ResolvedBoundary::NoPending,
                Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, outcome }),
            ) => self.admit_preannounce(round_epoch, *epoch, outcome.as_ref())?,
            (ResolvedBoundary::AlreadyCommitted | ResolvedBoundary::NoPending, None) => {}
        }
        Ok(requirement)
    }

    /// A pre-announce is admissible only for the round's direct successor epoch
    /// and only when it byte-matches this node's own pending boundary for that
    /// epoch. Fail-closed: without a local successor boundary there is nothing
    /// to compare against, so the carrier cannot ride a finalized block.
    fn admit_preannounce(
        &self,
        round_epoch: Epoch,
        announced_epoch: u64,
        outcome: &[u8],
    ) -> Result<(), ArtifactAdmissionError> {
        let successor = round_epoch.get().checked_add(1).ok_or_else(|| {
            rejected("committee pre-announce has no successor for the round epoch")
        })?;
        if announced_epoch != successor {
            return Err(rejected(format!(
                "committee pre-announce epoch {announced_epoch} is not the successor of round epoch {}",
                round_epoch.get()
            )));
        }
        let expected = self
            .pending_next_epoch_artifact(round_epoch)
            .ok_or_else(|| {
                rejected("no pending DKG boundary artifact to validate committee pre-announce")
            })?;
        if expected.outcome.as_ref() != outcome {
            return Err(rejected(format!(
                "committee pre-announce outcome does not match local DKG output for epoch {announced_epoch}"
            )));
        }
        Ok(())
    }

    async fn resolve_for_round<R: AncestryReader>(
        &self,
        parent: Option<&ConsensusBlock>,
        round_epoch: Epoch,
        ancestry: &R,
    ) -> Result<ResolvedBoundary, BoundaryRequirementError> {
        let pending = self.pending_boundary_artifact(round_epoch).await;
        let requirement = self
            .resolve_boundary(parent, pending.as_ref(), ancestry)
            .await?;
        Ok(match (requirement, pending) {
            (BoundaryRequirement::NoPending, _) => ResolvedBoundary::NoPending,
            (BoundaryRequirement::AlreadyCommitted, _) => ResolvedBoundary::AlreadyCommitted,
            (BoundaryRequirement::MustEmit, Some(boundary)) => {
                ResolvedBoundary::MustEmit(Box::new(boundary))
            }
            (BoundaryRequirement::MustEmit, None) => {
                return Err(BoundaryRequirementError::Unavailable(
                    "boundary requirement requested emission without pending artifact".to_string(),
                ))
            }
        })
    }
}

fn rejected(message: impl Into<String>) -> ArtifactAdmissionError {
    ArtifactAdmissionError::Rejected(message.into())
}
