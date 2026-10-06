//! Staged proposals and their durability barriers, shared across Simplex restarts.

use crate::{block::ConsensusBlock, digest::Digest, marshal_types::MarshalMailbox};
use commonware_actor::Feedback;
use commonware_consensus::types::Round;
use commonware_cryptography::bls12381;
use commonware_p2p::Recipients;
use commonware_runtime::{Error, Handle, Spawner};
use commonware_utils::channel::oneshot;
use futures::future::{BoxFuture, FutureExt, Shared};
use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc};

type Durability = Shared<BoxFuture<'static, bool>>;
type Observe = dyn Fn(Durability) + Send + Sync;

struct Staged {
    block: Arc<ConsensusBlock>,
    ack: oneshot::Sender<Handle<()>>,
}

struct Entry {
    staged: Option<Staged>,
    durable: Durability,
}

#[derive(Default)]
struct State {
    finalized: Option<Round>,
    entries: BTreeMap<(Round, Digest), Entry>,
}

impl State {
    fn contains_round(&self, round: Round) -> bool {
        self.entries
            .range((round, Digest::ZERO)..)
            .next()
            .is_some_and(|((candidate_round, _), _)| *candidate_round == round)
            || self.finalized.is_some_and(|finalized| round <= finalized)
    }
}

/// Owns locally built candidates until relay or certification hands them to marshal.
///
/// Register the block and its exact `(round, digest)` barrier before releasing a
/// proposal digest. Response cancellation and Simplex restart do not retire it.
#[derive(Clone)]
pub struct ProposalPublication {
    state: Arc<Mutex<State>>,
    observe: Arc<Observe>,
}

impl ProposalPublication {
    pub fn new<E: Spawner>(context: E) -> Self {
        let context = Arc::new(context);
        Self {
            state: Arc::new(Mutex::new(State::default())),
            // Poll even if no certification is requested: a real sync failure
            // must surface promptly and remain fatal after response cancellation.
            observe: Arc::new(move |durable| {
                context
                    .child("proposal_durability")
                    .spawn(move |_| async move {
                        durable.await;
                    });
            }),
        }
    }

    pub(crate) fn contains_round(&self, round: Round) -> bool {
        self.state.lock().contains_round(round)
    }

    /// Accept at most one locally built candidate per round, including a race
    /// between overlapping proposal tasks. Never replace an outstanding gate.
    pub(crate) fn stage(&self, round: Round, block: ConsensusBlock) -> bool {
        let mut state = self.state.lock();
        if state.contains_round(round) {
            return false;
        }
        let digest = block.digest();
        let (ack, receiver) = oneshot::channel();
        let durable = await_durability(round, receiver).boxed().shared();
        state.entries.insert(
            (round, digest),
            Entry {
                staged: Some(Staged {
                    block: Arc::new(block),
                    ack,
                }),
                durable: durable.clone(),
            },
        );
        drop(state);
        (self.observe)(durable);
        true
    }

    /// Store a received candidate independently of its verification request.
    /// Availability is not an application verdict; certification owns durability.
    pub(crate) fn store_candidate(
        &self,
        marshal: &MarshalMailbox,
        round: Round,
        block: Arc<ConsensusBlock>,
    ) {
        let digest = block.digest();
        let mut state = self.state.lock();
        if state.finalized.is_some_and(|finalized| round <= finalized)
            || state.entries.contains_key(&(round, digest))
        {
            return;
        }
        let (ack, receiver) = oneshot::channel();
        let durable = await_durability(round, receiver).boxed().shared();
        state.entries.insert(
            (round, digest),
            Entry {
                staged: None,
                durable: durable.clone(),
            },
        );
        // Enqueue before publishing the gate to concurrent certification.
        marshal.verified_deferred(round, block, ack);
        drop(state);
        (self.observe)(durable);
    }

    /// First relay consumes the staged block atomically. Later relays use
    /// marshal's digest lookup. Keep the gate until actual finalization.
    pub(crate) fn relay(
        &self,
        marshal: &MarshalMailbox,
        candidate: (Round, Digest),
        recipients: Recipients<bls12381::PublicKey>,
    ) -> Feedback {
        let (round, digest) = candidate;
        let staged = self
            .state
            .lock()
            .entries
            .get_mut(&candidate)
            .and_then(|entry| entry.staged.take());
        match staged {
            Some(Staged { block, ack }) => marshal.proposed(round, block, recipients, ack),
            None => marshal.forward(round, digest, recipients),
        }
    }

    /// Certification also flushes a candidate that was never relayed, so it
    /// cannot wait forever on an acknowledgement that nobody will deliver.
    pub(crate) fn certification_gate(
        &self,
        marshal: &MarshalMailbox,
        round: Round,
        digest: Digest,
    ) -> Option<Durability> {
        let (staged, durable) = {
            let mut state = self.state.lock();
            let entry = state.entries.get_mut(&(round, digest))?;
            (entry.staged.take(), entry.durable.clone())
        };
        if let Some(Staged { block, ack }) = staged {
            marshal.verified_deferred(round, block, ack);
        }
        Some(durable)
    }

    /// A nullified or cancelled view alone is insufficient to retire a gate:
    /// its block can still be notarized later. The caller supplies marshal's
    /// actual finalized round, never an execution height or speculative view.
    pub fn retire_through(&self, round: Round) {
        let mut state = self.state.lock();
        if state.finalized.is_some_and(|finalized| round <= finalized) {
            return;
        }
        state.finalized = Some(round);
        state
            .entries
            .retain(|(candidate_round, _), _| *candidate_round > round);
    }
}

async fn await_durability(round: Round, receiver: oneshot::Receiver<Handle<()>>) -> bool {
    let Ok(handle) = receiver.await else {
        return false;
    };
    match handle.await {
        Ok(()) => true,
        Err(Error::Closed | Error::Aborted) => false,
        Err(error) => panic!("failed to sync proposal at {round}: {error}"),
    }
}

#[cfg(test)]
#[path = "publication_tests.rs"]
pub(crate) mod tests;
