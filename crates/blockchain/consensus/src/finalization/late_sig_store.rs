//! Process-local store of individual finalize signatures for the late
//! credits (Phase 5).
//!
//! After a block finalizes on the eager 2f+1 quorum, additional (slow-but-honest)
//! validators keep gossiping their finalize votes. The
//! [`OutbeReporter`](crate::reporter) observes those individual votes. The
//! `FinalizeVerifyActor` verifies them and buffers them by proposal hash and
//! signed `(epoch, view, parent_view, signer)` binding. A proposer selects the
//! escrow binding from its own branch and aggregates matching votes for the
//! in-window targets into a [`LateFinalizeCreditsArtifact`]. No direct local
//! finalization of the target is required for this best-effort credit.
//!
//! This is **best-effort, process-local** state, not consensus state. The chosen
//! artifact is in the block and therefore affects its hash. Validators re-verify (via
//! [`crate::proof::verify_late_finalize_proof`]) whatever a proposer chooses to
//! include. An empty store simply credits nobody (degrading to today's
//! behavior).

use alloy_primitives::B256;
use commonware_codec::Encode;
use commonware_cryptography::bls12381::{
    self,
    primitives::{
        ops::aggregate,
        variant::{MinPk, Variant},
    },
};
use outbe_primitives::reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

type MinPkSig = <MinPk as Variant>::Signature;

// MinPk signatures are fixed 96-byte G2 points; a size drift fails compilation.
const _: [(); 96] = [(); <MinPkSig as commonware_codec::FixedSize>::SIZE];

/// Process-local late-finalize signature store. These components share it
/// (read-modify-write under a `Mutex`):
/// - the `FinalizeVerifyActor` (records verified votes that the reporter observed)
/// - the `FinalizationActor` (resolves `fb_hash` targets to block numbers)
/// - the application handler (packs the proposer artifact)
///
/// It is **not** consensus state. See the module docs.
pub type SharedLateFinalizeStore = Arc<Mutex<LateFinalizeSigStore>>;

/// Construct an empty [`SharedLateFinalizeStore`] for inclusion window `window_k`.
pub fn shared(window_k: u64) -> SharedLateFinalizeStore {
    Arc::new(Mutex::new(LateFinalizeSigStore::new(window_k)))
}

/// Maximum number of distinct proposal hashes with locally observed votes.
/// The oldest observation is evicted when full; hash byte order must never
/// determine retention on a long-running network.
const MAX_PENDING_TARGETS: usize = 512;

/// Exact signed proposal domain for a finalize vote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizeVoteTarget {
    pub epoch: u64,
    pub view: u64,
    pub parent_view: u64,
    pub fb_hash: B256,
}

impl FinalizeVoteTarget {
    pub fn from_proposal(
        proposal: &commonware_consensus::simplex::types::Proposal<crate::digest::Digest>,
    ) -> Self {
        Self {
            epoch: proposal.round.epoch().get(),
            view: proposal.round.view().get(),
            parent_view: proposal.parent.get(),
            fb_hash: proposal.payload.0,
        }
    }

    fn matches_binding(&self, epoch: u64, view: u64, parent_view: u64) -> bool {
        self.epoch == epoch && self.view == view && self.parent_view == parent_view
    }
}

/// Canonical committee fingerprint and size carried by late credits.
#[derive(Clone, Copy, Debug)]
pub struct LateFinalizeCommittee {
    pub set_hash: B256,
    pub size: usize,
}

/// The target and escrow binding selected by the candidate proposal branch.
/// The target need not have a direct local finalization certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LateCreditBranchTarget {
    pub fb_number: u64,
    pub binding: FinalizeVoteTarget,
    pub committee_set_hash: B256,
    pub committee_size: usize,
}

/// One buffered finalize vote with the proposal binding that the signer actually
/// signed. A finalize signature is valid only over `Proposal{Round(epoch, view),
/// parent_view, payload = fb_hash}.encode()`. Several views may validly sign
/// the same hash. They remain separate so the proposer can select the view
/// escrowed on its own branch without poisoning the aggregate.
#[derive(Clone)]
struct BoundVote {
    epoch: u64,
    view: u64,
    parent_view: u64,
    sig: MinPkSig,
}

