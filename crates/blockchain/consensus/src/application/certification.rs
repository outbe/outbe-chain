//! Durability barrier for a notarized candidate, independent of local verification.
use crate::{digest::Digest, marshal_types::MarshalMailbox};
use commonware_consensus::{marshal::core::DigestFallback, types::Round};
use commonware_utils::channel::oneshot;

/// Simplex has authenticated the notarization for this exact round and digest.
/// Recover its block even if this node did not verify it before the notarization.
/// A cache entry or an execution verdict alone never authorizes a finalize vote.
pub(super) async fn certify(
    marshal: MarshalMailbox,
    publication: super::publication::ProposalPublication,
    round: Round,
    digest: Digest,
    mut response: oneshot::Sender<bool>,
) {
    let persist = async {
        if let Some(gate) = publication.certification_gate(&marshal, round, digest) {
            if gate.await {
                return true;
            }
        }
        // A missing or abandoned gate is not evidence against the candidate.
        // Recover by exact identity using the authenticated notarization.
        let Ok(block) = marshal
            .subscribe_by_digest(digest, DigestFallback::FetchByRound { round })
            .await
        else {
            return false;
        };
        marshal.certified(round, block).await
    };
    // Closing the response abandons this single-shot request. Real storage
    // failures retain Commonware's fatal policy; shutdown is not a false vote.
    let durable =
        match futures::future::select(Box::pin(persist), Box::pin(response.closed())).await {
            futures::future::Either::Left((durable, _)) => durable,
            futures::future::Either::Right(_) => false,
        };
    if durable {
        let _ = response.send(true);
    }
}
