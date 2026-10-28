//! Consensus finalization and ancestor finality proof RPC handlers.

use alloy_primitives::Bytes;
use jsonrpsee::{core::RpcResult, types::ErrorObject};
use outbe_primitives::consensus::{
    ConsensusExecutionBridge, FinalizationRequestError, FINALIZATION_REQUEST_TIMEOUT,
};

use super::{internal_err, invalid_params_err, OutbeApiHandler};
use crate::{
    api::{AncestorFinalizationProof, FinalizationProof},
    errors::{
        RpcInternalError, RpcInvalidParamsError, RpcResourceExhaustedError,
        RESOURCE_EXHAUSTED_ERROR_CODE,
    },
};

/// Maximum descendant offset inspected by `outbe_getFinalityProof`.
const FINALITY_PROOF_MAX_DESCENDANT_OFFSET: u64 = 64;

/// Maximum cumulative raw block bytes collected across `outbe_getFinalityProof`.
///
/// Hex JSON doubles the block payload; 4 MiB of raw bytes stays well below the
/// default 10 MiB upstream HTTP client response bound with envelope room.
const FINALITY_PROOF_MAX_RAW_BLOCK_BYTES: usize = 4 * 1024 * 1024;

fn resource_exhausted_err(err: RpcResourceExhaustedError) -> ErrorObject<'static> {
    ErrorObject::owned(RESOURCE_EXHAUSTED_ERROR_CODE, err.to_string(), None::<()>)
}

fn ensure_height_not_above_finalized(
    bridge: &ConsensusExecutionBridge,
    height: u64,
) -> RpcResult<()> {
    let finalized = bridge.consensus_status().last_finalized_block;
    if height > finalized {
        return Err(invalid_params_err(
            RpcInvalidParamsError::HeightAboveFinalized { height, finalized },
        ));
    }
    Ok(())
}

fn map_finalization_request_error(
    err: FinalizationRequestError,
    not_serving: RpcInternalError,
) -> ErrorObject<'static> {
    match err {
        FinalizationRequestError::NotServing => internal_err(not_serving),
        FinalizationRequestError::Busy => {
            resource_exhausted_err(RpcResourceExhaustedError::FinalizationServiceBusy)
        }
        FinalizationRequestError::TimedOut => {
            resource_exhausted_err(RpcResourceExhaustedError::FinalizationRequestTimedOut)
        }
    }
}

impl<P> OutbeApiHandler<P> {
    pub(crate) async fn get_finalization_for_height(
        &self,
        height: u64,
    ) -> RpcResult<FinalizationProof> {
        // Only nodes running consensus (or a follower that has itself synced the
        // height) can serve this. Both install a finalization fetcher on the
        // bridge at marshal-start. A node without a bridge (pure EL full node)
        // has no marshal and cannot answer.
        let bridge = self
            .bridge
            .as_ref()
            .ok_or_else(|| internal_err(RpcInternalError::NotServingConsensusFinalizations))?;
        ensure_height_not_above_finalized(bridge, height)?;
        let proof = bridge
            .request_finalization(height)
            .await
            .map_err(|err| {
                map_finalization_request_error(
                    err,
                    RpcInternalError::NotServingConsensusFinalizations,
                )
            })?
            .ok_or_else(|| internal_err(RpcInternalError::NoFinalizationAvailable(height)))?;
        if proof.finalization.is_empty() {
            return Err(internal_err(
                RpcInternalError::NoDirectFinalizationAvailable(height),
            ));
        }
        Ok(FinalizationProof {
            finalization_hex: format!("0x{}", hex::encode(&proof.finalization)),
            block_hex: format!("0x{}", hex::encode(&proof.block)),
        })
    }

    pub(crate) async fn get_consensus_block_for_height(&self, height: u64) -> RpcResult<Bytes> {
        let bridge = self
            .bridge
            .as_ref()
            .ok_or_else(|| internal_err(RpcInternalError::NotServingConsensusBlocks))?;
        ensure_height_not_above_finalized(bridge, height)?;
        bridge
            .request_finalization(height)
            .await
            .map_err(|err| {
                map_finalization_request_error(err, RpcInternalError::NotServingConsensusBlocks)
            })?
            .map(|proof| proof.block)
            .ok_or_else(|| {
                internal_err(RpcInternalError::NoFinalizedConsensusBlockAvailable(height))
            })
    }

