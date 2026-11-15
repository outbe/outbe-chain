use crate::block::ConsensusBlock;

use crate::digest::Digest;
use crate::errors::ApplicationError;

use commonware_utils::channel::oneshot;

use std::time::Duration;
use tracing::debug;

/// Delay between Engine API retries while execution reports temporary SYNCING.
pub(crate) const VERIFY_SYNCING_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Log-rate window for repeated critical proposal failures.
pub(crate) const PROPOSAL_FAILURE_LOG_WINDOW: Duration = Duration::from_secs(5);

/// epoch boundary: bounded wait inside `handle_genesis` for the
/// finalization view to expose a continuity anchor for the new epoch.
///
/// Simplex does not query `Automaton::genesis`. The genesis digest feeds
/// `simplex::Config.floor`. No production code sends `Message::Genesis`, so
/// only tests reach `handle_genesis` and this wait. The epoch-restart anchor
/// wait in `outbe-engine` uses its own timeout and poll interval.
pub(crate) const GENESIS_ANCHOR_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll interval used by the bounded wait in `handle_genesis`.
pub(crate) const GENESIS_ANCHOR_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Explicit source of proposer wall-clock time.
///
/// Production injects [`SystemUnixTimeSource`]. Localnet tests may inject an
/// [`OffsetUnixTimeSource`] through the explicitly testnet-scoped CLI option.
/// Consensus code never consults ambient process environment for logical time.
pub trait UnixTimeSource: Send + Sync {
    fn now_millis(&self) -> eyre::Result<u64>;
}

#[derive(Debug, Default)]
pub struct SystemUnixTimeSource;

impl UnixTimeSource for SystemUnixTimeSource {
    fn now_millis(&self) -> eyre::Result<u64> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ApplicationError::ClockBeforeUnixEpoch)?
            .as_millis()
            .try_into()
            .map_err(|_| ApplicationError::ClockMillisOverflow.into())
    }
}

#[derive(Debug)]
pub struct OffsetUnixTimeSource {
    base: SystemUnixTimeSource,
    offset_secs: i64,
}

impl OffsetUnixTimeSource {
    #[must_use]
    pub const fn new(offset_secs: i64) -> Self {
        Self {
            base: SystemUnixTimeSource,
            offset_secs,
        }
    }
}

impl UnixTimeSource for OffsetUnixTimeSource {
    fn now_millis(&self) -> eyre::Result<u64> {
        apply_unix_time_offset_millis(self.base.now_millis()?, self.offset_secs)
    }
}

pub(super) fn apply_unix_time_offset_millis(now: u64, offset_secs: i64) -> eyre::Result<u64> {
    let shifted = i128::from(now) + i128::from(offset_secs) * 1_000;
    u64::try_from(shifted).map_err(|_| ApplicationError::UnixTimeOffsetOverflow(offset_secs).into())
}

/// Clamp a proposer's block timestamp (ms) into the deterministic drift band
/// `[parent + min_advance, parent + band]`, with the genesis-child exception.
///
/// When `parent_timestamp_millis == 0`, the proposer did not resolve a parent
/// block. [`proposal_timestamp_millis`] passes zero for that absent parent. It
/// does not pass the genesis header timestamp, and it is not the validator
/// exemption for parent block number 0. A zero parent timestamp, a parent
/// number of 0, and an absent parent are different inputs. Capping at `0 + band`
/// would clamp the real wall-clock time far below the genesis timestamp. Reth
/// would then reject the payload as a past timestamp, stalling at block 0. For
/// a zero parent timestamp the function enforces only monotonicity (`max(now, parent + 1)`).
/// For every later parent timestamp the full two-sided band applies, using the
/// same bound arithmetic as the validator:
/// - lower bound `parent + min_advance`: if the proposer's clock has not
///   advanced `min_advance` past the parent, the function clamps the timestamp
///   *up*. The block then still satisfies the validator minimum-advance rule and
///   is never rejected. This denies a colluding leader majority the
///   `parent + 1 ms` timestamp freeze.
/// - upper bound `parent + band` (C-01): an honest proposer never emits an
///   over-drifted block, and a long stall self-heals by ratcheting forward at
///   most one band per block.
pub(super) fn clamp_proposed_timestamp_millis(
    parent_timestamp_millis: u64,
    now_millis: u64,
    band_millis: u64,
    min_advance_millis: u64,
) -> u64 {
    if parent_timestamp_millis == 0 {
        return std::cmp::max(now_millis, parent_timestamp_millis.saturating_add(1));
    }
    outbe_primitives::consensus::clamp_timestamp_into_band(
        parent_timestamp_millis,
        now_millis,
        band_millis,
        min_advance_millis,
    )
}

