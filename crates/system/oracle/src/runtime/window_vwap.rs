//! VWAP of a finalized snapshot window, judged by round coverage.

use alloy_primitives::U256;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;

use super::VwapAccumulator;
use crate::constants::{MIN_WINDOW_COVERAGE, VWAP_HOUR_SECONDS};
use crate::errors::OracleError;
use crate::schema::OracleContract;
use crate::state::hourly_vwap_cell;
use crate::window::VwapSnapshotId;

impl OracleContract<'_> {
    /// VWAP over the snapshot's window once its cutoff has passed.
    ///
    /// Coverage is judged twice against the tally rounds the blocks allowed
    /// (see [`MIN_WINDOW_COVERAGE`]):
    /// - An hour counts only when the pair has at least that share of the hour's
    ///   rounds.
    /// - The hours that count must together hold at least that share of the whole
    ///   window's rounds.
    ///
    /// Hours that do not count contribute neither price nor volume. `None` when
    /// the window fails that test, holds no observation, or has no positive price.
    pub fn finalized_window_vwap(
        &self,
        pair: AddressPair,
        snapshot: VwapSnapshotId,
    ) -> Result<Option<U256>> {
        if snapshot.cutoff() > self.storage.timestamp()?.to::<u64>() {
            return Err(OracleError::InvalidVwapSnapshot.into());
        }
        let (start, cutoff) = (snapshot.start(), snapshot.cutoff());
        let vwap = match self.window_block_bounds(start, cutoff)? {
            Some(bounds) => self.covered_window_vwap(pair, start, &bounds)?,
            // No block span to judge against (hours recorded before this
            // rule, or a window off the hour grid): unjudged, as before.
            None => self.try_calculate_vwap(pair, start, cutoff)?,
        };
        Ok(vwap.filter(|vwap| !vwap.is_zero()))
    }

    /// First block of every hour boundary in `[start, cutoff]`, zero where the
    /// hour saw no block. The cutoff boundary falls back to the current block.
    /// `None` when the window is off the hour grid or no hour has a record.
    fn window_block_bounds(&self, start: u64, cutoff: u64) -> Result<Option<Vec<u64>>> {
        if !start.is_multiple_of(VWAP_HOUR_SECONDS) || !cutoff.is_multiple_of(VWAP_HOUR_SECONDS) {
            return Ok(None);
        }
        let mut bounds = Vec::new();
        let mut hour = start;
        while hour < cutoff {
            bounds.push(self.hour_first_block.read(&hour)?);
            hour += VWAP_HOUR_SECONDS;
        }
        if bounds.iter().all(|block| *block == 0) {
            return Ok(None);
        }
        bounds.push(match self.hour_first_block.read(&cutoff)? {
            0 => self.storage.block_number()?,
            block => block,
        });
        Ok(Some(bounds))
    }

    fn covered_window_vwap(
        &self,
        pair: AddressPair,
        start: u64,
        bounds: &[u64],
    ) -> Result<Option<U256>> {
        let vote_period = self.config_vote_period.read()?.max(1);
        let (numerator, denominator) = MIN_WINDOW_COVERAGE;
        let covered = |actual: u64, possible: u64| {
            actual.saturating_mul(denominator) >= possible.saturating_mul(numerator)
        };
        let last = bounds.len() - 1;
        let window_end = bounds[last];
        let mut window_first = None;
        let mut counted = 0u64;
        let mut total = VwapAccumulator::default();
        for (index, &first_block) in bounds[..last].iter().enumerate() {
            if first_block == 0 {
                continue; // no block in this hour, so no round and no snapshot
            }
            window_first.get_or_insert(first_block);
            // The hour ends where the next hour with blocks begins.
            let end_block = bounds[index + 1..]
                .iter()
                .copied()
                .find(|block| *block != 0)
                .unwrap_or(window_end);
            let possible = end_block.saturating_sub(first_block) / vote_period;
            let hour = start + index as u64 * VWAP_HOUR_SECONDS;
            let mut sums = VwapAccumulator::default();
            let snapshots = self.add_hour(pair, hour, &mut sums)?;
            if snapshots == 0 || !covered(snapshots, possible) {
                continue;
            }
            counted = counted.saturating_add(snapshots);
            total.add(
                pair,
                sums.price_volume,
                sums.volume,
                "window sum accumulation",
                "window volume sum",
            )?;
        }
        let Some(window_first) = window_first else {
            return Ok(None);
        };
        let possible = window_end.saturating_sub(window_first) / vote_period;
        Ok(covered(counted, possible).then(|| total.finish()).flatten())
    }

    /// Adds one whole hour's sums to `total` and returns how many snapshots
    /// carried the pair. It reads the hourly cell while that cell still holds
    /// that hour. It reads raw snapshots once the cell was reused.
    fn add_hour(&self, pair: AddressPair, hour: u64, total: &mut VwapAccumulator) -> Result<u64> {
        let cell = hourly_vwap_cell(hour);
        let held = self.hourly_vwap_hour.get_nested(&pair).read(&cell)?;
        if held == hour {
            self.add_hourly_aggregate(pair, hour, total)?;
            return self.hourly_snapshot_count.get_nested(&pair).read(&cell);
        }
        if held < hour {
            return Ok(0); // snapshots are time-ordered: nothing was written
        }
        let end = hour + VWAP_HOUR_SECONDS;
        self.add_raw_snapshots(pair, hour, end, total)?;
        self.count_raw_snapshots(pair, hour, end)
    }
}
