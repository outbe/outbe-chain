//! Finalized per-UTC-day VWAPs.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

use crate::schema::{OracleContract, PairIndex};

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

    /// The only writer of a day's VWAP.
    pub fn record_utc_day_vwap(&self, utc_day: u32, index: PairIndex, vwap: U256) -> Result<()> {
        self.utc_day_vwap_value
            .get_nested(&utc_day)
            .write(&index, vwap)
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