    pub(crate) async fn finality_proof_for_height(
        &self,
        height: u64,
    ) -> RpcResult<AncestorFinalizationProof> {
        let bridge = self
            .bridge
            .as_ref()
            .ok_or_else(|| internal_err(RpcInternalError::NotServingConsensusFinalizations))?;
        ensure_height_not_above_finalized(bridge, height)?;
        tokio::time::timeout(FINALIZATION_REQUEST_TIMEOUT, async {
            let mut ancestors = Vec::new();
            let mut total_bytes = 0usize;
            for offset in 0..=FINALITY_PROOF_MAX_DESCENDANT_OFFSET {
                let next = height
                    .checked_add(offset)
                    .ok_or_else(|| internal_err(RpcInternalError::ProofHeightOverflow))?;
                let proof = bridge
                    .request_finalization(next)
                    .await
                    .map_err(|err| {
                        map_finalization_request_error(
                            err,
                            RpcInternalError::NotServingConsensusFinalizations,
                        )
                    })?
                    .ok_or_else(|| {
                        internal_err(RpcInternalError::NoFinalityEvidenceAvailable(next))
                    })?;
                total_bytes = total_bytes.saturating_add(proof.block.len());
                if total_bytes > FINALITY_PROOF_MAX_RAW_BLOCK_BYTES {
                    return Err(internal_err(
                        RpcInternalError::AncestorFinalityProofExceedsByteLimit,
                    ));
                }
                if !proof.finalization.is_empty() {
                    return Ok(AncestorFinalizationProof {
                        certified: FinalizationProof {
                            finalization_hex: format!("0x{}", hex::encode(proof.finalization)),
                            block_hex: format!("0x{}", hex::encode(proof.block)),
                        },
                        ancestor_blocks_hex: ancestors,
                    });
                }
                ancestors.push(format!("0x{}", hex::encode(proof.block)));
            }
            Err(internal_err(
                RpcInternalError::NoDescendantFinalizationWithin64Blocks,
            ))
        })
        .await
        .unwrap_or_else(|_| {
            Err(resource_exhausted_err(
                RpcResourceExhaustedError::FinalizationRequestTimedOut,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll, Waker},
        time::Duration,
    };

    use alloy_primitives::Bytes;
    use jsonrpsee::types::error::{INTERNAL_ERROR_CODE, INVALID_PARAMS_CODE};
    use outbe_primitives::consensus::{
        ConsensusExecutionBridge, FinalizedBlockBytes, FINALIZATION_QUEUE_DEPTH,
        FINALIZATION_REQUEST_TIMEOUT,
    };

    use super::*;

    async fn assert_all_three_fail(
        handler: &OutbeApiHandler<()>,
        height: u64,
        expected_code: i32,
        expected_message: &str,
    ) {
        for err in [
            handler
                .get_finalization_for_height(height)
                .await
                .unwrap_err(),
            handler
                .get_consensus_block_for_height(height)
                .await
                .unwrap_err(),
            handler.finality_proof_for_height(height).await.unwrap_err(),
        ] {
            assert_eq!(err.code(), expected_code);
            assert_eq!(err.message(), expected_message);
            assert!(err.data().is_none());
        }
    }

    #[tokio::test]
    async fn rejects_height_above_finalized_across_all_three_endpoints() {
        let bridge = ConsensusExecutionBridge::new();
        bridge.set_last_finalized_block_number(50);
        let mut rx = bridge.set_finalization_fetcher();
        let handler = OutbeApiHandler::with_bridge(Arc::new(()), bridge.clone());

        assert_all_three_fail(
            &handler,
            51,
            INVALID_PARAMS_CODE,
            "height 51 is above the node's finalized height 50",
        )
        .await;
        assert!(
            rx.try_recv().is_err(),
            "above-finalized heights must be rejected before enqueueing"
        );

        // Even when the queue is completely full, height > finalized must return
        // -32602 (input validation precedes queue admission).
        let mut cx = Context::from_waker(Waker::noop());
        let mut queued: Vec<Pin<Box<dyn Future<Output = _>>>> =
            Vec::with_capacity(FINALIZATION_QUEUE_DEPTH);
        for i in 0..FINALIZATION_QUEUE_DEPTH {
            let b = bridge.clone();
            let mut fut = Box::pin(async move { b.request_finalization(i as u64).await });
            assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));
            queued.push(fut);
        }

        let err = handler.get_finalization_for_height(51).await.unwrap_err();
        assert_eq!(err.code(), INVALID_PARAMS_CODE);
        assert_eq!(
            err.message(),
            "height 51 is above the node's finalized height 50"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn busy_and_timed_out_and_not_found_are_distinct() {
        // 1. NotServing when no fetcher is installed or receiver is dropped.
        let unserved_bridge = ConsensusExecutionBridge::new();
        unserved_bridge.set_last_finalized_block_number(20);
        let unserved_handler = OutbeApiHandler::with_bridge(Arc::new(()), unserved_bridge);
        let err = unserved_handler
            .get_finalization_for_height(5)
            .await
            .unwrap_err();
        assert_eq!(err.code(), INTERNAL_ERROR_CODE);
        assert_eq!(err.message(), "node is not serving consensus finalizations");
        let err = unserved_handler
            .get_consensus_block_for_height(5)
            .await
            .unwrap_err();
        assert_eq!(err.code(), INTERNAL_ERROR_CODE);
        assert_eq!(err.message(), "node is not serving consensus blocks");
        let err = unserved_handler
            .finality_proof_for_height(5)
            .await
            .unwrap_err();
        assert_eq!(err.code(), INTERNAL_ERROR_CODE);
        assert_eq!(err.message(), "node is not serving consensus finalizations");

        // 2. Busy (-32005) when all FINALIZATION_QUEUE_DEPTH slots are occupied.
        let busy_bridge = ConsensusExecutionBridge::new();
        busy_bridge.set_last_finalized_block_number(500);
        let _busy_rx = busy_bridge.set_finalization_fetcher();
        let mut cx = Context::from_waker(Waker::noop());
        let mut in_flight: Vec<Pin<Box<dyn Future<Output = _>>>> =
            Vec::with_capacity(FINALIZATION_QUEUE_DEPTH);
        for i in 0..FINALIZATION_QUEUE_DEPTH {
            let b = busy_bridge.clone();
            let mut fut = Box::pin(async move { b.request_finalization(i as u64).await });
            assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));
            in_flight.push(fut);
        }
        let busy_handler = OutbeApiHandler::with_bridge(Arc::new(()), busy_bridge);
        assert_all_three_fail(
            &busy_handler,
            10,
            RESOURCE_EXHAUSTED_ERROR_CODE,
            "finalization service is busy; retry shortly",
        )
        .await;
        drop(in_flight);

        // 3. TimedOut (-32005) when the drainer does not reply within 5s.
        let timeout_bridge = ConsensusExecutionBridge::new();
        timeout_bridge.set_last_finalized_block_number(20);
        let _timeout_rx = timeout_bridge.set_finalization_fetcher();
        let timeout_handler = OutbeApiHandler::with_bridge(Arc::new(()), timeout_bridge);

        assert_all_three_fail(
            &timeout_handler,
            7,
            RESOURCE_EXHAUSTED_ERROR_CODE,
            "finalization request timed out",
        )
        .await;

        // 4. Not found (Ok(None)) maps to -32603 with the endpoint-specific message.
        let none_bridge = ConsensusExecutionBridge::new();
        none_bridge.set_last_finalized_block_number(20);
        let mut none_rx = none_bridge.set_finalization_fetcher();
        tokio::spawn(async move {
            while let Some((_, reply)) = none_rx.recv().await {
                let _ = reply.send(None);
            }
        });
        let none_handler = OutbeApiHandler::with_bridge(Arc::new(()), none_bridge);

        let err = none_handler
            .get_finalization_for_height(9)
            .await
            .unwrap_err();
        assert_eq!(err.code(), INTERNAL_ERROR_CODE);
        assert_eq!(
            err.message(),
            "no finalization available for height 9 (not finalized locally or pruned)"
        );

        let err = none_handler
            .get_consensus_block_for_height(9)
            .await
            .unwrap_err();
        assert_eq!(err.code(), INTERNAL_ERROR_CODE);
        assert_eq!(
            err.message(),
            "no finalized consensus block available for height 9"
        );

        let err = none_handler.finality_proof_for_height(9).await.unwrap_err();
        assert_eq!(err.code(), INTERNAL_ERROR_CODE);
        assert_eq!(err.message(), "no finality evidence available at height 9");
    }

    #[tokio::test]
    async fn available_height_returns_unchanged_success_payloads() {
        let bridge = ConsensusExecutionBridge::new();
        bridge.set_last_finalized_block_number(15);
        let mut rx = bridge.set_finalization_fetcher();
        tokio::spawn(async move {
            while let Some((height, reply)) = rx.recv().await {
                let response = match height {
                    10 => Some(FinalizedBlockBytes {
                        finalization: Bytes::new(),
                        block: Bytes::from_static(&[0x10, 0xaa]),
                    }),
                    11 => Some(FinalizedBlockBytes {
                        finalization: Bytes::from_static(&[0xc1, 0xc2]),
                        block: Bytes::from_static(&[0x11, 0xbb]),
                    }),
                    _ => None,
                };
                let _ = reply.send(response);
            }
        });
        let handler = OutbeApiHandler::with_bridge(Arc::new(()), bridge);

        // Height 10 has an empty direct certificate: rejected by get_finalization,
        // accepted by get_consensus_block, and walked past by finality_proof_for_height.
        let no_direct = handler.get_finalization_for_height(10).await.unwrap_err();
        assert_eq!(no_direct.code(), INTERNAL_ERROR_CODE);
        assert_eq!(
            no_direct.message(),
            "no direct finalization available for height 10"
        );

        let block_10 = handler.get_consensus_block_for_height(10).await.unwrap();
        assert_eq!(block_10, Bytes::from_static(&[0x10, 0xaa]));

        let direct_11 = handler.get_finalization_for_height(11).await.unwrap();
        assert_eq!(direct_11.finalization_hex, "0xc1c2");
        assert_eq!(direct_11.block_hex, "0x11bb");

        let ancestor_proof = handler.finality_proof_for_height(10).await.unwrap();
        assert_eq!(ancestor_proof.certified.finalization_hex, "0xc1c2");
        assert_eq!(ancestor_proof.certified.block_hex, "0x11bb");
        assert_eq!(ancestor_proof.ancestor_blocks_hex, vec!["0x10aa"]);
    }

    #[tokio::test]
    async fn finality_proof_succeeds_when_marshal_has_certified_descendant_above_lagging_bridge_status(
    ) {
        let bridge = ConsensusExecutionBridge::new();
        // Bridge still advertises 20 while Marshal already holds certified descendant 21.
        bridge.set_last_finalized_block_number(20);
        let mut rx = bridge.set_finalization_fetcher();
        tokio::spawn(async move {
            while let Some((height, reply)) = rx.recv().await {
                let response = match height {
                    20 => Some(FinalizedBlockBytes {
                        finalization: Bytes::new(),
                        block: Bytes::from_static(&[0x20]),
                    }),
                    21 => Some(FinalizedBlockBytes {
                        finalization: Bytes::from_static(&[0xf1]),
                        block: Bytes::from_static(&[0x21]),
                    }),
                    _ => None,
                };
                let _ = reply.send(response);
            }
        });
        let handler = OutbeApiHandler::with_bridge(Arc::new(()), bridge);

        let proof = handler.finality_proof_for_height(20).await.unwrap();
        assert_eq!(proof.ancestor_blocks_hex, vec!["0x20"]);
        assert_eq!(proof.certified.finalization_hex, "0xf1");
        assert_eq!(proof.certified.block_hex, "0x21");
    }

    #[tokio::test(start_paused = true)]
    async fn finality_proof_enforces_total_walk_deadline() {
        let bridge = ConsensusExecutionBridge::new();
        bridge.set_last_finalized_block_number(200);
        let mut rx = bridge.set_finalization_fetcher();
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_responder = Arc::clone(&seen);

        tokio::spawn(async move {
            while let Some((_, reply)) = rx.recv().await {
                seen_responder.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // Each individual hop completes in 1s (< 5s per-request timeout),
                // but the cumulative walk exceeds the 5s total budget.
                tokio::time::sleep(Duration::from_secs(1)).await;
                let _ = reply.send(Some(FinalizedBlockBytes {
                    finalization: Bytes::new(),
                    block: Bytes::from_static(&[0xaa]),
                }));
            }
        });

        let handler = OutbeApiHandler::with_bridge(Arc::new(()), bridge);
        let err = handler.finality_proof_for_height(100).await.unwrap_err();
        assert_eq!(err.code(), RESOURCE_EXHAUSTED_ERROR_CODE);
        assert_eq!(err.message(), "finalization request timed out");
        let requests_issued = seen.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            requests_issued <= 6,
            "walk must abort at the 5s total budget (saw {requests_issued} requests, expected <= 6)"
        );

        // Advance time further and verify no more requests are sent after the outer timeout aborts.
        tokio::time::advance(FINALIZATION_REQUEST_TIMEOUT).await;
        tokio::task::yield_now().await;
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            requests_issued
        );
    }
}
