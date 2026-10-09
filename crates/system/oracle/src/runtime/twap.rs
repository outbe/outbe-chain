//! TWAP over the raw price snapshot ring.

use alloy_primitives::U256;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;

use super::{pair_series, PairSeries};
use crate::errors::OracleError;
use crate::schema::OracleContract;

/// Running sums of a time-weighted average.
#[derive(Default)]
struct TimeWeightedSum {
    price_time: U256,
    time: U256,
}

impl TimeWeightedSum {
    /// Adds `rate` held for `duration` seconds.
    fn add(&mut self, rate: U256, duration: U256) -> Result<()> {
        let pv = rate
            .checked_mul(duration)
            .ok_or(OracleError::TwapOverflow)?;
        self.price_time = self
            .price_time
            .checked_add(pv)
            .ok_or(OracleError::TwapOverflow)?;
        self.time = self
            .time
            .checked_add(duration)
            .ok_or(OracleError::TwapOverflow)?;
        Ok(())
    }
}

/// Time-weighted average of at least two chronological samples. Each rate
/// holds until the next sample. The last rate holds until `now`.
fn time_weighted_average(data: &[(u64, U256)], now: u64) -> Result<U256> {
    // TWAP: weight each price by time until next price change
    let mut sums = TimeWeightedSum::default();
    for i in 0..data.len() - 1 {
        let duration = U256::from(data[i + 1].0 - data[i].0);
        sums.add(data[i].1, duration)?;
    }

    // Include last price until `now`
    let last = data.last().ok_or(OracleError::MissingTwapData)?;
    let last_duration = U256::from(now.saturating_sub(last.0));
    if !last_duration.is_zero() {
        sums.add(last.1, last_duration)?;
    }

    if sums.time.is_zero() {
        return Ok(data[0].1);
    }

    Ok(sums.price_time / sums.time)
}

impl OracleContract<'_> {
    /// Calculates TWAP (time-weighted average price) for a pair.
    ///
    /// TWAP = sum(price_i * duration_i) / sum(duration_i)
    /// where duration_i is the time between consecutive snapshots.
    pub fn calculate_twap(
        &self,
        pair: AddressPair,
        now: u64,
        lookback_seconds: u64,
    ) -> Result<U256> {
        self.try_calculate_twap(pair, now, lookback_seconds)?
            .ok_or_else(|| OracleError::NoTwapData.into())
    }

    /// [`Self::calculate_twap`] with "the window held no samples" as `Ok(None)`.
    fn try_calculate_twap(
        &self,
        pair: AddressPair,
        now: u64,
        lookback_seconds: u64,
    ) -> Result<Option<U256>> {
        let max_lookback = self.config_lookback_duration.read()?;
        if lookback_seconds == 0 || lookback_seconds > max_lookback {
            return Err(OracleError::InvalidLookbackSeconds.into());
        }
        let start_time = now.saturating_sub(lookback_seconds);
        let data = self.twap_samples(pair, start_time, now)?;

        if data.is_empty() {
            return Ok(None);
        }

        if data.len() == 1 {
            return Ok(Some(data[0].1));
        }

        time_weighted_average(&data, now).map(Some)
    }

    /// Collects the `(timestamp, rate)` samples of `pair` in
    /// `[start_time, now]` in chronological order.
    fn twap_samples(
        &self,
        pair: AddressPair,
        start_time: u64,
        now: u64,
    ) -> Result<Vec<(u64, U256)>> {
        let write_idx = self.snapshot_write_idx.read()?;
        let oldest_idx = self.snapshot_oldest_idx.read()?;

        // Collect (timestamp, rate) pairs in chronological order
        let mut data: Vec<(u64, U256)> = Vec::new();

        for idx in oldest_idx..write_idx {
            let ts = self.snapshot_timestamp.read(&idx)?;
            if ts < start_time {
                continue;
            }
            if ts > now {
                break;
            }

            if let Some(entry) = self.snapshot_entry(idx, pair)? {
                data.push((ts, self.snapshot_rate.get_nested(&idx).read(&entry)?));
            }
        }
        Ok(data)
    }

    /// Calculates TWAPs for all active vote-target pairs.
    pub fn calculate_twaps(&self, now: u64, lookback_seconds: u64) -> Result<PairSeries> {
        self.try_calculate_twaps(now, lookback_seconds)?
            .ok_or_else(|| OracleError::NoTwapData.into())
    }

    /// [`Self::calculate_twaps`] with "no pair had data" as `Ok(None)`.
    fn try_calculate_twaps(&self, now: u64, lookback_seconds: u64) -> Result<Option<PairSeries>> {
        let twaps =
            self.vote_target_values(|pair| self.try_calculate_twap(pair, now, lookback_seconds))?;
        Ok(pair_series(twaps, lookback_seconds))
    }
}
