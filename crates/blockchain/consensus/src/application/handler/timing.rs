use crate::block::ConsensusBlock;

use crate::errors::ApplicationError;

use std::time::Duration;

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
/// `[parent + 1, parent + band]`, with the absent-parent guard.
///
/// When `parent_timestamp_millis == 0`, the proposer did not resolve a parent
/// block. [`proposal_timestamp_millis`] passes zero for that absent parent.
/// Capping at `0 + band` would clamp the real wall-clock time far below the
/// genesis timestamp. Reth would then reject the payload as a past timestamp,
/// stalling at block 0. For a zero parent timestamp the function enforces only
/// monotonicity (`max(now, 1)`).
/// For every resolved parent timestamp (`parent_timestamp_millis > 0`), the
/// band `[parent + 1, parent + band]` applies, using the same bound arithmetic
/// as the validator:
/// - lower bound `parent + 1`: guarantees strict `+1 ms` monotonicity even
///   under local clock lag.
/// - upper bound `parent + band` (C-01): an honest proposer never emits an
///   over-drifted block, and a long stall self-heals by ratcheting forward at
///   most one band per block.
pub(super) fn clamp_proposed_timestamp_millis(
    parent_timestamp_millis: u64,
    now_millis: u64,
    band_millis: u64,
) -> u64 {
    if parent_timestamp_millis == 0 {
        return std::cmp::max(now_millis, 1);
    }
    outbe_primitives::consensus::clamp_timestamp_into_band(
        parent_timestamp_millis,
        now_millis,
        band_millis,
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
) -> u64 {
    let parent_timestamp_millis = parent_block.map_or(0, ConsensusBlock::timestamp_millis);
    clamp_proposed_timestamp_millis(parent_timestamp_millis, now_millis, band_millis)
}

/// Whether the exact parent is ahead of the local proposer clock.
pub(super) fn parent_ahead_of_local_clock(
    parent_block: Option<&ConsensusBlock>,
    now_millis: u64,
) -> bool {
    parent_block.is_some_and(|p| p.timestamp_millis() > 0 && now_millis < p.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            for (now, ahead, expected_clamped) in [
                (2999, true, 3001),
                (3000, false, 3001),
                (3001, false, 3001),
                (3999, false, 3999),
                (4000, false, 4000),
                (63_001, false, 63_000),
            ] {
                assert_eq!(parent_ahead_of_local_clock(Some(&parent), now), ahead);
                assert_eq!(
                    clamp_proposed_timestamp_millis(3000, now, 60_000),
                    expected_clamped
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