/// Individual finalize votes for one proposal (keyed by its `fb_hash`).
/// Keying by `fb_hash` (not `view`) keeps
/// votes for distinct proposals at the same view (equivocations / forks) in
/// separate buffers. So the store never aggregates them together.
struct PendingTarget {
    /// One signed vote per full proposal binding and signer. Distinct valid
    /// views of one digest remain available for branch-local escrow selection.
    votes: BTreeMap<(u64, u64, u64, u32), BoundVote>,
    last_observed_seq: u128,
}

/// Finalize votes for a target block, resolved to its number (window-scoped).
struct ResolvedTarget {
    fb_hash: B256,
    epoch: u64,
    view: u64,
    parent_view: u64,
    committee_set_hash: B256,
    committee_size: usize,
    votes: BTreeMap<u32, MinPkSig>,
}

/// See module docs.
pub struct LateFinalizeSigStore {
    window_k: u64,
    observation_seq: u128,
    /// Votes observed before or after local finalization, keyed by `fb_hash`.
    pending_by_fb_hash: BTreeMap<B256, PendingTarget>,
    /// Votes for finalized proposals, keyed by block number (window-scoped).
    resolved_by_number: BTreeMap<u64, ResolvedTarget>,
    /// `fb_hash -> block number` for every resolved target still in the window.
    /// With this index, the store routes a vote that arrives **after** finalization
    /// (the slow-validator case) straight into the resolved target. The vote does
    /// not stay stranded in `pending_by_fb_hash`.
    resolved_fb_hash_to_number: BTreeMap<B256, u64>,
}

impl LateFinalizeSigStore {
    pub fn new(window_k: u64) -> Self {
        Self {
            window_k,
            observation_seq: 0,
            pending_by_fb_hash: BTreeMap::new(),
            resolved_by_number: BTreeMap::new(),
            resolved_fb_hash_to_number: BTreeMap::new(),
        }
    }

    pub fn window_k(&self) -> u64 {
        self.window_k
    }

    /// Aggregate only votes matching the escrow binding chosen by this branch.
    /// Local direct-F resolution is deliberately irrelevant: an ancestor may be
    /// finalized implicitly, or another branch may use a later F for one digest.
    pub fn build_artifact_for_branch(
        &self,
        proposer_block_number: u64,
        targets: &[LateCreditBranchTarget],
    ) -> LateFinalizeCreditsArtifact {
        let lo = proposer_block_number.saturating_sub(self.window_k);
        let mut ordered = targets.to_vec();
        ordered.sort_by_key(|target| target.fb_number);
        let mut batches = Vec::new();
        let mut last_seen_number = None;

        for target in ordered {
            if last_seen_number == Some(target.fb_number) {
                // Each height has one escrow binding on a proposal branch.
                // A duplicate input is ambiguous, so never merge it.
                continue;
            }
            last_seen_number = Some(target.fb_number);
            if target.fb_number < lo || target.fb_number >= proposer_block_number {
                continue;
            }
            let Some(pending) = self.pending_by_fb_hash.get(&target.binding.fb_hash) else {
                continue;
            };
            let signatures: Vec<_> = pending
                .votes
                .iter()
                .filter(|((epoch, view, parent_view, signer), _)| {
                    *epoch == target.binding.epoch
                        && *view == target.binding.view
                        && *parent_view == target.binding.parent_view
                        && (*signer as usize) < target.committee_size
                })
                .map(|((_, _, _, signer), bound)| (*signer, &bound.sig))
                .collect();
            let Some(nonempty) =
                commonware_utils::iter::NonEmpty::try_new(signatures.iter().map(|(_, sig)| *sig))
            else {
                continue;
            };
            let aggregate = aggregate::combine_signatures::<MinPk, _>(nonempty);
            let encoded = aggregate.encode();
            let Ok(aggregate_signature): Result<[u8; 96], _> = encoded.as_ref().try_into() else {
                tracing::warn!(
                    fb_number = target.fb_number,
                    "unexpected late-credit aggregate length"
                );
                continue;
            };
            let mut signer_bitmap = vec![0u8; target.committee_size.div_ceil(8)];
            for (signer, _) in signatures {
                let idx = signer as usize;
                signer_bitmap[idx / 8] |= 1u8 << (idx % 8);
            }
            batches.push(PerBlockCredit {
                fb_number: target.fb_number,
                fb_hash: target.binding.fb_hash,
                epoch: target.binding.epoch,
                view: target.binding.view,
                parent_view: target.binding.parent_view,
                committee_set_hash: target.committee_set_hash,
                signer_bitmap,
                aggregate_signature,
            });
        }
        LateFinalizeCreditsArtifact { batches }
    }

