//! VWAP of a finalized snapshot window, judged by round coverage.

use alloy_primitives::U256;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;

use super::VwapAccumulator;
use crate::constants::{MIN_WINDOW_COVERAGE, VWAP_HOUR_SECONDS};
use crate::coverage::{hour_spans, CoverageTally};
use crate::errors::OracleError;
use crate::schema::OracleContract;
use crate::state::hourly_vwap_cell;
use crate::window::VwapSnapshotId;

impl OracleContract<'_> {
    /// VWAP over the snapshot's window once its cutoff has passed.
    ///
    /// Coverage is judged three times by the share in [`crate::constants::MIN_WINDOW_COVERAGE`]:
    /// - An hour counts only when the pair has at least that share of the tally
    ///   rounds the hour's blocks allowed.
    /// - At least that share of the window's hours must count. An hour without
    ///   blocks does not, so a halt holds the window back until enough whole
    ///   hours have passed since.
    /// - The hours that count must together hold at least that share of the
    ///   rounds the window's blocks allowed.
    ///
    /// Hours that do not count contribute neither price nor volume. `None` when
    /// the window fails a test, holds no observation, or has no positive price.
    pub fn finalized_window_vwap(
        &self,
        pair: AddressPair,
        snapshot: VwapSnapshotId,
    ) -> Result<Option<U256>> {
        if snapshot.cutoff() > self.storage.timestamp()?.saturating_to::<u64>() {
            return Err(OracleError::InvalidVwapSnapshot.into());
        }
        self.judged_window_vwap(
            pair,
            snapshot.start(),
            snapshot.cutoff(),
            MIN_WINDOW_COVERAGE,
        )
    }

    /// VWAP over the closed window `[start, cutoff)`, judged by round coverage
    /// as in [`Self::finalized_window_vwap`] with `share` in place of
    /// [`MIN_WINDOW_COVERAGE`].
    pub(crate) fn judged_window_vwap(
        &self,
        pair: AddressPair,
        start: u64,
        cutoff: u64,
        share: (u64, u64),
    ) -> Result<Option<U256>> {
        let vwap = match self.window_block_bounds(start, cutoff)? {
            Some(bounds) => self.covered_window_vwap(pair, start, &bounds, share)?,
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
        share: (u64, u64),
    ) -> Result<Option<U256>> {
        let vote_period = self.config_vote_period.read()?;
        let mut tally = CoverageTally::new(bounds, vote_period, share);
        let mut total = VwapAccumulator::default();
        for span in hour_spans(bounds, vote_period) {
            let hour = start + span.index as u64 * VWAP_HOUR_SECONDS;
            let mut sums = VwapAccumulator::default();
            let snapshots = self.add_hour(pair, hour, &mut sums)?;
            if tally.record(span, snapshots) {
                total.add(
                    pair,
                    sums.price_volume,
                    sums.volume,
                    "window sum accumulation",
                    "window volume sum",
                )?;
            }
        }
        Ok(tally
            .verdict()
            .filter(|verdict| verdict.covered())
            .and_then(|_| total.finish()))
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
