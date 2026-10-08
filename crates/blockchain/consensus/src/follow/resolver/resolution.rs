//! Block acquisition, proof authentication and awaited marshal deliveries.
use super::*;
use bytes::Bytes;
use commonware_codec::Encode as _;
use commonware_resolver::Consumer;

/// Largest value the follower delivers to its marshal: the validator p2p
/// message cap, so a follower never accepts a value a validator peer could not
/// send. The size is checked from `encode_size` before any value is encoded.
pub(in crate::follow) const MAX_DELIVERY_BYTES: usize =
    crate::config::MAX_P2P_MESSAGE_SIZE as usize;

/// Largest encoded finality-proof bundle (anchor finalization + anchor block +
/// every ancestor block) one fetch may process. Each delivery is also capped by
/// [`MAX_DELIVERY_BYTES`]; this bounds the sum, so many individually small
/// ancestors cannot add up past it. Checked from `encode_size` before the
/// proof is authenticated, mutates follower history or is encoded.
pub(in crate::follow) const MAX_PROOF_BUNDLE_BYTES: usize = 10 * 1024 * 1024;

/// `true` when a value of `size` bytes may be encoded and delivered.
fn deliverable(key: &ResolverKey, size: usize) -> bool {
    if size <= MAX_DELIVERY_BYTES {
        return true;
    }
    warn!(%key, size, limit = MAX_DELIVERY_BYTES, "follower value exceeds the delivery cap; dropping fetch");
    false
}

pub(in crate::follow) struct FetchResolution<F, L> {
    pub(in crate::follow) upstream: F,
    pub(in crate::follow) local: L,
    pub(in crate::follow) chain: SharedCommitteeChain,
    pub(in crate::follow) epocher: FollowerEpocher,
}

impl<F: Clone, L: Clone> Clone for FetchResolution<F, L> {
    fn clone(&self) -> Self {
        Self {
            upstream: self.upstream.clone(),
            local: self.local.clone(),
            chain: self.chain.clone(),
            epocher: self.epocher.clone(),
        }
    }
}

impl<F: FinalizedSource, L: LocalBlockSource> FetchResolution<F, L> {
    /// Test helper using the same acquisition and marshal delivery as opaque.
    #[cfg(test)]
    pub(in crate::follow) async fn resolve<C>(
        self,
        fetch: Fetch<ResolverKey, Annotation>,
        mut handler: C,
    ) where
        C: Consumer<Key = ResolverKey, Value = Bytes, Subscriber = Annotation, Outcome = bool>,
    {
        let Fetch {
            key,
            subscriber,
            span,
        } = fetch;
        let bound = BoundKey::new(key, &subscriber);
        if let Some(Fetched::Value(value)) = self.acquire(bound, &span, &mut handler).await {
            let _ = handler
                .deliver(
                    Delivery {
                        key,
                        subscribers: NonEmptyVec::new((subscriber, span)),
                    },
                    value,
                )
                .await;
        }
    }

    /// Application-specific acquisition only. Opaque owns request lifetime and retries.
    pub(super) async fn acquire<C>(
        self,
        request: BoundKey,
        span: &tracing::Span,
        handler: &mut C,
    ) -> Option<Fetched>
    where
        C: Consumer<Key = ResolverKey, Value = Bytes, Subscriber = Annotation, Outcome = bool>,
    {
        match request.key {
            Key::Block(commitment) => self
                .block(commitment, request.height)
                .await
                .map(Fetched::Value),
            Key::Finalized { height } => self.finalized(height, span, handler).await,
            Key::Notarized { .. } => std::future::pending().await,
        }
    }
    async fn block(&self, commitment: Digest, height: Option<Height>) -> Option<Bytes> {
        let key = Key::Block(commitment);
        // A local block is left for the marshal to validate, as before.
        if let Some(block) = self.local.get_block_by_digest(commitment).await {
            return deliverable(&key, block.encode_size()).then(|| block.encode());
        }
        let Some(height) = height else {
            // Round-bound requests carry no height. The follower cannot map them upstream.
            debug!(%key, "block request without a height annotation; dropping fetch");
            return None;
        };
        // A height-bound gap-repair block must match both height and commitment.
        match self.upstream.get_block(height).await {
            Some(block) if block.number() == height.get() && block.digest() == commitment => {
                deliverable(&key, block.encode_size()).then(|| block.encode())
            }
            Some(_) => {
                debug!(%key, %height, "upstream block at height did not match requested commitment; dropping fetch");
                None
            }
            None => {
                debug!(%key, %height, "upstream did not have requested block; dropping fetch");
                None
            }
        }
    }