    /// Buffer one observed (already signature-verified) individual finalize vote,
    /// keyed by the proposal's hash and full signed binding. First writer wins
    /// per `(fb_hash, epoch, view, parent_view, signer)`. Votes arriving after a
    /// local finalization remain available to a child that selects another
    /// valid finalization view for the same digest.
    pub fn record_bound_vote(&mut self, target: FinalizeVoteTarget, signer: u32, sig: MinPkSig) {
        self.observation_seq = self.observation_seq.saturating_add(1);
        let observation_seq = self.observation_seq;
        let binding = target;
        let FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash,
        } = target;
        // A locally resolved round is only one possible child-escrow binding.
        // Keep every verified binding for branch-specific proposal selection.
        if let Some(&fb_number) = self.resolved_fb_hash_to_number.get(&fb_hash) {
            if let Some(target) = self.resolved_by_number.get_mut(&fb_number) {
                if binding.matches_binding(target.epoch, target.view, target.parent_view) {
                    target.votes.entry(signer).or_insert(sig);
                }
            }
        }

        let entry = self
            .pending_by_fb_hash
            .entry(fb_hash)
            .or_insert_with(|| PendingTarget {
                votes: BTreeMap::new(),
                last_observed_seq: observation_seq,
            });
        if let std::collections::btree_map::Entry::Vacant(vacant) =
            entry.votes.entry((epoch, view, parent_view, signer))
        {
            vacant.insert(BoundVote {
                epoch,
                view,
                parent_view,
                sig,
            });
            entry.last_observed_seq = observation_seq;
        }

