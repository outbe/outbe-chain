//! Finalized per-UTC-day VWAPs and their monthly maxima.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

use crate::schema::{OracleContract, PairIndex};

/// Days `first_dd..=last_dd` of one `yyyymm` month.
#[derive(Clone, Copy, Debug)]
struct MonthDays {
    month: u32,
    first_dd: u32,
    last_dd: u32,
}

impl MonthDays {
    const fn new(month: u32, first_dd: u32, last_dd: u32) -> Self {
        Self {
            month,
            first_dd,
            last_dd,
        }
    }
}

impl OracleContract<'_> {
    /// Returns the finalized per-UTC-day VWAP for the pair registered under
    /// `index` on `utc_day` (yyyymmdd UTC). Returns `None` if the day is not
    /// finalized or had no data for that pair. To distinguish "not finalized
    /// yet" from "finalized, no data", compare `utc_day` against
    /// `utc_day_vwap_last_finalized`.
    pub fn get_utc_day_vwap_for_pair(
        &self,
        utc_day: u32,
        index: PairIndex,
    ) -> Result<Option<U256>> {
        let vwap = self.utc_day_vwap_value.get_nested(&utc_day).read(&index)?;
        Ok((!vwap.is_zero()).then_some(vwap))
    }

    /// The only writer of a day's VWAP, so the month maximum and the earliest recorded day
    /// cannot drift from the days themselves.
    pub fn record_utc_day_vwap(&self, utc_day: u32, index: PairIndex, vwap: U256) -> Result<()> {
        let days = self.utc_day_vwap_value.get_nested(&utc_day);
        let previous = days.read(&index)?;
        if previous == vwap {
            return Ok(());
        }
        days.write(&index, vwap)?;
        let first = self.utc_day_vwap_first_recorded.read()?;
        if first == 0 || utc_day < first {
            self.utc_day_vwap_first_recorded.write(utc_day)?;
        }
        let month = utc_day / 100;
        let maxima = self.utc_month_vwap_max.get_nested(&month);
        let max = maxima.read(&index)?;
        if vwap >= max {
            maxima.write(&index, vwap)
        } else if previous == max {
            // The day that held the maximum went down, so read the month again.
            maxima.write(
                &index,
                self.month_day_vwap_max(index, MonthDays::new(month, 1, 31), None)?,
            )
        } else {
            Ok(())
        }
    }

    /// Largest finalized day VWAP of `index` from `from_utc_day` to the watermark. This function
    /// reads the two edge months day by day and every month between them once. It stops above
    /// `stop_above`.
    pub(crate) fn max_finalized_day_vwap_since(
        &self,
        index: PairIndex,
        from_utc_day: u32,
        stop_above: Option<U256>,
    ) -> Result<U256> {
        let last = self.utc_day_vwap_last_finalized.read()?;
        let from = from_utc_day.max(self.utc_day_vwap_first_recorded.read()?);
        if from_utc_day == 0 || last < from {
            return Ok(U256::ZERO);
        }
        let (first_month, last_month) = (from / 100, last / 100);
        if first_month == last_month {
            let days = MonthDays::new(first_month, from % 100, last % 100);
            return self.month_day_vwap_max(index, days, stop_above);
        }
        let first_days = MonthDays::new(first_month, from % 100, 31);
        let mut max = self.month_day_vwap_max(index, first_days, stop_above)?;
        let mut month = next_month(first_month);
        while month < last_month && !exceeds(max, stop_above) {
            max = max.max(self.utc_month_vwap_max.get_nested(&month).read(&index)?);
            month = next_month(month);
        }
        if exceeds(max, stop_above) {
            return Ok(max);
        }
        let last_days = MonthDays::new(last_month, 1, last % 100);
        Ok(max.max(self.month_day_vwap_max(index, last_days, stop_above)?))
    }

    /// Largest VWAP of `index` over the days of `days`.
    fn month_day_vwap_max(
        &self,
        index: PairIndex,
        days: MonthDays,
        stop_above: Option<U256>,
    ) -> Result<U256> {
        let mut max = U256::ZERO;
        for dd in days.first_dd..=days.last_dd.min(31) {
            let vwap = self
                .utc_day_vwap_value
                .get_nested(&(days.month * 100 + dd))
                .read(&index)?;
            max = max.max(vwap);
            if exceeds(max, stop_above) {
                break;
            }
        }
        Ok(max)
    }

    /// Returns the full finalized VWAP set for `utc_day` as
    /// `(bases, quotes, vwaps)`. All vectors are empty when the day is
    /// unfinalized or had no data.
    pub fn get_utc_day_vwap_snapshot(
        &self,
        utc_day: u32,
    ) -> Result<(Vec<Address>, Vec<Address>, Vec<U256>)> {
        let value_map = self.utc_day_vwap_value.get_nested(&utc_day);
        let day_vwaps = self.registered_nonzero_values(&value_map)?;
        let bases = day_vwaps.iter().map(|(pair, _)| pair.address1()).collect();
        let quotes = day_vwaps.iter().map(|(pair, _)| pair.address2()).collect();
        let vwaps = day_vwaps.iter().map(|(_, vwap)| *vwap).collect();
        Ok((bases, quotes, vwaps))
    }
}

fn next_month(month: u32) -> u32 {
    if month % 100 == 12 {
        (month / 100 + 1) * 100 + 1
    } else {
        month + 1
    }
}

fn exceeds(max: U256, stop_above: Option<U256>) -> bool {
    stop_above.is_some_and(|stop| max > stop)
}
