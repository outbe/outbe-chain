//! Reads of the raw price snapshot ring.

use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;

use super::VwapAccumulator;
use crate::constants::zero_volume_weight;
use crate::errors::OracleError;
use crate::schema::{OracleContract, PairIndex};

impl OracleContract<'_> {
    /// Returns the entry of the market of `pair` in snapshot `idx`, or `None`
    /// when the snapshot has no entry for that market.
    pub(super) fn snapshot_entry(&self, idx: u64, pair: AddressPair) -> Result<Option<PairIndex>> {
        let pair_count = self.snapshot_pair_count.read(&idx)?;
        let pair_map = self.snapshot_pair.get_nested(&idx);
        for entry in 0..pair_count {
            if pair_map.read_pair(&entry)?.same_market(&pair) {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    /// Returns the snapshot index range `[start, end)` for the time range
    /// `[start_time, end_time)`, or `None` when no snapshot was ever written.
    /// The range must not start before the oldest retained snapshot.
    fn raw_snapshot_range(&self, start_time: u64, end_time: u64) -> Result<Option<(u64, u64)>> {
        let write_idx = self.snapshot_write_idx.read()?;
        let oldest_idx = self.snapshot_oldest_idx.read()?;
        if write_idx <= oldest_idx {
            if oldest_idx > 0 {
                return Err(OracleError::NoVwapData.into());
            }
            return Ok(None);
        }
        if oldest_idx > 0 && start_time < self.snapshot_timestamp.read(&oldest_idx)? {
            return Err(OracleError::NoVwapData.into());
        }

        let range_start = self.binary_search_snapshot_idx(start_time, oldest_idx, write_idx)?;
        let range_end = self.binary_search_snapshot_idx(end_time, oldest_idx, write_idx)?;
        Ok(Some((range_start, range_end)))
    }

    pub(super) fn add_raw_snapshots(
        &self,
        pair: AddressPair,
        start_time: u64,
        end_time: u64,
        total: &mut VwapAccumulator,
    ) -> Result<()> {
        if start_time >= end_time {
            return Ok(());
        }
        let Some((range_start, range_end)) = self.raw_snapshot_range(start_time, end_time)? else {
            return Ok(());
        };
        for idx in range_start..range_end {
            let Some(entry) = self.snapshot_entry(idx, pair)? else {
                continue;
            };
            let rate = self.snapshot_rate.get_nested(&idx).read(&entry)?;
            let stored_volume = self.snapshot_volume.get_nested(&idx).read(&entry)?;
            let volume = if stored_volume.is_zero() {
                zero_volume_weight(pair)
            } else {
                stored_volume
            };
            let price_volume = rate
                .checked_mul(volume)
                .ok_or(OracleError::VwapOverflow("rate * volume"))?;
            total.add(pair, price_volume, volume, "sum accumulation", "volume sum")?;
        }
        Ok(())
    }

    pub(super) fn count_raw_snapshots(
        &self,
        pair: AddressPair,
        start: u64,
        end: u64,
    ) -> Result<u64> {
        let write_idx = self.snapshot_write_idx.read()?;
        let oldest_idx = self.snapshot_oldest_idx.read()?;
        if write_idx <= oldest_idx {
            return Ok(0);
        }
        let range_start = self.binary_search_snapshot_idx(start, oldest_idx, write_idx)?;
        let range_end = self.binary_search_snapshot_idx(end, oldest_idx, write_idx)?;
        let mut total = 0u64;
        for idx in range_start..range_end {
            if self.snapshot_entry(idx, pair)?.is_some() {
                total += 1;
            }
        }
        Ok(total)
    }
}