    async fn finalized<C>(
        &self,
        height: Height,
        span: &tracing::Span,
        handler: &mut C,
    ) -> Option<Fetched>
    where
        C: Consumer<Key = ResolverKey, Value = Bytes, Subscriber = Annotation, Outcome = bool>,
    {
        let key: ResolverKey = Key::Finalized { height };
        let Some(proof) = self.upstream.get_finality_proof(height).await else {
            debug!(%key, "upstream did not have finality proof; dropping fetch");
            return None;
        };
        let certified = &proof.certified;
        let anchor_size = certified
            .finalization
            .encode_size()
            .saturating_add(certified.block.encode_size());
        if !proof_within_budget(&key, anchor_size, &proof.ancestors) {
            return None;
        }
        if let Err(error) = super::super::engine::authenticate_ancestor_proof(
            &self.chain,
            &self.epocher,
            height,
            &proof,
        ) {
            warn!(%key, %error, "failed to authenticate follower finality proof; dropping fetch");
            return None;
        }
        let mut buf = Vec::with_capacity(anchor_size);
        buf.extend_from_slice(certified.finalization.encode().as_ref());
        buf.extend_from_slice(certified.block.encode().as_ref());
        if proof.ancestors.is_empty() {
            return Some(Fetched::Value(buf.into()));
        }
        let mut delivery = MarshalDelivery { handler, span };
        let certified_height = Height::new(certified.block.number());
        if !delivery
            .at_height(
                Key::Finalized {
                    height: certified_height,
                },
                certified_height,
                buf.into(),
            )
            .await
        {
            return None;
        }
        for block in proof.ancestors.iter().rev() {
            if !delivery
                .at_height(
                    Key::Block(block.digest()),
                    Height::new(block.number()),
                    block.encode(),
                )
                .await
            {
                return None;
            }
        }
        Some(Fetched::Delivered)
    }
}
/// Checks every delivery of a finality-proof bundle and the bundle total
/// against their encoded-byte caps.
fn proof_within_budget(
    key: &ResolverKey,
    anchor_size: usize,
    ancestors: &[crate::block::ConsensusBlock],
) -> bool {
    if !deliverable(key, anchor_size) {
        return false;
    }
    let mut total = anchor_size;
    for block in ancestors {
        let size = block.encode_size();
        if !deliverable(&Key::Block(block.digest()), size) {
            return false;
        }
        total = total.saturating_add(size);
        if total > MAX_PROOF_BUNDLE_BYTES {
            warn!(%key, limit = MAX_PROOF_BUNDLE_BYTES, "follower finality proof exceeds the bundle cap; dropping fetch");
            return false;
        }
    }
    true
}

struct MarshalDelivery<'a, C> {
    handler: &'a mut C,
    span: &'a tracing::Span,
}
impl<C> MarshalDelivery<'_, C>
where
    C: Consumer<Key = ResolverKey, Value = Bytes, Subscriber = Annotation, Outcome = bool>,
{
    async fn at_height(&mut self, key: ResolverKey, height: Height, value: Bytes) -> bool {
        let delivery = Delivery {
            key,
            subscribers: NonEmptyVec::new((
                Annotation::Finalized(handler::Finalized::ByHeight { height }),
                self.span.clone(),
            )),
        };
        matches!(self.handler.deliver(delivery, value).await, Ok(true))
    }
}
