//! Lightweight follower: cold-sync finalized blocks from an upstream node and
//! verify them against the chain's committee, WITHOUT running consensus.
//!
//! **Trust model - committee-chaining.** outbe's finalize certificate is an
//! atomic aggregate of individual MinPk votes and a mandatory MinSig threshold
//! VRF proof over a *committee-bound* namespace. Both are verified with the
//! epoch-scoped committee material, which changes on every reshare. A follower
//! therefore:
//!
//! 1. anchors the START epoch's committee on the **genesis validator MinPk
//!    set**, read from the follower's OWN genesis state - the trust root;
//!    nothing the operator must provide;
//! 2. reads each later epoch's committee from a `CommitteePreAnnounce` in the
//!    previous epoch's last finalized block, verifies that block with the already
//!    trusted previous committee, and only then installs the next verifier.
//!
//! All inputs are public on-chain data carried in the boundary block
//! `extra_data` (the full DKG [`Output`] - players + polynomial); the follower
//! never holds any DKG secret. [`CommitteeChain`] implements this chaining; it
//! is exercised by `phase0_spike_*` (the de-risk gate) and the tests below.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

use alloy_primitives::{keccak256, B256};
use commonware_consensus::{simplex::types::Finalization, types::Epoch};
use commonware_cryptography::bls12381;
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_parallel::Sequential;
use commonware_utils::ordered::Set;
use eyre::{bail, Result};

use crate::digest::Digest;
use crate::hybrid::{
    bls_batch_verification_rng, HybridScheme, HybridSchemeProvider, VrfMaterialProvider,
};

mod admission;
mod driver;
pub mod engine;
mod epocher;
mod resolver;
mod stubs;
pub mod upstream;

pub use admission::{Admission, AdmissionPolicy};
pub use engine::{run_follow_engine, FollowEngineConfig};
pub use epocher::FollowerEpocher;
pub use upstream::{
    decode_public_finalization, decode_public_finalized_block, CertifiedFinalizedBlock,
    FinalizedSource, LocalBlockSource, PublicFinalizedBlockDecodeError, TipSource,
};

/// Builds and chains per-epoch finalization verifiers from finalized boundary
/// blocks, anchored on the trusted genesis committee. Verifiers are kept in a
/// [`HybridSchemeProvider`] keyed by epoch - the same provider type the live
/// stack uses - so cert verification is byte-identical to the validator path.
///
/// **Trust root.** Consensus finality is a multisig over the committee's
/// individual MinPk keys. The mandatory MinSig VRF proof supplies the finalized
/// round seed but is not the committee identity authenticator. So the anchor is the **genesis validator
/// MinPk set**, read from the follower's OWN genesis state - not a VRF group
/// key, and nothing the operator has to provide. The start epoch's committee
/// (`output.players()`) must equal this set; each later epoch's committee is
/// trusted via the finalized-boundary chain.
pub struct CommitteeChain {
    /// The start epoch the anchor is rooted at (genesis = 0).
    anchor_epoch: Epoch,
    /// The trusted start-epoch committee: the genesis validator MinPk keys.
    anchor_participants: Set<bls12381::PublicKey>,
    scheme_provider: HybridSchemeProvider<MinSig>,
    /// Highest epoch whose committee verifier has been registered.
    highest_registered: Option<Epoch>,
    /// Exact authenticated outcome hash by epoch. Repeated pre-announces are
    /// idempotent; a conflicting outcome can never replace trusted material.
    outcome_hashes: BTreeMap<u64, B256>,
}

impl CommitteeChain {
    /// Create a chain anchored on the trusted genesis committee
    /// (`anchor_participants` = the genesis validator MinPk set, read from the
    /// follower's genesis state) at `anchor_epoch` (0 for a genesis anchor).
    pub fn new(anchor_epoch: Epoch, anchor_participants: Set<bls12381::PublicKey>) -> Self {
        Self {
            anchor_epoch,
            anchor_participants,
            scheme_provider: HybridSchemeProvider::new(),
            highest_registered: None,
            outcome_hashes: BTreeMap::new(),
        }
    }

    /// The epoch the anchor is rooted at (the first epoch the follower can verify).
    pub fn anchor_epoch(&self) -> u64 {
        self.anchor_epoch.get()
    }

    /// The per-epoch verifier provider, ready to hand to cert-verification paths.
    pub fn scheme_provider(&self) -> &HybridSchemeProvider<MinSig> {
        &self.scheme_provider
    }

    /// Highest epoch whose verifier is registered, if any.
    pub fn highest_registered(&self) -> Option<Epoch> {
        self.highest_registered
    }

