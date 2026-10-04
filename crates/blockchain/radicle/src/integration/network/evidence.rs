use crate::{
    endpoint::{PeerId, SignedEndpointResponse},
    manager::FinalizedSnapshot,
};
use parking_lot::RwLock;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedEndpointEvidence {
    pub peer: PeerId,
    pub response: SignedEndpointResponse,
    pub encoded_frame: Vec<u8>,
}

/// Read-only access to evidence published by the network event loop.
#[derive(Clone, Default)]
pub struct EndpointEvidenceHandle(Arc<RwLock<BTreeMap<PeerId, Arc<SignedEndpointEvidence>>>>);

impl EndpointEvidenceHandle {
    #[must_use]
    pub fn snapshot(&self) -> Vec<SignedEndpointEvidence> {
        self.shared_snapshot()
            .into_iter()
            .map(|proof| (*proof).clone())
            .collect()
    }

    fn shared_snapshot(&self) -> Vec<Arc<SignedEndpointEvidence>> {
        self.0.read().values().cloned().collect()
    }

    pub(super) fn publish(&self, proof: SignedEndpointEvidence) {
        let peer = proof.peer;
        let proof = Arc::new(proof);
        let replaced = self.0.write().insert(peer, proof);
        drop(replaced);
    }

    pub(super) fn prune(&self, snapshot: &FinalizedSnapshot) {
        let stale: Vec<_> = self
            .shared_snapshot()
            .into_iter()
            .filter(|proof| {
                let body = proof.response.body();
                body.valid_until <= snapshot.block.number
                    || !snapshot.validators.iter().any(|validator| {
                        validator.address == body.validator
                            && validator.peer == proof.peer
                            && validator.node_id == Some(body.node_id)
                    })
            })
            .map(|proof| proof.peer)
            .collect();
        let mut removed = Vec::with_capacity(stale.len());
        {
            // The network loop is the only writer, so prepared keys cannot become stale.
            let mut evidence = self.0.write();
            for peer in stale {
                removed.extend(evidence.remove(&peer));
            }
        }
        drop(removed);
    }
}

#[cfg(test)]
mod tests;