/// Derive the proposal timestamp from the exact resolved parent block.
///
/// A missing parent is the existing genesis-child case. For every later block,
/// the parent header is the sole source of the timestamp band: process-local
/// observations from earlier build attempts cannot move it.
pub(super) fn proposal_timestamp_millis(
    parent_block: Option<&ConsensusBlock>,
    now_millis: u64,
    band_millis: u64,
    min_advance_millis: u64,
) -> u64 {
    let parent_timestamp_millis = parent_block.map_or(0, ConsensusBlock::timestamp_millis);
    clamp_proposed_timestamp_millis(
        parent_timestamp_millis,
        now_millis,
        band_millis,
        min_advance_millis,
    )
}

/// Whether the exact parent is ahead of the local proposer clock.
pub(super) fn parent_ahead_of_local_clock(
    parent_block: Option<&ConsensusBlock>,
    now_millis: u64,
) -> bool {
    parent_block.is_some_and(|p| p.timestamp_millis() > 0 && now_millis < p.timestamp_millis())
}

pub(super) struct ProposalPacing {
    pub(super) min_block_time: Duration,
    pub(super) propose_start: std::time::SystemTime,
    pub(super) resolve_pad: Duration,
}

/// Pure min-block-time floor arithmetic: remaining pad = `min - elapsed`
/// (`saturating_sub`). A zero result means the floor is already met. Send the
/// digest immediately with no wait (case C / heavy block).
pub(super) fn floor_remaining(
    min_block_time: std::time::Duration,
    elapsed: std::time::Duration,
) -> std::time::Duration {
    min_block_time.saturating_sub(elapsed)
}

