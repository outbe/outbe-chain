//! Follower sync driver.
//!
//! Pulls the upstream's finalized tip and walks the follower's marshal forward
//! to it, one height at a time, by `hint_finalized`-ing each height. The
//! marshal's gap-repair then fetches `Finalized { height }` through the
//! [`FollowResolver`](super::resolver), which verifies the certificate against
//! the epoch committee and hands the block to the executor.
//!
//! The driver never predicts epochs. It hints only through the latest possible
//! boundary allowed by the observed activation plus genesis grace. The resolver
//! authenticates a pre-announce/boundary and advances the shared epocher before
//! handing a boundary block to the marshal.

use std::time::{Duration, SystemTime};

use commonware_consensus::types::Height;
use commonware_cryptography::bls12381;
use commonware_runtime::{Clock, Metrics, Spawner};
use commonware_utils::vec::NonEmptyVec;
use tracing::{debug, info};

use crate::follow::metrics::{
    record_follower_heights, record_follower_round, record_upstream_tip_err,
    record_upstream_tip_ok, FollowerRoundResult, TipKnownGuard,
};
use crate::follow::upstream::TipSource;
use crate::follow::FollowerEpocher;
use crate::marshal_types::MarshalMailbox;

/// How often the driver wakes to re-hint the marshal's pull window.
const TIP_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Re-query the upstream tip only every Nth wakeup. Hints re-issue every wakeup
/// (the marshal needs steady re-hinting as its floor advances). The driver throttles
/// the upstream `outbe_consensusStatus` tip query to avoid HTTP 429 rate-limits
/// on a busy upstream. Between tip queries the driver drives toward the last
/// known tip.
const TIP_REFRESH_EVERY: u32 = 4;

/// How many heights above the marshal's processed floor to keep hinted at once.
/// The marshal fetches the lowest permitted height and processes in order. A
/// modest window keeps a backlog of in-flight resolver fetches without flooding
/// the bounded handler mailbox. Re-hinting the (sliding) window each tick is
/// idempotent. The marshal skips a height that is already finalized locally.
const HINT_WINDOW: u64 = 64;

fn hint_range(
    processed: Option<Height>,
    tip: Height,
    ceiling: Option<Height>,
) -> (Option<std::ops::RangeInclusive<u64>>, bool) {
    let Some(processed) = processed.map(Height::get) else {
        return (None, false);
    };
    let end = tip.get().min(processed.saturating_add(HINT_WINDOW));
    let clamped = processed < tip.get() && ceiling.is_none_or(|ceiling| ceiling.get() < end);
    let range = ceiling
        .map(|ceiling| end.min(ceiling.get()))
        .filter(|end| *end > processed)
        .map(|end| processed.saturating_add(1)..=end);
    (range, clamped)
}

/// A stub target peer for `hint_finalized`. The follower has no real consensus
/// peers. The resolver ignores targets and serves from the upstream regardless.
/// But `hint_finalized` requires a non-empty target set.
fn stub_targets() -> NonEmptyVec<bls12381::PublicKey> {
    use commonware_cryptography::Signer as _;
    NonEmptyVec::new(bls12381::PrivateKey::from_seed(0).public_key())
}

/// Configuration for the follow driver.
pub(super) struct Config<T> {
    /// Marshal mailbox. It receives `hint_finalized` for each height to pull.
    pub(super) marshal: MarshalMailbox,
    /// Upstream tip discovery.
    pub(super) tip: T,
    /// Height->epoch strategy (shared with the marshal).
    pub(super) epocher: FollowerEpocher,
}

/// The follow driver actor.
pub(super) struct Driver<E, T> {
    context: E,
    config: Config<T>,
}

