//! Off-thread verification of raw finalize votes. The Simplex reporter observes
//! these before batch verification, so signatures MUST be verified before late
//! credit admission. The voter gets bounded, nonblocking, best-effort admission;
//! a single worker yields between votes and retains bounded verified evidence.

mod admission;
#[cfg(test)]
mod tests;

use crate::{
    digest::Digest,
    finalization::late_sig_store::SharedLateFinalizeStore,
    hybrid::{bls_batch_verification_rng, HybridScheme, HybridSchemeProvider},
};
use admission::{Admission, Queued};
use alloy_primitives::{keccak256, B256};
use commonware_codec::{Encode, EncodeSize};
use commonware_consensus::{
    simplex::types::{Attributable as _, Finalize},
    types::Epoch,
    Viewable as _,
};
use commonware_cryptography::{bls12381::primitives::variant::MinSig, certificate::Scheme as _};
use commonware_parallel::Sequential;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

type Vote = Finalize<HybridScheme<MinSig>, Digest>;
type Job = (Epoch, Vote);

/// Availability budgets, not consensus validity limits. Overload is metered and
/// oldest backlog is replaced fairly; late credits remain best-effort.
pub const MAX_QUEUED_VOTES: usize = 256;
pub const MAX_QUEUED_BYTES: usize = 256 * 1024;
pub const MAX_VOTE_BYTES: usize = 1024;
pub const MAX_VOTES_PER_SIGNER: usize = 4;
const OBSERVED_RETAIN_VIEWS: u64 = 32;
const MAX_OBSERVED_VOTES: usize = 2048;
const MAX_OBSERVED_BYTES: usize = 2 * 1024 * 1024;
const MAX_VERIFIED_CONFLICTS: usize = 2;

#[derive(Clone)]
pub struct FinalizeVerifyMailbox {
    admission: Arc<Mutex<Admission>>,
    wake: mpsc::Sender<()>,
    scheme_provider: HybridSchemeProvider<MinSig>,
}

impl FinalizeVerifyMailbox {
    /// Enqueue a raw vote without awaiting capacity or doing cryptography. Exact
    /// bytes are deduplicated only while pending. A claimed signer/target is
    /// never treated as authenticated before verification.
    pub fn verify(&self, epoch: Epoch, finalize: Vote) {
        let reject =
            if self.wake.is_closed() {
                Some("closed")
            } else if finalize.proposal.round.epoch() != epoch {
                Some("epoch_mismatch")
            } else if self.scheme_provider.scoped(epoch).is_none_or(|scheme| {
                finalize.signer().get() as usize >= scheme.participants().len()
            }) {
                Some("ineligible")
            } else if finalize.encode_size() > MAX_VOTE_BYTES {
                Some("oversize")
            } else {
                None
            };
        if let Some(reason) = reject {
            metrics::counter!("outbe_finalize_verify_admission_total", "result" => reason)
                .increment(1);
            return;
        }
        let bytes = finalize.encode();
        let queued = Queued {
            id: keccak256(&bytes),
            bytes: bytes.len(),
            job: (epoch, finalize),
        };
        let key = (epoch.get(), queued.job.1.signer().get());
        if self
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(key, queued)
        {
            metrics::counter!("outbe_finalize_verify_admission_total", "result" => "queued")
                .increment(1);
            // Capacity-one wake channel coalesces notifications. No sender waits.
            let _ = self.wake.try_send(());
        }
    }

    #[cfg(test)]
    pub fn disconnected() -> Self {
        let (wake, _rx) = mpsc::channel(1);
        Self {
            admission: Arc::default(),
            wake,
            scheme_provider: HybridSchemeProvider::new(),
        }
    }
}

// Full signed proposal binding (including parent_view), not just payload hash.
type EvidenceKey = (u64, u64, u32, B256);

pub struct FinalizeVerifyActor {
    admission: Arc<Mutex<Admission>>,
    wake: mpsc::Receiver<()>,
    scheme_provider: HybridSchemeProvider<MinSig>,
    late_sig_store: SharedLateFinalizeStore,
    observed_finalizes: BTreeMap<EvidenceKey, Vote>,
    observed_bytes: usize,
    newest_epoch: u64,
    highest_views: BTreeMap<u64, u64>,
}

impl FinalizeVerifyActor {
    pub fn new(
        scheme_provider: HybridSchemeProvider<MinSig>,
        late_sig_store: SharedLateFinalizeStore,
    ) -> (Self, FinalizeVerifyMailbox) {
        let (wake, rx) = mpsc::channel(1);
        let admission = Arc::new(Mutex::new(Admission::default()));
        let mailbox = FinalizeVerifyMailbox {
            admission: admission.clone(),
            wake,
            scheme_provider: scheme_provider.clone(),
        };
        (
            Self {
                admission,
                wake: rx,
                scheme_provider,
                late_sig_store,
                observed_finalizes: BTreeMap::new(),
                observed_bytes: 0,
                newest_epoch: 0,
                highest_views: BTreeMap::new(),
            },
            mailbox,
        )
    }

