//! Committee admission: the one transition from a certified finalized header to
//! the follower's committee chain, shared by live delivery, restart replay,
//! restart rebuild, the trust-root anchor and the enclave's committee stream.
//!
//! Every path verifies the header's finalization certificate against the
//! already trusted committee *before* the chain changes, then applies the rule
//! its policy names. The anchor is the one exception to "already trusted": its
//! committee is authenticated by the genesis validator set, so it is verified
//! against the committee it would install before that committee is installed.

use commonware_consensus::{simplex::types::Finalization, types::Epoch};
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use eyre::{bail, ensure, eyre, Result};
use outbe_primitives::reshare_artifact::{
    decode_outbe_block_artifacts, ConsensusHeaderArtifact as Cha,
};

use super::{verify_with, CommitteeChain};
use crate::{digest::Digest, hybrid::HybridScheme};

/// What a certified header may change in the committee chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionPolicy {
    /// Delivery routed to `routed_epoch` (live and replay): the certificate is
    /// from that epoch, or from its successor only on the block carrying that
    /// successor's `BoundaryOutcome`. A pre-announce must name the certifying
    /// epoch's successor; a boundary must be the certifying epoch's own.
    Routed { routed_epoch: Epoch },
    /// A committee handoff that must pre-announce the certifying epoch's
    /// successor (the enclave's committee stream).
    Successor,
    /// Rebuild scan for `epoch`'s carrier: only a pre-announce of `epoch`
    /// changes the chain; any other artifact is ignored.
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
        if let AdmissionPolicy::Routed { routed_epoch } = policy {
            ensure!(
                certified == routed_epoch
                    || routed_epoch.get().checked_add(1) == Some(certified.get()),
                "certified epoch {} is neither routed epoch {} nor its successor",
                certified.get(),
                routed_epoch.get()
            );
        }
        self.verify_finalization(certified, finalization)?;
        let artifact = decode_outbe_block_artifacts(extra_data)
            .map_err(|error| eyre!("failed to decode authenticated block artifacts: {error:?}"))?
            .consensus_header_artifact;
        let successor = certified.get().checked_add(1);

        match (policy, artifact) {
            (AdmissionPolicy::Routed { routed_epoch }, artifact) => {
                if certified != routed_epoch {
                    ensure!(
                        matches!(&artifact, Some(Cha::BoundaryOutcome(boundary)) if boundary.epoch == certified.get()),
                        "epoch changes from {} to {} without its BoundaryOutcome",
                        routed_epoch.get(),
                        certified.get()
                    );
                }
                match artifact {
                    Some(Cha::CommitteePreAnnounce { epoch, outcome }) => {
                        ensure!(
                            successor == Some(epoch),
                            "authenticated epoch {} block pre-announces non-successor epoch {epoch}",
                            certified.get()
                        );
                        self.register_epoch_from_outcome(Epoch::new(epoch), &outcome)?;
                        Ok(Admission::SuccessorRegistered(Epoch::new(epoch)))
                    }
                    Some(Cha::BoundaryOutcome(boundary)) => {
                        ensure!(
                            boundary.epoch == certified.get(),
                            "boundary artifact epoch {} differs from certificate epoch {}",
                            boundary.epoch,
                            certified.get()
                        );
                        self.register_epoch_from_outcome(certified, &boundary.outcome)
                            .map_err(|error| {
                                eyre!(
                                    "boundary outcome conflicts with authenticated epoch {} outcome: {error}",
                                    certified.get()
                                )
                            })?;
                        Ok(Admission::BoundaryRegistered(certified))
                    }
                    Some(Cha::DealerLog(_)) | None => Ok(Admission::Unchanged),
                }
            }
            (AdmissionPolicy::Successor, Some(Cha::CommitteePreAnnounce { epoch, outcome })) => {
                ensure!(
                    successor == Some(epoch),
                    "committee transition does not announce the immediate successor"
                );
                self.register_epoch_from_outcome(Epoch::new(epoch), &outcome)?;
                Ok(Admission::SuccessorRegistered(Epoch::new(epoch)))
            }
            (
                AdmissionPolicy::Successor,
                Some(Cha::BoundaryOutcome(_) | Cha::DealerLog(_)) | None,
            ) => bail!("committee transition lacks a pre-announce"),
            (
                AdmissionPolicy::CarrierFor { epoch: target },
                Some(Cha::CommitteePreAnnounce { epoch, outcome }),
            ) if epoch == target.get() => {
                ensure!(
                    successor == Some(epoch),
                    "epoch {} carrier is finalized by non-predecessor epoch {}",
                    target.get(),
                    certified.get()
                );
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

    /// Establish the trust root from the anchor epoch's boundary block: its
    /// committee must be the genesis validator set, and the block's certificate
    /// must verify against that committee before it is installed.
    pub fn admit_anchor(
        &mut self,
        finalization: &Finalization<HybridScheme<MinSig>, Digest>,
        extra_data: &[u8],
    ) -> Result<Epoch> {
        let anchor = self.anchor_epoch;
        ensure!(
            self.highest_registered.is_none(),
            "anchor epoch {} is already established",
            anchor.get()
        );
        let artifact = decode_outbe_block_artifacts(extra_data)
            .map_err(|error| eyre!("failed to decode block artifacts: {error:?}"))?
            .consensus_header_artifact;
        let boundary = match artifact {
            Some(Cha::BoundaryOutcome(boundary)) if boundary.epoch == anchor.get() => boundary,
            Some(Cha::BoundaryOutcome(_) | Cha::DealerLog(_) | Cha::CommitteePreAnnounce { .. })
            | None => bail!(
                "anchor epoch {}'s first block carried no boundary outcome; cannot establish the trust root",
                anchor.get()
            ),
        };
        let prepared = self.prepare_committee(anchor, &boundary.outcome)?;
        let Some((verifier, _)) = prepared.install.as_ref() else {
            bail!("anchor epoch {} is already established", anchor.get());
        };
        verify_with(verifier, anchor, finalization)?;
        self.commit_committee(prepared);
        Ok(anchor)
    }
}
