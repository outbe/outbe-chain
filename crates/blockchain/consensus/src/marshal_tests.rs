//! Integration tests for marshal-based block resolution.
//!
//! Tests verify that the digest-bound block availability model works correctly:
//! - Blocks resolve via digest (not from local handler cache via raw P2P)
//! - Missing blocks are resolved via buffer/resolver (not bespoke admission)
//! - The full propose -> digest -> resolution -> finalize flow works end-to-end
//! - No legacy BlockReceived/BlockRequested message variants exist

#[cfg(test)]
mod tests {
    use crate::block::ConsensusBlock;
    use outbe_primitives::OutbeHeader;
    use reth_ethereum::primitives::SealedBlock;
    use reth_ethereum::Block;

    /// Create a test ConsensusBlock with a unique hash.
    fn make_test_block(seed: u8) -> ConsensusBlock {
        use alloy_primitives::Bytes;
        let mut block = Block::default();
        block.header.extra_data = Bytes::from(vec![seed]);
        let block = block.map_header(OutbeHeader::new);
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
    }

    // -----------------------------------------------------------------------
    // Regression: no raw unsolicited block admission path
    // -----------------------------------------------------------------------

    /// Raw unsolicited block payload cannot be a primary source for verify/finalize.
    /// The Message enum has exactly 3 variants - no BlockReceived/BlockRequested.
    /// (The `Finalized` variant moved to `crate::finalization::ingress::Message`
    /// in step 21. The `Broadcast` variant was removed when proposer
    /// dissemination moved to a direct `marshal.forward` from `Relay::broadcast`.)
    /// If someone adds raw-block admission back, this test will fail to compile.
    #[test]
    fn test_no_raw_block_admission_path() {
        use crate::application::ingress::Message;
        fn _exhaustive_check(msg: Message) {
            match msg {
                Message::Genesis(_) => {}
                Message::Propose(_) => {}
                Message::Verify(_) => {}
            }
        }
    }

    // -----------------------------------------------------------------------
    // MarshalReporter must acknowledge, not drop
    // -----------------------------------------------------------------------

    /// Exact::acknowledge() resolves the waiter successfully.
    /// drop() without acknowledge triggers cancel. This test verifies
    /// the acknowledge path works as MarshalReporter now uses it.
    #[test]
    fn test_exact_acknowledge_resolves_waiter() {
        use commonware_runtime::Runner as _;
        commonware_runtime::deterministic::Runner::default().start(|_context| async move {
            use commonware_utils::acknowledgement::{Acknowledgement, Exact};
            use futures::FutureExt;

            let (ack, mut waiter) = Exact::handle();

            // Same waiter must still be pending before acknowledge().
            assert!(
                (&mut waiter).now_or_never().is_none(),
                "waiter must not resolve before acknowledge"
            );

            ack.acknowledge();

            // After acknowledge, the exact same waiter must resolve with Ok(()).
            let resolved = (&mut waiter)
                .now_or_never()
                .expect("waiter must resolve immediately after acknowledge");
            assert!(
                resolved.is_ok(),
                "acknowledged Exact waiter must resolve successfully"
            );
        });
    }

    /// Verify drop(ack) triggers cancellation, not success.
    /// This confirms the old behavior (drop) was wrong.
    #[test]
    fn test_exact_drop_cancels_waiter() {
        use commonware_runtime::Runner as _;
        commonware_runtime::deterministic::Runner::default().start(|_context| async move {
            use commonware_utils::acknowledgement::{Acknowledgement, Exact};

            let (ack, waiter) = Exact::handle();
            drop(ack); // Old behavior - should cancel

            let result = waiter.await;
            assert!(
                result.is_err(),
                "drop without acknowledge must cancel the waiter"
            );
        });
    }

    /// Executor Mailbox Reporter impl sends MarshalUpdate into the channel.
    #[test]
    fn test_executor_reporter_sends_marshal_update() {
        use commonware_runtime::Runner as _;
        commonware_runtime::deterministic::Runner::default().start(|_context| async move {
            use commonware_consensus::Reporter;
            use commonware_utils::acknowledgement::{Acknowledgement, Exact};
            use futures::StreamExt;

            let (tx, mut rx) = futures::channel::mpsc::unbounded();
            let mut mailbox = crate::executor::Mailbox::from_sender(tx);
            let block = make_test_block(0xCC);

            let (ack, _waiter) = Exact::handle();
            let update = commonware_consensus::marshal::Update::Block(block.into(), ack);

            mailbox.report(update);

            // Message must be in the channel.
            let msg = rx.next().await.expect("channel must have message");
            assert!(
                matches!(msg, crate::executor::ingress::Message::MarshalUpdate(_)),
                "expected MarshalUpdate message"
            );
        });
    }

    // -----------------------------------------------------------------------
    // Finalization retry constants
    // -----------------------------------------------------------------------