    fn pop(&self) -> Option<Job> {
        self.admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop()
            .map(|q| q.job)
    }

    pub async fn run(mut self) {
        loop {
            if let Some((epoch, finalize)) = self.pop() {
                self.verify_and_admit(epoch, finalize);
                // At most one vote's crypto work per cooperative scheduling turn.
                tokio::task::yield_now().await;
            } else if self.wake.recv().await.is_none() {
                break;
            }
        }
    }

    pub(crate) fn verify_and_admit(&mut self, epoch: Epoch, finalize: Vote) {
        if finalize.proposal.round.epoch() != epoch || finalize.encode_size() > MAX_VOTE_BYTES {
            return;
        }
        let Some(scheme) = self.scheme_provider.scoped(epoch) else {
            return;
        };
        if finalize.signer().get() as usize >= scheme.participants().len() {
            return;
        }
        let view = finalize.proposal.view().get();
        if epoch.get().saturating_add(1) < self.newest_epoch {
            return;
        }
        if self
            .highest_views
            .get(&epoch.get())
            .is_some_and(|highest| view < highest.saturating_sub(OBSERVED_RETAIN_VIEWS))
        {
            return;
        }
        let mut rng = bls_batch_verification_rng();
        if !finalize.verify(&mut rng, scheme.as_ref(), &Sequential) {
            metrics::counter!("outbe_finalize_verify_verification_total", "result" => "invalid")
                .increment(1);
            return;
        }
        // Only authenticated votes advance retention watermarks.
        self.newest_epoch = self.newest_epoch.max(epoch.get());
        let highest = self.highest_views.entry(epoch.get()).or_default();
        *highest = (*highest).max(view);
        self.highest_views
            .retain(|epoch, _| epoch.saturating_add(1) >= self.newest_epoch);
        self.observed_finalizes.retain(|(epoch, view, _, _), _| {
            self.highest_views
                .get(epoch)
                .is_some_and(|highest| *view >= highest.saturating_sub(OBSERVED_RETAIN_VIEWS))
        });
        self.recount_bytes();
        let key = (
            epoch.get(),
            view,
            finalize.signer().get(),
            keccak256(finalize.proposal.encode()),
        );
        if self.observed_finalizes.contains_key(&key) {
            return;
        }
        if let Some(hybrid_sig) = finalize.attestation.signature.get() {
            if let Ok(mut store) = self.late_sig_store.lock() {
                store.record_bound_individual_vote(
                    crate::finalization::late_sig_store::FinalizeVoteTarget::from_proposal(
                        &finalize.proposal,
                    ),
                    finalize.signer().get(),
                    &hybrid_sig.bls_individual_vote,
                );
            }
        }
        // This evidence is process-local observability. It is not the external
        // watcher's on-chain slashing transport and cannot guarantee completeness.
        let conflicts = self
            .observed_finalizes
            .keys()
            .filter(|existing| (existing.0, existing.1, existing.2) == (key.0, key.1, key.2))
            .count();
        if conflicts < MAX_VERIFIED_CONFLICTS {
            let bytes = finalize.encode_size();
            while self.observed_finalizes.len() >= MAX_OBSERVED_VOTES
                || self.observed_bytes.saturating_add(bytes) > MAX_OBSERVED_BYTES
            {
                if let Some((_, evicted)) = self.observed_finalizes.pop_first() {
                    self.observed_bytes = self.observed_bytes.saturating_sub(evicted.encode_size());
                } else {
                    break;
                }
            }
            self.observed_bytes = self.observed_bytes.saturating_add(bytes);
            self.observed_finalizes.insert(key, finalize);
        }
        metrics::gauge!("outbe_finalize_verify_observed_votes")
            .set(self.observed_finalizes.len() as f64);
        metrics::gauge!("outbe_finalize_verify_observed_bytes").set(self.observed_bytes as f64);
        metrics::counter!("outbe_finalize_verify_verification_total", "result" => "valid")
            .increment(1);
    }

    fn recount_bytes(&mut self) {
        self.observed_bytes = self
            .observed_finalizes
            .values()
            .map(EncodeSize::encode_size)
            .sum();
    }

    #[cfg(test)]
    pub(crate) fn observed_len(&self, view: u64) -> usize {
        self.observed_finalizes
            .keys()
            .filter(|key| key.1 == view)
            .count()
    }
    #[cfg(test)]
    pub(crate) fn try_process_one(&mut self) -> bool {
        if let Some((epoch, finalize)) = self.pop() {
            self.verify_and_admit(epoch, finalize);
            true
        } else {
            false
        }
    }
}
