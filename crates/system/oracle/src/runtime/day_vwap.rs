//! UTC-day VWAP finalization.

use outbe_primitives::error::Result;
use outbe_system_common::time::{date_key_to_utc_timestamp, SECONDS_PER_DAY};

use crate::schema::OracleContract;

impl OracleContract<'_> {
    /// Computes and persists the VWAP of every active vote-target pair for the
    /// fully-closed UTC calendar day `utc_day` (yyyymmdd UTC - *not* a
    /// WorldwideDay, which is UTC+14). The window is the canonical
    /// `[date_key_to_utc_timestamp(utc_day), +SECONDS_PER_DAY)`.
    ///
    /// The day is judged by round coverage like a VWAP window (see
    /// [`Self::vwap_24h`]), with the share of
    /// `oracle.dayVwapMinCoverage` (two thirds in production). A pair that fails coverage or has no
    /// data for the day is skipped, so catch-up blocks after a halt cannot set
    /// a day price. If no pair passes, nothing is written. The day then stays
    /// empty, and the `utc_day_vwap_last_finalized` watermark tells it apart
    /// from an unfinalized one. It emits no event: the value of the day is the
    /// `Vwap24hSet` of 00:00 of the next day. The method overwrites unconditionally. The caller gates
    /// re-finalization via that same watermark.
    pub fn finalize_utc_day_vwap(&mut self, utc_day: u32) -> Result<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| self.finalize_utc_day_vwap_inner(utc_day))
    }

    fn finalize_utc_day_vwap_inner(&mut self, utc_day: u32) -> Result<()> {
        let day_start =
            date_key_to_utc_timestamp(utc_day).map_err(crate::errors::date_step_error)?;
        let day_end = day_start.saturating_add(SECONDS_PER_DAY);

        // A pair with no covered VWAP stays unwritten, so the day reads as
        // finalized-empty for it against the watermark.
        let share = outbe_chain_constants::get_day_vwap_min_coverage();
        let day_vwaps = self
            .vote_target_values(|pair| self.judged_window_vwap(pair, day_start, day_end, share))?;

        // Keyed by the registry index. Unwritten entries stay zero and read back
        // as "no VWAP for this pair on this day". Re-finalizing a closed day
        // recomputes over the same immutable window, so no stale entry survives.
        for (pair, vwap) in day_vwaps {
            self.record_utc_day_vwap(utc_day, self.pair_index_of(pair)?, vwap)?;
        }

        Ok(())
    }
}
