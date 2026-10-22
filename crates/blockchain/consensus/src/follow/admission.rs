//! Committee admission: the one transition from a certified finalized header to
//! the follower's committee chain. These paths share it:
//! - live delivery
//! - restart replay
//! - restart rebuild
//! - the trust-root anchor
//! - the enclave's committee stream
//!
//! Every path verifies the header's finalization certificate against the
//! already trusted committee *before* the chain changes. Then it applies the rule
//! its policy names. The anchor is the one exception to "already trusted". The
//! genesis validator set authenticates its committee. Thus the anchor path verifies
//! it against the committee it would install, before it installs that committee.

use commonware_consensus::{simplex::types::Finalization, types::Epoch};
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use eyre::Result;
use outbe_primitives::reshare_artifact::{
    decode_outbe_block_artifacts, ConsensusHeaderArtifact as Cha,
};

use super::{verify_with, CommitteeChain};
use crate::{digest::Digest, errors::FollowError, hybrid::HybridScheme};

/// What a certified header may change in the committee chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionPolicy {
    /// Delivery routed to `routed_epoch` (live and replay). The certificate is
    /// from that epoch, or from its successor only on the block carrying that
    /// successor's `BoundaryOutcome`. A pre-announce must name the certifying
    /// epoch's successor. A boundary must be the certifying epoch's own.
    Routed { routed_epoch: Epoch },
    /// A committee handoff that must pre-announce the certifying epoch's
    /// successor (the enclave's committee stream).
    Successor,
    /// Rebuild scan for `epoch`'s carrier. Only a pre-announce of `epoch`
    /// changes the chain. The scan ignores any other artifact.
    CarrierFor { epoch: Epoch },
}

/// The chain change an admitted header made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Unchanged,
    SuccessorRegistered(Epoch),
    BoundaryRegistered(Epoch),
}

impl CommitteeChain {
    /// Admit one certified finalized header under `policy`.
    pub fn admit(
        &mut self,
        finalization: &Finalization<HybridScheme<MinSig>, Digest>,
        extra_data: &[u8],
        policy: AdmissionPolicy,
    ) -> Result<Admission> {
        let certified = finalization.proposal.round.epoch();
        self.verify_finalization(certified, finalization)?;
        self.admit_authenticated_header(certified, extra_data, policy)
    }

    /// Apply a header authenticated by a verified descendant certificate and
    /// a consecutive parent-hash chain. The caller must verify that proof
    /// before invoking this method.
    pub(crate) fn admit_authenticated_header(
        &mut self,
        certified: Epoch,
        extra_data: &[u8],
        policy: AdmissionPolicy,
    ) -> Result<Admission> {
        if let AdmissionPolicy::Routed { routed_epoch } = policy {
            if certified != routed_epoch
                && routed_epoch.get().checked_add(1) != Some(certified.get())
            {
                return Err(FollowError::CertifiedEpochNotRoutedOrSuccessor {
                    certified: certified.get(),
                    routed: routed_epoch.get(),
                }
                .into());
            }
        }
        let artifact = decode_outbe_block_artifacts(extra_data)
            .map_err(FollowError::decode_authenticated_block_artifacts)?
            .consensus_header_artifact;
        let successor = certified.get().checked_add(1);

        match (policy, artifact) {
            (AdmissionPolicy::Routed { routed_epoch }, artifact) => {
                self.admit_routed_artifact(certified, routed_epoch, artifact)
            }
            (AdmissionPolicy::Successor, Some(Cha::CommitteePreAnnounce { epoch, outcome })) => {
                if successor != Some(epoch) {
                    return Err(FollowError::TransitionNotImmediateSuccessor.into());
                }
                self.register_epoch_from_outcome(Epoch::new(epoch), &outcome)?;
                Ok(Admission::SuccessorRegistered(Epoch::new(epoch)))
            }
            (
                AdmissionPolicy::Successor,
                Some(Cha::BoundaryOutcome(_) | Cha::DealerLog(_)) | None,
            ) => Err(FollowError::TransitionLacksPreAnnounce.into()),
            (
                AdmissionPolicy::CarrierFor { epoch: target },
                Some(Cha::CommitteePreAnnounce { epoch, outcome }),
            ) if epoch == target.get() => {
                if successor != Some(epoch) {
                    return Err(FollowError::CarrierFinalizedByNonPredecessor {
                        target: target.get(),
                        certified: certified.get(),
                    }
                    .into());
                }
                self.register_epoch_from_outcome(target, &outcome)?;
                Ok(Admission::SuccessorRegistered(target))
            }
            (
                AdmissionPolicy::CarrierFor { .. },
                Some(
                    Cha::CommitteePreAnnounce { .. } | Cha::BoundaryOutcome(_) | Cha::DealerLog(_),
                )
                | None,
            ) => Ok(Admission::Unchanged),
        }
    }

