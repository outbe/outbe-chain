use crate::block::ConsensusBlock;

use crate::digest::Digest;

use commonware_utils::channel::oneshot;

use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_primitives::projection::ProjectionReadinessHandle;
use outbe_primitives::projection::WaitOutcome;

use std::time::Duration;
use tracing::debug;

/// Delay between Engine API retries while execution reports temporary SYNCING.
pub(crate) const VERIFY_SYNCING_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Log-rate window for repeated critical proposal failures.
pub(crate) const PROPOSAL_FAILURE_LOG_WINDOW: Duration = Duration::from_secs(5);

/// epoch boundary: bounded wait inside `handle_genesis` for the
/// finalization view to expose a continuity anchor for the new epoch.
///
/// If Commonware Simplex queries `Automaton::genesis(epoch>0)` faster than the
/// finalization actor publishes the boundary block's anchor into
/// `FinalizationView`, we wait up to this deadline before declaring the
/// terminal failure path. The companion `stack.rs` pre-restart guard should
/// normally make sure this never trips in practice.
pub(crate) const GENESIS_ANCHOR_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll interval used by the bounded waits in `handle_genesis` and the
/// `stack.rs` pre-restart preconditions.
pub(crate) const GENESIS_ANCHOR_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Explicit source of proposer wall-clock time.
///
/// Production injects [`SystemUnixTimeSource`]. Localnet tests may inject an
/// [`OffsetUnixTimeSource`] through the explicitly testnet-scoped CLI option;
/// consensus code never consults ambient process environment for logical time.
pub trait UnixTimeSource: Send + Sync {
    fn now_millis(&self) -> eyre::Result<u64>;
}

#[derive(Debug, Default)]
pub struct SystemUnixTimeSource;

impl UnixTimeSource for SystemUnixTimeSource {
    fn now_millis(&self) -> eyre::Result<u64> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| eyre::eyre!("system clock before UNIX_EPOCH: {e}"))?
            .as_millis()
            .try_into()
            .map_err(|_| eyre::eyre!("system clock millis does not fit in u64"))
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
    u64::try_from(shifted)
        .map_err(|_| eyre::eyre!("Unix time offset {offset_secs} moves timestamp outside u64"))
}

/// Clamp a proposer's block timestamp (ms) into the deterministic drift band
/// `[parent + min_advance, parent + band]`, with the genesis-child exception.
///
/// When `parent_timestamp_millis == 0` there is no finalized parent yet (the
/// `finalization_view` is unseeded at genesis - it does NOT carry the genesis
/// header timestamp), so the band is meaningless: capping at `0 + band` would
/// clamp the real wall-clock time far below the genesis timestamp and reth
/// would reject the payload as a past timestamp, stalling at block 0. In that
/// case only monotonicity is enforced (`max(now, parent + 1)`), mirroring the
/// validator-side genesis exemption (`parent.number() == 0`). For every real
/// parent the full two-sided band applies, mirroring the validator-side
/// `validate_against_parent_timestamp_millis`:
/// - lower bound `parent + min_advance`: if the proposer's clock has not
///   advanced `min_advance` past the parent, the timestamp is clamped *up* so
///   the block still satisfies the validator minimum-advance rule and is never
///   rejected; this is what denies a colluding leader majority the
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
    let min_timestamp_millis = parent_timestamp_millis.saturating_add(min_advance_millis);
    let max_timestamp_millis = parent_timestamp_millis.saturating_add(band_millis);
    std::cmp::max(now_millis, min_timestamp_millis).min(max_timestamp_millis)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ParentProjectionGate {
    Ready,
    Withhold,
}

pub(super) async fn wait_for_projected_parent<F>(
    readiness: ProjectionReadinessHandle,
    required: ProjectionCheckpoint,
    budget_expired: F,
) -> eyre::Result<ParentProjectionGate>
where
    F: std::future::Future<Output = ()>,
{
    match readiness.wait_for(required, budget_expired).await {
        WaitOutcome::Ready => Ok(ParentProjectionGate::Ready),
        WaitOutcome::BudgetExpired | WaitOutcome::ProjectionAhead => {
            Ok(ParentProjectionGate::Withhold)
        }
        WaitOutcome::Fatal(failure) => Err(eyre::eyre!(
            "projection readiness failed ({:?}): {}",
            failure.class,
            failure.message
        )),
    }
}

/// Pure min-block-time floor arithmetic: remaining pad = `min - elapsed`
/// (`saturating_sub`). A zero result means the floor is already met - send the
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
/// `response.closed()` and nothing is sent. Liveness pacing only - it never
/// touches block bytes/hash/validation, so it is invisible to validators.
///
/// `propose_start` is the closure-level instant captured before `handle_propose`;
/// `elapsed` therefore subsumes the whole build + marshal path, making the floor
/// a total ceiling (`max(floor, build)`), not an additive delay.
pub(super) async fn pace_and_send<C>(
    ctx: &C,
    mut response: oneshot::Sender<Digest>,
    digest: Digest,
    min_block_time: std::time::Duration,
    propose_start: std::time::SystemTime,
) where
    C: commonware_runtime::Clock,
{
    let elapsed = ctx
        .current()
        .duration_since(propose_start)
        .unwrap_or_default();
    let remaining = floor_remaining(min_block_time, elapsed);
    crate::metrics::record_block_build_time(elapsed);
    crate::metrics::record_block_wait_time(remaining);
    if remaining.is_zero() {
        // Case C (elapsed >= min): send now, control-flow identical to pre-pacing.
        let _ = response.send(digest);
    } else {
        // Biased select!: cancellation wins over a near-simultaneous sleep
        // completion, so we never send into a closed channel.
        commonware_macros::select! {
            () = response.closed() => {
                debug!("view cancelled during min-block-time pacing; dropping proposal");
            },
            _ = ctx.sleep(remaining) => {
                let _ = response.send(digest);
            },
        }
    }
}