        // Keep recent proposals regardless of digest ordering. A byte-order
        // eviction would eventually discard almost every new target.
        while self.pending_by_fb_hash.len() > MAX_PENDING_TARGETS {
            let Some((&oldest_fb_hash, _)) = self
                .pending_by_fb_hash
                .iter()
                .min_by_key(|(hash, target)| (target.last_observed_seq, *hash))
            else {
                break;
            };
            self.pending_by_fb_hash.remove(&oldest_fb_hash);
        }
    }

    /// Convenience over [`Self::record_bound_vote`]. It takes the raw MinPk individual
    /// vote as a `bls12381::Signature` (the `bls_individual_vote` that a consensus
    /// `HybridSignature` carries). It keeps the MinPk-variant conversion in one
    /// place, so callers stay free of variant generics. The
    /// caller MUST have verified the finalize vote's signature first.
    pub fn record_bound_individual_vote(
        &mut self,
        target: FinalizeVoteTarget,
        signer: u32,
        bls_individual_vote: &bls12381::Signature,
    ) {
        let sig: MinPkSig = *bls_individual_vote.as_ref();
        self.record_bound_vote(target, signer, sig);
    }

    /// For a finalized proposal:
    /// 1. Rekey its buffered votes (keyed by `fb_hash`) to its block number.
    /// 2. Register the `fb_hash -> number` index, so later-arriving votes route
    ///    straight to the resolved target.
    /// 3. Prune targets that are no longer in the inclusion window.
    // The canonical certificate binding (epoch/view/parent_view/fb_hash/
    // committee_set_hash/committee_size) is irreducible here. The per-block credit
    // binds every field, so a pure post-finalization vote still verifies.
    pub fn resolve_finalized_target(
        &mut self,
        binding: FinalizeVoteTarget,
        fb_number: u64,
        committee: LateFinalizeCommittee,
    ) {
        let FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash,
        } = binding;
        let LateFinalizeCommittee {
            set_hash: committee_set_hash,
            size: committee_size,
        } = committee;
        // The canonical (epoch, view, parent_view) come from the finalized
        // certificate. So the resolved target has the correct binding whether or
        // not the store buffered any vote before finalization. (A pure
        // post-finalization vote has no pending entry. It must still carry the real
        // binding, or the rebuilt proposal won't match what the signer signed.)
        let target = self
            .resolved_by_number
            .entry(fb_number)
            .or_insert_with(|| ResolvedTarget {
                fb_hash,
                epoch,
                view,
                parent_view,
                committee_set_hash,
                committee_size,
                votes: BTreeMap::new(),
            });
        if let Some(pending) = self.pending_by_fb_hash.get(&fb_hash) {
            // Merge (first writer wins per signer) in case votes arrive split.
            // Merge ONLY votes whose binding matches the canonical certificate. A
            // cross-view vote for the same `fb_hash` signed a different message. It
            // would make the aggregate fail `verify_same_message`, so the loop drops it.
            for ((_, _, _, signer), bound) in &pending.votes {
                if binding.matches_binding(bound.epoch, bound.view, bound.parent_view) {
                    target.votes.entry(*signer).or_insert(bound.sig);
                }
            }
        }
        self.resolved_fb_hash_to_number.insert(fb_hash, fb_number);

        // Window close: prune the in-memory buffer at N+K+1. Keep only block
        // numbers within `[resolved - K, resolved]`. Older targets can no longer
        // receive credit. (This prunes the process-local gathering buffer. The EVM
        // settlement state is freed separately at settle, N+K.)
        let min_keep = fb_number.saturating_sub(self.window_k);
        self.resolved_by_number.retain(|&n, _| n >= min_keep);
        self.resolved_fb_hash_to_number
            .retain(|_, &mut n| n >= min_keep);
    }

    /// Assemble the late-finalize-credits artifact for a block proposed at
    /// `proposer_block_number`. The artifact covers every resolved target whose
    /// window is still open, i.e.
    /// `fb_number in [proposer_block_number - K, proposer_block_number - 1]`.
    /// For each target, aggregate the locally-held individual votes into a
    /// per-block credit.
    pub fn build_artifact(&self, proposer_block_number: u64) -> LateFinalizeCreditsArtifact {
        let lo = proposer_block_number.saturating_sub(self.window_k);
        let hi = proposer_block_number.saturating_sub(1);
        let mut batches = Vec::new();
        if lo > hi {
            return LateFinalizeCreditsArtifact { batches };
        }
        // BTreeMap::range yields ascending fb_number, so batches are already in
        // the canonical strictly-ascending order the codec requires.
        for (&fb_number, target) in self.resolved_by_number.range(lo..=hi) {
            let Some(sigs) = commonware_utils::iter::NonEmpty::try_new(target.votes.values())
            else {
                continue;
            };
            let agg = aggregate::combine_signatures::<MinPk, _>(sigs);
            let encoded = agg.encode();
            let Ok(aggregate_signature): Result<[u8; 96], _> = encoded.as_ref().try_into() else {
                tracing::warn!(
                    fb_number,
                    encoded_len = encoded.len(),
                    "unexpected aggregate signature length; skipping late-credit batch"
                );
                continue;
            };

            let mut signer_bitmap = vec![0u8; target.committee_size.div_ceil(8)];
            for &signer in target.votes.keys() {
                let idx = signer as usize;
                if idx < target.committee_size {
                    signer_bitmap[idx / 8] |= 1u8 << (idx % 8);
                }
            }

            batches.push(PerBlockCredit {
                fb_number,
                fb_hash: target.fb_hash,
                epoch: target.epoch,
                view: target.view,
                parent_view: target.parent_view,
                committee_set_hash: target.committee_set_hash,
                signer_bitmap,
                aggregate_signature,
            });
        }
        LateFinalizeCreditsArtifact { batches }
    }

    #[cfg(test)]
    pub(crate) fn resolved_len(&self) -> usize {
        self.resolved_by_number.len()
    }

    /// Number of buffered (not-yet-resolved) votes for `fb_hash`. Test-only:
    /// used by the reporter wiring test to assert an observed `Finalize`
    /// activity was recorded.
    #[cfg(test)]
    pub(crate) fn pending_vote_count(&self, fb_hash: B256) -> usize {
        self.pending_by_fb_hash
            .get(&fb_hash)
            .map(|p| p.votes.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod proptests;