impl<E, T> Driver<E, T>
where
    E: Spawner + Clock + Metrics + Send + Sync + 'static,
    T: TipSource,
{
    pub(super) fn new(context: E, config: Config<T>) -> Self {
        Self { context, config }
    }

    pub(super) fn start(self) -> commonware_runtime::Handle<()> {
        let context = self.context.child("run");
        context.spawn(move |_| self.run())
    }

    async fn run(mut self) {
        let _tip_known_guard = TipKnownGuard::new();
        info!("follow driver started");
        // Cancellation covers pending upstream and marshal queries as well as
        // the polling sleep. A stopped marshal must never be polled for work.
        let stopped = self.context.stopped();
        tokio::select! {
            biased;
            _ = stopped => info!("follow driver stopped"),
            _ = self.drive() => {},
        }
    }

    async fn drive(&mut self) {
        // Last successfully discovered upstream tip. A fresh tip query can fail
        // transiently (e.g. the upstream RPC rate-limits our poll with HTTP 429).
        // The driver keeps driving the marshal toward the last known tip rather than
        // stalling the whole sync on one failed status call.
        let mut last_tip: Option<Height> = None;
        let mut wakeups: u32 = 0;
        loop {
            // Refresh the tip on the first wakeup and every TIP_REFRESH_EVERY
            // after. Otherwise reuse the last known tip and just re-hint.
            if wakeups.is_multiple_of(TIP_REFRESH_EVERY) {
                match self.config.tip.finalized_tip().await {
                    Ok(tip) => {
                        let now_unix_secs = self
                            .context
                            .current()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64();
                        record_upstream_tip_ok(tip.get(), now_unix_secs);
                        last_tip = Some(tip);
                    }
                    Err(err) => {
                        record_upstream_tip_err(&err);
                        debug!(
                            error = outbe_primitives::telemetry::error_field(&err),
                            "upstream tip query failed; driving to last known tip"
                        );
                    }
                }
            }
            wakeups = wakeups.wrapping_add(1);
            if let Some(tip) = last_tip {
                self.pull_to(tip).await;
            } else {
                record_follower_round(FollowerRoundResult::TipUnreachable);
            }
            self.context.sleep(TIP_POLL_INTERVAL).await;
        }
    }

    /// Drive the marshal forward to `tip`.
    ///
    /// The marshal advances its finalized chain ONE height at a time. It only
    /// admits a resolver fetch for a height ABOVE its processed floor. The marshal
    /// silently drops a flooded batch of out-of-order hints. `hint_finalized` is
    /// fire-and-forget. So each tick the driver reads the
    /// marshal's current processed height and (re-)hints a small contiguous WINDOW
    /// just above it. The epocher's `supported_ceiling()` caps the window. The
    /// driver registers no committees. The resolver admission path does that.
    /// As the marshal processes the lowest height,
    /// the floor rises and the next tick's window slides up. This keeps a bounded
    /// backlog of in-flight fetches and never leaves a gap unhinted.
    async fn pull_to(&mut self, tip: Height) {
        // Marshal's processed floor (genesis anchor = height 0 on a fresh node).
        let processed = self
            .config
            .marshal
            .get_processed()
            .await
            .map(commonware_consensus::marshal::core::Processed::height);
        let ceiling = self.config.epocher.supported_ceiling();
        record_follower_heights(processed.map(Height::get), ceiling.map(Height::get));
        let (range, clamped) = hint_range(processed, tip, ceiling);
        let Some(range) = range else {
            debug!(
                tip = tip.get(),
                processed = ?processed,
                "follow driver has no eligible hint range"
            );
            if clamped {
                record_follower_round(FollowerRoundResult::CeilingClamped);
            } else if processed.is_some_and(|processed| processed >= tip) {
                record_follower_round(FollowerRoundResult::Idle);
            }
            // Missing progress is not genesis or rollback. The engine supervises
            // marshal termination. This driver must not fabricate new work.
            return;
        };

        let targets = stub_targets();
        let hint_start = *range.start();
        let hint_end = *range.end();
        for height in range {
            self.config
                .marshal
                .hint_finalized(Height::new(height), targets.clone());
        }
        record_follower_round(if clamped {
            FollowerRoundResult::CeilingClamped
        } else {
            FollowerRoundResult::Pulled
        });
        debug!(
            tip = tip.get(),
            processed = ?processed,
            hint_start,
            hint_end,
            observed_ceiling = hint_end,
            "follow driver hinted window"
        );
    }
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;
