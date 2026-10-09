//! VWAP over a time range from daily, hourly and raw snapshot sums.

use alloy_primitives::U256;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;
use outbe_primitives::time::SECONDS_PER_DAY;

use super::{pair_series, PairSeries, VwapAccumulator};
use crate::constants::VWAP_HOUR_SECONDS;
use crate::errors::OracleError;
use crate::schema::OracleContract;
use crate::state::hourly_vwap_cell;

impl OracleContract<'_> {
    fn add_daily_aggregates(
        &self,
        pair: AddressPair,
        start_time: u64,
        end_time: u64,
        total: &mut VwapAccumulator,
    ) -> Result<()> {
        let day_pv = self.daily_pv_sum.get_nested(&pair);
        let day_vol = self.daily_vol_sum.get_nested(&pair);
        let mut day = start_time;
        while day < end_time {
            let pv = day_pv.read(&day)?;
            let volume = day_vol.read(&day)?;
            if !volume.is_zero() {
                total.add(
                    pair,
                    pv,
                    volume,
                    "daily sum accumulation",
                    "daily volume sum",
                )?;
            }
            day = day
                .checked_add(SECONDS_PER_DAY)
                .ok_or(OracleError::InvalidVwapRange)?;
        }
        Ok(())
    }

    /// Calculates VWAP for a specific pair over a time range.
    ///
    /// VWAP = sum(price_i * volume_i) / sum(volume_i)
    /// Values remain in the pair's registered scale.
    pub fn calculate_vwap(
        &self,
        pair: AddressPair,
        start_time: u64,
        end_time: u64,
    ) -> Result<U256> {
        self.try_calculate_vwap(pair, start_time, end_time)?
            .ok_or_else(|| OracleError::NoVwapData.into())
    }

    /// [`Self::calculate_vwap`] with "the window held no samples" as `Ok(None)`.
    ///
    /// Callers that skip empty pairs rather than reverting route through this,
    /// so no code path has to recognise the empty case by its revert message.
    pub(crate) fn try_calculate_vwap(
        &self,
        pair: AddressPair,
        start_time: u64,
        end_time: u64,
    ) -> Result<Option<U256>> {
        if start_time >= end_time {
            return Err(OracleError::InvalidVwapRange.into());
        }
        let first_full_day = if start_time.is_multiple_of(SECONDS_PER_DAY) {
            start_time
        } else {
            start_time
                .checked_add(SECONDS_PER_DAY - start_time % SECONDS_PER_DAY)
                .ok_or(OracleError::InvalidVwapRange)?
        };
        let complete_days_end = end_time - end_time % SECONDS_PER_DAY;
        let mut total = VwapAccumulator::default();
        if first_full_day < complete_days_end {
            self.add_sub_day_span(pair, start_time, first_full_day, &mut total)?;
            self.add_daily_aggregates(pair, first_full_day, complete_days_end, &mut total)?;
            self.add_sub_day_span(pair, complete_days_end, end_time, &mut total)?;
        } else {
            self.add_sub_day_span(pair, start_time, end_time, &mut total)?;
        }
        Ok(total.finish())
    }

    /// Whole hours come from the hourly cells. Partial hours and hours whose cell
    /// was already reused for a later hour are read from raw snapshots.
    fn add_sub_day_span(
        &self,
        pair: AddressPair,
        start_time: u64,
        end_time: u64,
        total: &mut VwapAccumulator,
    ) -> Result<()> {
        let first_hour = start_time
            .checked_next_multiple_of(VWAP_HOUR_SECONDS)
            .ok_or(OracleError::InvalidVwapRange)?;
        let last_hour = end_time - end_time % VWAP_HOUR_SECONDS;
        if first_hour >= last_hour {
            return self.add_raw_snapshots(pair, start_time, end_time, total);
        }
        // Consecutive hours without a usable cell share one raw scan.
        let mut raw_from = start_time;
        let mut hour = first_hour;
        while hour < last_hour {
            if self.add_hourly_aggregate(pair, hour, total)? {
                self.add_raw_snapshots(pair, raw_from, hour, total)?;
                raw_from = hour + VWAP_HOUR_SECONDS;
            }
            hour += VWAP_HOUR_SECONDS;
        }
        self.add_raw_snapshots(pair, raw_from, end_time, total)
    }

    /// Whether the hour's cell accounts for it. Snapshots are written in time
    /// order, so a cell still labelled with an earlier hour proves the pair had no
    /// entry in this one. Only a cell reused for a later hour does not.
    pub(super) fn add_hourly_aggregate(
        &self,
        pair: AddressPair,
        hour_start: u64,
        total: &mut VwapAccumulator,
    ) -> Result<bool> {
        let cell = hourly_vwap_cell(hour_start);
        let held = self.hourly_vwap_hour.get_nested(&pair).read(&cell)?;
        if held != hour_start {
            return Ok(held < hour_start);
        }
        let volume = self.hourly_vol_sum.get_nested(&pair).read(&cell)?;
        if !volume.is_zero() {
            let pv = self.hourly_pv_sum.get_nested(&pair).read(&cell)?;
            total.add(
                pair,
                pv,
                volume,
                "hourly sum accumulation",
                "hourly volume sum",
            )?;
        }
        Ok(true)
    }

    /// Calculates VWAPs for all active vote-target pairs over an explicit range.
    pub fn calculate_vwaps(&self, start_time: u64, end_time: u64) -> Result<PairSeries> {
        self.try_calculate_vwaps(start_time, end_time)?
            .ok_or_else(|| OracleError::NoVwapData.into())
    }

    /// [`Self::calculate_vwaps`] with "no pair had data" as `Ok(None)`.
    ///
    /// An invalid range is still an error: an empty result means the oracle had
    /// nothing to say about a well-formed window, not that the window was junk.
    pub(super) fn try_calculate_vwaps(
        &self,
        start_time: u64,
        end_time: u64,
    ) -> Result<Option<PairSeries>> {
        if start_time >= end_time {
            return Err(OracleError::InvalidVwapRange.into());
        }

        let lookback = end_time - start_time;
        let vwaps =
            self.vote_target_values(|pair| self.try_calculate_vwap(pair, start_time, end_time))?;
        Ok(pair_series(vwaps, lookback))
    }

    /// Returns `(nominal, vwap, max_scurve, source)` for a pair.
    ///
    /// Nominal price follows the Cosmos port rule: `max(VWAP, S-curve)`.
    /// If no VWAP samples exist for the day, VWAP contributes zero.
    pub fn get_nominal_price_components(
        &self,
        pair: AddressPair,
        timestamp: u64,
    ) -> Result<(U256, U256, U256, String)> {
        let day_start = crate::scurve::truncate_to_day(timestamp);
        let day_end = day_start.saturating_add(crate::scurve::DAY_SECONDS);
        let vwap = self
            .try_calculate_vwap(pair, day_start, day_end)?
            .unwrap_or(U256::ZERO);
        let max_scurve = crate::scurve::get_max_active_scurve_value(self, pair, timestamp)?;

        let (nominal, source) = if vwap.is_zero() && max_scurve.is_zero() {
            (U256::ZERO, "none".to_string())
        } else if vwap > max_scurve {
            (vwap, "vwap".to_string())
        } else {
            (max_scurve, "scurve".to_string())
        };

        Ok((nominal, vwap, max_scurve, source))
    }

    /// Calculates VWAP for a pair using a lookback in seconds from `now`.
    pub fn calculate_vwap_lookback(
        &self,
        pair: AddressPair,
        now: u64,
        lookback_seconds: u64,
    ) -> Result<U256> {
        let max_lookback = self.config_lookback_duration.read()?;
        let effective_lookback = lookback_seconds.min(max_lookback);
        let start_time = now.saturating_sub(effective_lookback);
        self.calculate_vwap(pair, start_time, now)
    }
}