    /// Register epoch `epoch`'s committee verifier from its finalized boundary
    /// `outcome` bytes (the ODKO-wrapped DKG output in the boundary block's
    /// `extra_data`).
    ///
    /// For the anchor epoch the committee MUST equal the trusted genesis
    /// validator set - this is the trust root. For later epochs the caller must
    /// have authenticated the carrier with the prior committee; [`Self::admit`]
    /// is the transition that does so.
    ///
    /// Returns the epoch's ordered participant set.
    pub fn register_epoch_from_outcome(
        &mut self,
        epoch: Epoch,
        outcome: &[u8],
    ) -> Result<Set<bls12381::PublicKey>> {
        let prepared = self.prepare_committee(epoch, outcome)?;
        Ok(self.commit_committee(prepared))
    }

    /// Validate `outcome` as epoch `epoch`'s committee and build its verifier,
    /// without changing the chain. Idempotent replays of the registered outcome
    /// prepare nothing to install.
    fn prepare_committee(&self, epoch: Epoch, outcome: &[u8]) -> Result<PreparedCommittee> {
        let decoded = crate::dkg_manager::OdkoOutcome::decode(outcome).map_err(|error| {
            eyre::eyre!("boundary outcome is not a decodable full DKG output: {error}")
        })?;
        if decoded.epoch != epoch {
            bail!(
                "boundary outcome epoch label {} does not match registered epoch {}",
                decoded.epoch.get(),
                epoch.get()
            );
        }
        let participants = decoded.output.players().clone();
        let outcome_hash = keccak256(outcome);

        if let Some(existing) = self.outcome_hashes.get(&epoch.get()) {
            if *existing != outcome_hash {
                bail!(
                    "conflicting committee outcome replay for epoch {}",
                    epoch.get()
                );
            }
            return Ok(PreparedCommittee {
                epoch,
                participants,
                install: None,
            });
        }

        if let Some(highest) = self.highest_registered {
            let expected = highest.get().saturating_add(1);
            if epoch.get() != expected {
                bail!(
                    "committee epoch {} is not sequential after authenticated epoch {}",
                    epoch.get(),
                    highest.get()
                );
            }
        } else if epoch != self.anchor_epoch {
            bail!(
                "first registered committee epoch {} is not anchor epoch {}",
                epoch.get(),
                self.anchor_epoch.get()
            );
        }

        // Trust root: the anchor epoch's committee MUST be the trusted genesis
        // validator set. Consensus finality is a multisig over these MinPk keys,
        // so matching the participant set (NOT the VRF group key) authenticates
        // the committee. Compare as ordered sets (both pubkey-sorted).
        if epoch == self.anchor_epoch && participants != self.anchor_participants {
            bail!(
                "anchor mismatch: start-epoch {} committee ({} validators) does not match the \
                 trusted genesis validator set ({} validators)",
                epoch.get(),
                participants.len(),
                self.anchor_participants.len(),
            );
        }

        // The follower is anchored at genesis epoch/version 0, and every
        // successful DKG activation increments both counters exactly once.
        // Restore the authenticated epoch's material version explicitly:
        // `HybridScheme::verifier` defaults it to zero, which verifies the BLS
        // certificate but produces a non-canonical committee_set_hash_v2 after
        // epoch 0.
        let vrf_materials =
            VrfMaterialProvider::new(epoch.get(), decoded.output.public().clone(), None);
        let verifier = HybridScheme::<MinSig>::verifier_with_vrf_provider(
            &crate::config::outbe_app_namespace(),
            participants.clone(),
            vrf_materials,
        )
        .ok_or_else(|| {
            eyre::eyre!(
                "failed to build committee verifier for epoch {}",
                epoch.get()
            )
        })?;
        Ok(PreparedCommittee {
            epoch,
            participants,
            install: Some((verifier, outcome_hash)),
        })
    }

    fn commit_committee(&mut self, prepared: PreparedCommittee) -> Set<bls12381::PublicKey> {
        let PreparedCommittee {
            epoch,
            participants,
            install,
        } = prepared;
        if let Some((verifier, outcome_hash)) = install {
            self.scheme_provider.register(epoch, verifier);
            self.outcome_hashes.insert(epoch.get(), outcome_hash);
            self.highest_registered = Some(match self.highest_registered {
                Some(h) => h.max(epoch),
                None => epoch,
            });
        }
        participants
    }