/// Proposer-side minimum block-time pacing.
///
/// Holds the already-sealed `digest` until the floor (`min_block_time`) elapses,
/// then hands it to Simplex via `response`. If the view is cancelled first
/// (Simplex drops the proposal receiver), the `select!` aborts on
/// `response.closed()` and nothing is sent. This is liveness pacing only. It never
/// touches block bytes/hash/validation, so it is invisible to validators.
///
/// `propose_start` is the closure-level instant captured before `handle_propose`.
/// `elapsed` therefore subsumes the whole build + marshal path. This makes the
/// floor a total ceiling (`max(floor, build)`), not an additive delay.
pub(super) async fn pace_and_send<C>(
    ctx: &C,
    mut response: oneshot::Sender<Digest>,
    digest: Digest,
    pacing: ProposalPacing,
) -> bool
where
    C: commonware_runtime::Clock,
{
    let elapsed = ctx
        .current()
        .duration_since(pacing.propose_start)
        .unwrap_or_default();
    let remaining = floor_remaining(pacing.min_block_time, elapsed);
    crate::metrics::record_block_build_time(elapsed.saturating_sub(pacing.resolve_pad));
    crate::metrics::record_block_wait_time(remaining);
    if remaining.is_zero() {
        // Case C (elapsed >= min): send now, control-flow identical to pre-pacing.
        response.send(digest).is_ok()
    } else {
        // Biased select!: cancellation wins over a near-simultaneous sleep
        // completion, so we never send into a closed channel.
        commonware_macros::select! {
            () = response.closed() => {
                debug!("view cancelled during min-block-time pacing; dropping proposal");
                false
            },
            _ = ctx.sleep(remaining) => {
                response.send(digest).is_ok()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use commonware_runtime::{Clock as _, Runner as _};

    /// Test 5: with a held receiver and a non-trivial floor, `pace_and_send` waits
    /// ~`remaining` of virtual time and then delivers the digest.
    #[test]
    fn floor_sends_after_remainder() {
        let (received, waited) = commonware_runtime::deterministic::Runner::timed(
            Duration::from_secs(30),
        )
        .start(|context| async move {
            let (tx, rx) = oneshot::channel::<Digest>();
            let t0 = context.current();
            assert!(
                super::pace_and_send(
                    &context,
                    tx,
                    Digest(B256::ZERO),
                    ProposalPacing {
                        min_block_time: Duration::from_millis(2000),
                        propose_start: t0,
                        resolve_pad: Duration::ZERO,
                    },
                )
                .await
            );
            let waited = context.current().duration_since(t0).unwrap_or_default();
            (rx.await.ok(), waited)
        });
        assert_eq!(received, Some(Digest(B256::ZERO)));
        assert!(
            waited >= Duration::from_millis(2000),
            "expected to wait the floor, waited {waited:?}"
        );
    }

    /// Test 6 (the `select!` guard): if the proposal receiver is dropped (view
    /// cancelled), `pace_and_send` aborts promptly via `response.closed()` and does
    /// not sleep the full floor.
    #[test]
    fn floor_aborts_on_closed_receiver() {
        let waited = commonware_runtime::deterministic::Runner::timed(Duration::from_secs(30))
            .start(|context| async move {
                let (tx, rx) = oneshot::channel::<Digest>();
                drop(rx); // Simplex dropped the proposal receiver on a view change.
                let t0 = context.current();
                assert!(
                    !super::pace_and_send(
                        &context,
                        tx,
                        Digest(B256::ZERO),
                        ProposalPacing {
                            min_block_time: Duration::from_millis(2000),
                            propose_start: t0,
                            resolve_pad: Duration::ZERO,
                        },
                    )
                    .await
                );
                context.current().duration_since(t0).unwrap_or_default()
            });
        assert!(
            waited < Duration::from_millis(2000),
            "aborted pacing must not sleep the full floor, waited {waited:?}"
        );
    }

    /// Test 7: zero remaining (case C / floor already met) sends immediately with no
    /// sleep - control-flow identical to pre-pacing behavior.
    #[test]
    fn floor_zero_sends_immediately() {
        let (received, waited) = commonware_runtime::deterministic::Runner::timed(
            Duration::from_secs(30),
        )
        .start(|context| async move {
            let (tx, rx) = oneshot::channel::<Digest>();
            let t0 = context.current();
            assert!(
                super::pace_and_send(
                    &context,
                    tx,
                    Digest(B256::ZERO),
                    ProposalPacing {
                        min_block_time: Duration::ZERO,
                        propose_start: t0,
                        resolve_pad: Duration::ZERO
                    }
                )
                .await
            );
            let waited = context.current().duration_since(t0).unwrap_or_default();
            (rx.await.ok(), waited)
        });
        assert_eq!(received, Some(Digest(B256::ZERO)));
        assert_eq!(waited, Duration::ZERO);
    }

    #[test]
    fn active_build_excludes_resolve_pad_without_changing_the_floor() {
        use metrics_util::debugging::{DebugValue, DebuggingRecorder};
        for (elapsed_ms, pad_ms, active_ms, wait_ms) in [(500, 200, 300, 500), (100, 200, 0, 900)] {
            let recorder = DebuggingRecorder::new();
            metrics::with_local_recorder(&recorder, || {
                commonware_runtime::deterministic::Runner::default().start(|clock| async move {
                    let start = clock.current();
                    clock.sleep(Duration::from_millis(elapsed_ms)).await;
                    let (response, receiver) = oneshot::channel();
                    assert!(
                        pace_and_send(
                            &clock,
                            response,
                            Digest::ZERO,
                            ProposalPacing {
                                min_block_time: Duration::from_millis(1000),
                                propose_start: start,
                                resolve_pad: Duration::from_millis(pad_ms),
                            }
                        )
                        .await
                    );
                    assert_eq!(receiver.await.unwrap(), Digest::ZERO);
                    assert_eq!(
                        clock.current().duration_since(start).unwrap(),
                        Duration::from_millis(1000)
                    );
                });
            });
            let snapshot = recorder.snapshotter().snapshot().into_vec();
            assert_eq!(snapshot.len(), 2);
            for (key, _, _, value) in snapshot {
                let expected = match key.key().name() {
                    "outbe_consensus_block_build_duration_ms" => active_ms,
                    "outbe_consensus_block_wait_duration_ms" => wait_ms,
                    name => panic!("unexpected metric: {name}"),
                };
                assert_eq!(value, DebugValue::Histogram(vec![(expected as f64).into()]));
            }
        }
    }

    #[test]
    fn pacing_reports_failed_delivery_even_without_a_floor() {
        commonware_runtime::deterministic::Runner::default().start(|clock| async move {
            let (response, receiver) = oneshot::channel();
            drop(receiver);
            assert!(
                !pace_and_send(
                    &clock,
                    response,
                    Digest::ZERO,
                    ProposalPacing {
                        min_block_time: Duration::ZERO,
                        propose_start: clock.current(),
                        resolve_pad: Duration::ZERO,
                    }
                )
                .await
            );
        });
    }

    #[test]
    fn parent_clock_comparison_is_strict_and_timestamp_clamping_stays_pure() {
        use metrics_util::debugging::DebuggingRecorder;
        let recorder = DebuggingRecorder::new();
        metrics::with_local_recorder(&recorder, || {
            let mut block = crate::test_fixtures::block_with_number(1)
                .into_inner()
                .into_block();
            block.header.inner.timestamp = 3;
            let parent = ConsensusBlock::from_sealed(
                reth_ethereum::primitives::SealedBlock::seal_slow(block),
            );
            assert_eq!(parent.timestamp_millis(), 3000);
            for (now, ahead) in [
                (2999, true),
                (3000, false),
                (3001, false),
                (3999, false),
                (4000, false),
            ] {
                assert_eq!(parent_ahead_of_local_clock(Some(&parent), now), ahead);
                assert_eq!(
                    clamp_proposed_timestamp_millis(3000, now, 60_000, 1000),
                    4000
                );
            }
            assert!(!parent_ahead_of_local_clock(None, 0));
            let mut block = crate::test_fixtures::block_with_number(1)
                .into_inner()
                .into_block();
            block.header.inner.timestamp = 0;
            let zero_parent = ConsensusBlock::from_sealed(
                reth_ethereum::primitives::SealedBlock::seal_slow(block),
            );
            assert!(!parent_ahead_of_local_clock(Some(&zero_parent), 0));
        });
        assert!(recorder.snapshotter().snapshot().into_vec().is_empty());
    }
}
