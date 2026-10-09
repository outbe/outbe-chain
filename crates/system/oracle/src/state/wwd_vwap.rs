//! Stored WorldwideDay VWAP snapshots.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;

use crate::errors::OracleError;
use crate::schema::{OracleContract, PairIndex};

/// `(start_time, end_time, bases, quotes, vwaps, lookbacks)` - stored WWD VWAP snapshot.
type WorldwideDayVwapSnapshot = (u64, u64, Vec<Address>, Vec<Address>, Vec<U256>, Vec<u64>);

impl OracleContract<'_> {
    /// Returns a stored WorldwideDay VWAP snapshot.
    pub fn get_worldwide_day_vwap_snapshot(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<WorldwideDayVwapSnapshot> {
        if !self.worldwide_day_vwap_exists.read(&worldwide_day)? {
            return Err(OracleError::WorldwideDayVwapSnapshotNotFound.into());
        }

        let start_time = self.worldwide_day_vwap_start.read(&worldwide_day)?;
        let end_time = self.worldwide_day_vwap_end.read(&worldwide_day)?;
        let lookback = end_time.saturating_sub(start_time);

        let value_map = self.worldwide_day_vwap_value.get_nested(&worldwide_day);
        let day_vwaps = self.registered_nonzero_values(&value_map)?;
        let bases = day_vwaps.iter().map(|(pair, _)| pair.address1()).collect();
        let quotes = day_vwaps.iter().map(|(pair, _)| pair.address2()).collect();
        let vwaps = day_vwaps.iter().map(|(_, vwap)| *vwap).collect();
        let lookbacks = vec![lookback; day_vwaps.len()];

        Ok((start_time, end_time, bases, quotes, vwaps, lookbacks))
    }

    /// Returns a stored WorldwideDay VWAP for the pair registered under `index`.
    /// Returns `None` when the day has no snapshot or that pair had no data in it.
    pub fn get_worldwide_day_vwap_for_pair(
        &self,
        worldwide_day: WorldwideDay,
        index: PairIndex,
    ) -> Result<Option<U256>> {
        if !self.worldwide_day_vwap_exists.read(&worldwide_day)? {
            return Ok(None);
        }

        let vwap = self
            .worldwide_day_vwap_value
            .get_nested(&worldwide_day)
            .read(&index)?;
        Ok((!vwap.is_zero()).then_some(vwap))
    }
}
