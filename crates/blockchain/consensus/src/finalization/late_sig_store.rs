//! Process-local store of individual finalize signatures for the late
//! credits (Phase 5).
//!
//! After a block finalizes on the eager 2f+1 quorum, additional (slow-but-honest)
//! validators keep gossiping their finalize votes. The
//! [`OutbeReporter`](crate::reporter) observes those individual votes. The
//! `FinalizeVerifyActor` verifies them and buffers them here, keyed by the
//! finalized proposal hash (`fb_hash`). When the matching block finalizes and
//! its number is known, the store rekeys the buffer to `(fb_number, fb_hash)`.
//! (`FinalizationActor` has the height. The reporter sets
//! `finalized_block_number: 0`.) When this node is the
//! proposer of `N+1..N+K`, it aggregates the votes it locally holds for each
//! in-window target into a [`LateFinalizeCreditsArtifact`].
//!
//! This is **best-effort, process-local** state. It is *not* consensus state. It
//! never affects the block hash. Validators re-verify (via
//! [`crate::proof::verify_late_finalize_proof`]) whatever a proposer chooses to
//! include. An empty store simply credits nobody (degrading to today's
//! behavior). Durable rebroadcast / hard anti-censorship.

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

/// Cap on per-target buffers awaiting block-number resolution. It bounds memory if
/// finalizations stall (targets never resolve). The store drops the lowest-`fb_hash`
/// targets first. This order is arbitrary, NOT by age (the store tracks no
/// insertion order or view). Eviction only fires under a stalled-finalization flood,
/// where any bounded drop suffices. If true age-based eviction is ever needed, add
/// a per-target view/height field and evict on that instead.
const MAX_PENDING_TARGETS: usize = 512;

/// Exact signed proposal domain for a finalize vote and its canonical resolution.
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

/// One buffered finalize vote with the proposal binding that the signer actually
/// signed. A finalize signature is valid only over `Proposal{Round(epoch, view),
/// parent_view, payload = fb_hash}.encode()`. A vote can be signed at a non-canonical
/// `(epoch, view, parent_view)` for the same `fb_hash` (equivocation / a buggy or
/// Byzantine validator gossiping across views). The store MUST NOT merge such a vote
/// into the canonical aggregate. Such a vote would fail `verify_same_message` and poison the
/// whole per-block credit. We keep the per-vote binding so `resolve_finalized_target`
/// can drop any vote whose binding != the finalized certificate's.
#[derive(Clone)]
struct BoundVote {
    epoch: u64,
    view: u64,
    parent_view: u64,
    sig: MinPkSig,
}

/// Individual finalize votes for one finalized proposal (keyed by its `fb_hash`),
/// awaiting block-number resolution. Keying by `fb_hash` (not `view`) keeps
/// votes for distinct proposals at the same view (equivocations / forks) in
/// separate buffers. So the store never aggregates them together.
struct PendingTarget {
    /// Signer committee index -> that signer's bound individual MinPk finalize
    /// vote. The binding travels with each vote. So at `resolve_finalized_target`
    /// time, the store removes cross-view votes for the same `fb_hash` against
    /// the canonical certificate.
    votes: BTreeMap<u32, BoundVote>,
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
    /// Votes observed before the proposal finalized, keyed by `fb_hash`.
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
            pending_by_fb_hash: BTreeMap::new(),
            resolved_by_number: BTreeMap::new(),
            resolved_fb_hash_to_number: BTreeMap::new(),
        }
    }

    /// Buffer one observed (already signature-verified) individual finalize vote,
    /// keyed by the proposal's `fb_hash`. First writer wins per `(fb_hash, signer)`.
    /// `(epoch, view, parent_view)` is the binding that the signer signed.
    ///
    /// If the proposal already finalized (its `fb_hash` is in
    /// `resolved_fb_hash_to_number`), this function appends the vote **directly** to
    /// the resolved target. This is the slow-validator path, where the vote arrives
    /// after the eager quorum finalized the block. The append happens ONLY if the
    /// vote's binding matches the canonical certificate's. The function drops a
    /// cross-view vote for the same `fb_hash`, so it cannot poison the canonical
    /// aggregate.
    pub fn record_bound_vote(&mut self, target: FinalizeVoteTarget, signer: u32, sig: MinPkSig) {
        let binding = target;
        let FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash,
        } = target;
        // Late arrival: the target already resolved -> append to it directly, but
        // only if the vote's binding equals the canonical one (else drop).
        if let Some(&fb_number) = self.resolved_fb_hash_to_number.get(&fb_hash) {
            if let Some(target) = self.resolved_by_number.get_mut(&fb_number) {
                if binding.matches_binding(target.epoch, target.view, target.parent_view) {
                    target.votes.entry(signer).or_insert(sig);
                }
                return;
            }
        }

        let entry = self
            .pending_by_fb_hash
            .entry(fb_hash)
            .or_insert_with(|| PendingTarget {
                votes: BTreeMap::new(),
            });
        entry.votes.entry(signer).or_insert(BoundVote {
            epoch,
            view,
            parent_view,
            sig,
        });

        // Bound memory: drop the lowest-`fb_hash` entries beyond the cap
        // (arbitrary order, not by age - see `MAX_PENDING_TARGETS`).
        while self.pending_by_fb_hash.len() > MAX_PENDING_TARGETS {
            let Some((&lowest_fb_hash, _)) = self.pending_by_fb_hash.iter().next() else {
                break;
            };
            self.pending_by_fb_hash.remove(&lowest_fb_hash);
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
        if let Some(pending) = self.pending_by_fb_hash.remove(&fb_hash) {
            // Merge (first writer wins per signer) in case votes arrive split.
            // Merge ONLY votes whose binding matches the canonical certificate. A
            // cross-view vote for the same `fb_hash` signed a different message. It
            // would make the aggregate fail `verify_same_message`, so the loop drops it.
            for (signer, bound) in pending.votes {
                if binding.matches_binding(bound.epoch, bound.view, bound.parent_view) {
                    target.votes.entry(signer).or_insert(bound.sig);
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
