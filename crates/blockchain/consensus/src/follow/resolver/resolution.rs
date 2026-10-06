//! Block acquisition, proof authentication and awaited marshal deliveries.
use super::*;
use bytes::Bytes;
use commonware_resolver::Consumer;

pub(in crate::follow) struct FetchResolution<F, L> {
    pub(in crate::follow) upstream: F,
    pub(in crate::follow) local: L,
    pub(in crate::follow) chain: SharedCommitteeChain,
    pub(in crate::follow) epocher: FollowerEpocher,
}
impl<F: FinalizedSource, L: LocalBlockSource> FetchResolution<F, L> {
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
        debug!(%key, "resolver received fetch");
        let value = match &key {
            Key::Block(commitment) => self.block(*commitment, &subscriber).await,
            Key::Finalized { height } => self.finalized(*height, &span, &mut handler).await,
            Key::Notarized { .. } => {
                debug!(%key, "ignoring notarized backfill request (follower)");
                None
            }
        };
        let Some(value) = value else {
            return;
        };
        let delivery = Delivery {
            key,
            subscribers: NonEmptyVec::new((subscriber, span)),
        };
        // AWAIT the marshal's validation response. Dropping the returned receiver is
        // the resolver-protocol CANCELLATION signal: the marshal checks
        // `response.is_closed()` at dequeue and silently skips a delivery whose
        // receiver is gone (see `handler::Message::response_closed`). Since this
        // fetch runs on its own spawned task, holding the receiver open until the
        // marshal answers costs nothing. The answer tells us whether the marshal
        // accepted the value. We do not retry on rejection (the marshal re-requests
        // if it still needs the height).
        match handler.deliver(delivery, value).await {
            Ok(true) => debug!(%key, "delivery accepted by marshal"),
            Ok(false) => warn!(%key, "delivery rejected by marshal"),
            Err(_) => debug!(%key, "marshal dropped delivery response (shutdown or batch prune)"),
        }
    }
    async fn block(&self, commitment: Digest, subscriber: &Annotation) -> Option<Bytes> {
        let key = Key::Block(commitment);
        // A local block is left for the marshal to validate, as before.
        if let Some(block) = self.local.get_block_by_digest(commitment).await {
            return Some(block.encode());
        }
        let Some(height) = block_request_height(subscriber) else {
            // Round-bound requests carry no height. The follower cannot map them upstream.
            debug!(%key, "block request without a height annotation; dropping fetch");
            return None;
        };
        // A height-bound gap-repair block must match both height and commitment.
        match self.upstream.get_block(height).await {
            Some(block) if block.number() == height.get() && block.digest() == commitment => {
                Some(block.encode())
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
    ) -> Option<Bytes>
    where
        C: Consumer<Key = ResolverKey, Value = Bytes, Subscriber = Annotation, Outcome = bool>,
    {
        let key: ResolverKey = Key::Finalized { height };
        let Some(proof) = self.upstream.get_finality_proof(height).await else {
            debug!(%key, "upstream did not have finality proof; dropping fetch");
            return None;
        };
        if let Err(error) = super::super::engine::authenticate_ancestor_proof(
            &self.chain,
            &self.epocher,
            height,
            &proof,
        ) {
            warn!(%key, %error, "failed to authenticate follower finality proof; dropping fetch");
            return None;
        }
        let certified = &proof.certified;
        let mut buf = certified.finalization.encode().to_vec();
        buf.extend_from_slice(certified.block.encode().as_ref());
        if proof.ancestors.is_empty() {
            return Some(buf.into());
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
        None
    }
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
