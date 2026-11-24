//! Resolution stall tracking and timer deadline calculation for `FinalizationActor`.

use std::time::{Duration, SystemTime};

use commonware_consensus::types::Round;
use commonware_runtime::Clock;
use tracing::warn;

use super::{FinalizationActor, ParkedReason, ResolutionStall};
use crate::digest::Digest;

struct StallObservation {
    round: Option<Round>,
    digest: Option<Digest>,
    cycle: Duration,
    elapsed: Duration,
    last_finalized_number: u64,
}

impl FinalizationActor {
    pub(super) fn resolution_cycle() -> Duration {
        crate::config::FINALIZE_RESOLUTION_TIMEOUT * crate::config::FINALIZE_MAX_RETRIES
            + crate::config::FINALIZE_RETRY_DELAY * (crate::config::FINALIZE_MAX_RETRIES - 1)
    }

    pub(super) fn clear_resolution_stall(&mut self) {
        if self
            .resolution_stall
            .as_ref()
            .is_some_and(|stall| stall.alarmed)
        {
            crate::metrics::record_finalization_divergence_cleared();
        }
        self.resolution_stall = None;
    }

    pub(super) fn stall_target_identity(&self) -> (Option<Round>, Option<Digest>) {
        self.unresolved
            .iter()
            .next()
            .map(|(round, digest)| (Some(*round), Some(*digest)))
            .or_else(|| {
                self.pending_target
                    .as_ref()
                    .map(|target| (Some(target.tip.round), Some(target.tip.digest)))
            })
            .unwrap_or((None, None))
    }

    pub(super) fn cap_delay_for_stall(&self, delay: Duration, now: SystemTime) -> Duration {
        let Some(stall) = self.resolution_stall.as_ref() else {
            return delay;
        };
        let cycle = Self::resolution_cycle();
        let next_cycle = stall.started
            + cycle.saturating_mul((stall.cycles.saturating_add(1)).min(u32::MAX as u64) as u32);
        let next_alarm =
            (!stall.alarmed).then_some(stall.started + crate::config::STALL_DIVERGENCE_ALARM);
        let deadline = next_alarm.map_or(next_cycle, |alarm| alarm.min(next_cycle));
        let until_deadline = deadline.duration_since(now).unwrap_or_default();
        delay.min(until_deadline)
    }

    pub(super) fn update_resolution_stall(&mut self, clock: &impl Clock, progressed: bool) {
        if progressed {
            self.clear_resolution_stall();
        }
        let missing_block =
            !self.unresolved.is_empty() || self.parked_reason == Some(ParkedReason::MissingBlock);
        if !missing_block {
            self.clear_resolution_stall();
            return;
        }
        let now = clock.current();
        let (round, digest) = self.stall_target_identity();
        let cycle = Self::resolution_cycle();
        let last_finalized_number = self.deps.view.read().last_finalized_number;
        let stall = self.resolution_stall.get_or_insert(ResolutionStall {
            started: now,
            cycles: 0,
            alarmed: false,
        });
        let elapsed = now.duration_since(stall.started).unwrap_or_default();
        record_stall_progress(
            stall,
            StallObservation {
                round,
                digest,
                cycle,
                elapsed,
                last_finalized_number,
            },
        );
    }
}

fn record_stall_progress(stall: &mut ResolutionStall, obs: StallObservation) {
    let due_cycles = (obs.elapsed.as_nanos() / obs.cycle.as_nanos()).min(u64::MAX as u128) as u64;
    if stall.cycles < due_cycles {
        let completed_cycles = due_cycles - stall.cycles;
        stall.cycles = due_cycles;
        crate::metrics::record_finalization_resolution_stalled(completed_cycles);
        warn!(
            target: "outbe::finalization",
            round = ?obs.round,
            digest = ?obs.digest,
            stall_cycles = stall.cycles,
            cycle_budget_secs = obs.cycle.as_secs(),
            last_finalized_number = obs.last_finalized_number,
            "finalized block remains unavailable after one resolution cycle"
        );
    }
    if !stall.alarmed && obs.elapsed >= crate::config::STALL_DIVERGENCE_ALARM {
        stall.alarmed = true;
        crate::metrics::record_finalization_divergence_suspected();
        tracing::error!(
            target: "outbe::finalization",
            round = ?obs.round,
            digest = ?obs.digest,
            stall_cycles = stall.cycles,
            elapsed_secs = obs.elapsed.as_secs(),
            "suspected local divergence: marshal resolution exceeded the alarm threshold"
        );
    }
}
