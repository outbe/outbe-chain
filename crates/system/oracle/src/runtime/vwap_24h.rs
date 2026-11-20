//! The 24-hour VWAP of a closed snapshot window, judged by round coverage.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_chain_constants::VWAP_24H_WINDOW_SECONDS;
use outbe_primitives::error::Result;
use outbe_system_common::address_pair::AddressPair;
use outbe_system_common::addresses::ORACLE_ADDRESS;

use super::VwapAccumulator;
use crate::constants::VWAP_HOUR_SECONDS;
use crate::coverage::{hour_spans, CoverageTally};
use crate::errors::OracleError;
use crate::precompile::IOracle;
use crate::schema::OracleContract;
use crate::state::hourly_vwap_cell;
use crate::window::VwapSnapshotId;

impl OracleContract<'_> {
    /// VWAP over the snapshot's window once its cutoff has passed.
    ///
    /// Coverage is judged three times by the coverage share of the UTC day VWAP
    /// (`get_day_vwap_min_coverage`), so the two values are equal at 00:00:
    /// - An hour counts only when the pair has at least that share of the tally
    ///   rounds the hour's blocks allowed.
    /// - At least that share of the window's hours must count. An hour without
    ///   blocks does not, so a halt holds the window back until enough whole
    ///   hours have passed since.
    /// - The hours that count must together hold at least that share of the
    ///   rounds the window's blocks allowed.
    ///
    /// Hours that do not count contribute neither price nor volume. `None` when
    /// the window has no hour note, fails a test, holds no observation, or has
    /// no positive price.
    pub fn vwap_24h(&self, pair: AddressPair, snapshot: VwapSnapshotId) -> Result<Option<U256>> {
        if snapshot.cutoff() > self.storage.timestamp()?.saturating_to::<u64>() {
            return Err(OracleError::InvalidVwapSnapshot.into());
        }
        self.judged_window_vwap(
            pair,
            snapshot.start(),
            snapshot.cutoff(),
            outbe_chain_constants::get_day_vwap_min_coverage(),
        )
    }

    /// The step of the whole hour `window_end`. It emits `Vwap24hSet` for each
    /// vote target that has a 24-hour VWAP in the window that ends at that
    /// hour. Then it starts an S-curve for each reference currency whose VWAP
    /// is above its curve.
    pub(crate) fn close_vwap_24h_hour(&mut self, window_end: u64) -> Result<()> {
        let Some(start) = window_end.checked_sub(VWAP_24H_WINDOW_SECONDS) else {
            return Ok(());
        };
        let share = outbe_chain_constants::get_day_vwap_min_coverage();
        let vwaps = self
            .vote_target_values(|pair| self.judged_window_vwap(pair, start, window_end, share))?;
        for &(pair, vwap) in &vwaps {
            let event = IOracle::Vwap24hSet {
                windowEnd: window_end,
                base: pair.address1(),
                quote: pair.address2(),
                vwap,
            };
            self.storage
                .emit_event(ORACLE_ADDRESS, event.encode_log_data())?;
        }
        crate::scurve::process_hourly_scurves(self, window_end, &vwaps)
    }

    /// VWAP over the closed window `[start, cutoff)`, judged by round coverage
    /// as in [`Self::vwap_24h`] with `share` as the coverage share.
    pub(crate) fn judged_window_vwap(
        &self,
        pair: AddressPair,
        start: u64,
        cutoff: u64,
        share: (u64, u64),
    ) -> Result<Option<U256>> {
        // No block span to judge against (no hour note, or a window off the
        // hour grid): no VWAP.
        let Some(bounds) = self.window_block_bounds(start, cutoff)? else {
            return Ok(None);
        };
        let vwap = self.covered_window_vwap(pair, start, &bounds, share)?;
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