    fn admit_routed_artifact(
        &mut self,
        certified: Epoch,
        routed_epoch: Epoch,
        artifact: Option<Cha>,
    ) -> Result<Admission> {
        if certified != routed_epoch
            && !matches!(
                &artifact,
                Some(Cha::BoundaryOutcome(boundary)) if boundary.epoch == certified.get()
            )
        {
            return Err(FollowError::EpochChangedWithoutBoundaryOutcome {
                from: routed_epoch.get(),
                to: certified.get(),
            }
            .into());
        }
        match artifact {
            Some(Cha::CommitteePreAnnounce { epoch, outcome }) => {
                if certified.get().checked_add(1) != Some(epoch) {
                    return Err(FollowError::PreAnnouncesNonSuccessor {
                        certified: certified.get(),
                        announced: epoch,
                    }
                    .into());
                }
                self.register_epoch_from_outcome(Epoch::new(epoch), &outcome)?;
                Ok(Admission::SuccessorRegistered(Epoch::new(epoch)))
            }
            Some(Cha::BoundaryOutcome(boundary)) => {
                if boundary.epoch != certified.get() {
                    return Err(FollowError::BoundaryEpochDiffersFromCertificate {
                        boundary: boundary.epoch,
                        certificate: certified.get(),
                    }
                    .into());
                }
                self.register_epoch_from_outcome(certified, &boundary.outcome)
                    .map_err(|error| {
                        FollowError::boundary_outcome_conflicts(certified.get(), error)
                    })?;
                Ok(Admission::BoundaryRegistered(certified))
            }
            Some(Cha::DealerLog(_)) | None => Ok(Admission::Unchanged),
        }
    }

    /// Establish the trust root from the anchor epoch's boundary block. Its
    /// committee must be the genesis validator set. The block's certificate
    /// must verify against that committee before this method installs it.
    pub fn admit_anchor(
        &mut self,
        finalization: &Finalization<HybridScheme<MinSig>, Digest>,
        extra_data: &[u8],
    ) -> Result<Epoch> {
        let anchor = self.anchor_epoch;
        if self.highest_registered.is_some() {
            return Err(FollowError::AnchorAlreadyEstablished {
                epoch: anchor.get(),
            }
            .into());
        }
        let artifact = decode_outbe_block_artifacts(extra_data)
            .map_err(FollowError::decode_block_artifacts)?
            .consensus_header_artifact;
        let boundary = match artifact {
            Some(Cha::BoundaryOutcome(boundary)) if boundary.epoch == anchor.get() => boundary,
            Some(
                Cha::BoundaryOutcome(_) | Cha::DealerLog(_) | Cha::CommitteePreAnnounce { .. },
            )
            | None => {
                return Err(FollowError::AnchorMissingBoundaryOutcome {
                    epoch: anchor.get(),
                }
                .into());
            }
        };
        let prepared = self.prepare_committee(anchor, &boundary.outcome)?;
        let Some((verifier, _)) = prepared.install.as_ref() else {
            return Err(FollowError::AnchorAlreadyEstablished {
                epoch: anchor.get(),
            }
            .into());
        };
        verify_with(verifier, anchor, finalization)?;
        self.commit_committee(prepared);
        Ok(anchor)
    }
}