    /// Advance the chain from a finalized block's `extra_data`, registering an
    /// epoch's committee verifier (the forward-chaining step). Returns the
    /// registered epoch, if any.
    ///
    /// Two carriers register a committee:
    /// - [`CommitteePreAnnounce`](outbe_primitives::reshare_artifact::ConsensusHeaderArtifact::CommitteePreAnnounce)
    ///   the Path A committee-chaining carrier: epoch `E`'s committee riding a
    ///   block finalized by the already-trusted `E-1` committee. This is the
    ///   authenticated path - the trust chains from genesis through each E-1.
    /// - [`BoundaryOutcome`](outbe_primitives::reshare_artifact::ConsensusHeaderArtifact::BoundaryOutcome)
    ///   the activating boundary at `E*L+1`, finalized by `E` ITSELF. We register
    ///   from it ONLY for a not-yet-known epoch (the genesis anchor; and, until the
    ///   pre-announce producer is wired, epochs lacking a pre-announce). We must NOT
    ///   let it OVERRIDE a committee already registered via its `E-1` pre-announce:
    ///   a self-finalized boundary overriding the chained committee is exactly the
    ///   D1 self-certification bug.
    ///
    /// Safe only for `extra_data` from blocks already verified as finalized by the
    /// trusted committee (the marshal enforces this via its `provider`), so the
    /// registered committee inherits that trust. Test-only: production chaining
    /// goes through [`Self::admit`] / [`Self::admit_anchor`], which verify the
    /// certificate before any registration.
    #[cfg(test)]
    pub(crate) fn advance_from_block_extra_data(
        &mut self,
        extra_data: &[u8],
    ) -> Result<Option<Epoch>> {
        use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact as CHA;
        let artifacts =
            outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(extra_data)
                .map_err(|e| eyre::eyre!("failed to decode block artifacts: {e:?}"))?;
        match artifacts.consensus_header_artifact {
            Some(CHA::CommitteePreAnnounce { epoch, outcome }) => {
                let epoch = Epoch::new(epoch);
                self.register_epoch_from_outcome(epoch, &outcome)?;
                Ok(Some(epoch))
            }
            Some(CHA::BoundaryOutcome(boundary)) => {
                let epoch = Epoch::new(boundary.epoch);
                if epoch == self.anchor_epoch && self.highest_registered.is_none() {
                    self.register_epoch_from_outcome(epoch, &boundary.outcome)?;
                    Ok(Some(epoch))
                } else {
                    // Later boundaries are self-finalized and never install or
                    // replace a committee. Their committee must already have been
                    // chained from an E-1 pre-announce.
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    /// Verify a finalization certificate for `epoch` against its registered
    /// committee verifier. Errors if no verifier is registered for `epoch` or the
    /// certificate fails verification.
    pub fn verify_finalization(
        &self,
        epoch: Epoch,
        finalization: &Finalization<HybridScheme<MinSig>, Digest>,
    ) -> Result<()> {
        let scheme = self.scheme_provider.scoped(epoch).ok_or_else(|| {
            eyre::eyre!("no committee verifier registered for epoch {}", epoch.get())
        })?;
        verify_with(scheme.as_ref(), epoch, finalization)
    }

    /// Drop every authenticated verifier/outcome except the current epoch.
    /// Admission streaming uses this after each verified successor so memory is
    /// independent of network age. It must only be called after the successor
    /// has been registered successfully.
    pub fn retain_only_highest(&mut self) {
        let Some(highest) = self.highest_registered else {
            return;
        };
        for epoch in self.outcome_hashes.keys().copied().collect::<Vec<_>>() {
            if epoch != highest.get() {
                self.scheme_provider.remove(&Epoch::new(epoch));
                self.outcome_hashes.remove(&epoch);
            }
        }
    }
}

/// A validated committee ready to install; `install` is `None` for an
/// idempotent replay of the already registered outcome.
struct PreparedCommittee {
    epoch: Epoch,
    participants: Set<bls12381::PublicKey>,
    install: Option<(HybridScheme<MinSig>, B256)>,
}

fn verify_with(
    scheme: &HybridScheme<MinSig>,
    epoch: Epoch,
    finalization: &Finalization<HybridScheme<MinSig>, Digest>,
) -> Result<()> {
    let mut rng = bls_batch_verification_rng();
    if !finalization.verify(&mut rng, scheme, &Sequential) {
        bail!(
            "finalization certificate failed verification for epoch {}",
            epoch.get()
        );
    }
    Ok(())
}

/// The committee chain shared by the follower's engine, resolver and replay
/// paths. A poisoned lock is recovered: no chain mutation panics part-way (a
/// prepared committee is installed in one infallible step), so the state a
/// panicking holder leaves behind is consistent.
#[derive(Clone)]
pub struct SharedCommitteeChain(Arc<Mutex<CommitteeChain>>);

impl SharedCommitteeChain {
    pub fn new(chain: CommitteeChain) -> Self {
        Self(Arc::new(Mutex::new(chain)))
    }

    pub fn lock(&self) -> MutexGuard<'_, CommitteeChain> {
        match self.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests;