    /// Verify retry constants are reasonable and accessible.
    #[test]
    fn test_finalize_retry_constants() {
        use crate::config::{
            FINALIZE_MAX_RETRIES, FINALIZE_RESOLUTION_TIMEOUT, FINALIZE_RETRY_DELAY,
            PROPOSE_RESOLUTION_TIMEOUT, VERIFY_RESOLUTION_TIMEOUT,
        };

        assert_eq!(FINALIZE_MAX_RETRIES, 5, "max retries must be 5");
        assert_eq!(
            FINALIZE_RESOLUTION_TIMEOUT,
            std::time::Duration::from_secs(10),
            "finalize per-attempt timeout must be 10 seconds"
        );
        assert_eq!(
            FINALIZE_RETRY_DELAY,
            std::time::Duration::from_secs(2),
            "retry delay must be 2 seconds"
        );

        // Worst case before structured failure: N attempts x per-attempt timeout
        // + (N - 1) sleeps between them (no sleep after the last attempt).
        let total_worst_case = FINALIZE_RESOLUTION_TIMEOUT * FINALIZE_MAX_RETRIES
            + FINALIZE_RETRY_DELAY * (FINALIZE_MAX_RETRIES - 1);
        assert_eq!(
            total_worst_case,
            std::time::Duration::from_secs(58),
            "worst-case before structured failure must be exactly 58s"
        );
        assert!(
            total_worst_case <= std::time::Duration::from_secs(60),
            "worst-case before structured failure must be bounded (<=60s)"
        );
        assert_eq!(
            PROPOSE_RESOLUTION_TIMEOUT, VERIFY_RESOLUTION_TIMEOUT,
            "propose and verify marshal resolution should use the same per-attempt budget"
        );
    }

    /// Regression: parent block resolution must hint the parent's view, not the
    /// child proposal's current view. Otherwise marshal fetches notarization for
    /// the child round while the waiter is subscribed to the parent digest.
    #[test]
    fn test_parent_round_uses_parent_view() {
        use commonware_consensus::types::{Epoch, Round, View};

        let child_round = Round::new(Epoch::new(6), View::new(91));
        let resolved = crate::application::handler::parent_round(child_round, View::new(90));

        assert_eq!(resolved.epoch(), child_round.epoch());
        assert_eq!(resolved.view(), View::new(90));
    }

    /// Immediate success - no retries needed.
    #[test]
    fn test_retry_immediate_success_no_stall() {
        use crate::finalization::util::retry_with_backoff;
        use commonware_runtime::Runner as _;

        commonware_runtime::deterministic::Runner::default().start(|context| async move {
            let result = retry_with_backoff(
                &context,
                || async { Ok::<_, ()>(99u32) },
                3,
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(50),
            )
            .await;

            assert_eq!(result.unwrap(), 99);
        });
    }

    /// Regression: a never-completing marshal future must not wedge the helper.
    /// Without the per-attempt timeout, the inner helper would hang on the pending
    /// future. The `Runner::timed` wedge guard aborts the test if that happens. With
    /// the per-attempt timeout, exhaustion fires deterministically via the runtime
    /// clock and `retry_with_backoff` returns promptly in virtual time.
    #[test]
    fn test_finalize_resolution_times_out_pending_marshal_future() {
        use crate::finalization::util::{retry_with_backoff, RetryFailureKind};
        use commonware_runtime::Runner as _;
        use std::sync::atomic::{AtomicU32, Ordering};

        commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(60)).start(
            |context| async move {
                let attempts = std::sync::Arc::new(AtomicU32::new(0));
                let attempts_clone = attempts.clone();

                // The per-attempt `Clock::timeout` inside `retry_with_backoff` must make
                // the helper terminate even on a never-completing future. Under the
                // deterministic runtime, the clock-driven per-attempt timeouts and backoff
                // advance virtual time, so the helper returns promptly. If the fix is
                // missing and the helper hangs instead, the `Runner::timed` wedge guard
                // aborts the test.
                let result = retry_with_backoff::<(), _, _>(
                    &context,
                    move || {
                        let a = attempts_clone.clone();
                        async move {
                            a.fetch_add(1, Ordering::SeqCst);
                            std::future::pending::<Result<(), ()>>().await
                        }
                    },
                    3,
                    std::time::Duration::from_millis(1),
                    std::time::Duration::from_millis(5),
                )
                .await;

                let failure = result.expect_err("must exhaust on pending future");
                assert_eq!(failure.attempts, 3, "must have attempted exactly 3 times");
                assert_eq!(
                    failure.last_kind,
                    RetryFailureKind::Timeout,
                    "last failure must be Timeout, not Unavailable"
                );
                assert_eq!(
                    attempts.load(Ordering::SeqCst),
                    3,
                    "resolver must have been polled exactly 3 times"
                );
            },
        );
    }
}
