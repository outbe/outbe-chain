//! Newest-first reads of the raw price snapshot ring.

use alloy_primitives::{Address, U256};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;

use crate::schema::OracleContract;

/// `(snapshot_ids, timestamps, bases, quotes, rates, volumes)` - flattened history.
type SnapshotHistory = (
    Vec<u64>,
    Vec<u64>,
    Vec<Address>,
    Vec<Address>,
    Vec<U256>,
    Vec<U256>,
);

impl OracleContract<'_> {
    /// Returns price snapshot history for a pair (most recent first).
    ///
    /// Returns `(timestamps, rates, volumes)` as parallel arrays,
    /// up to `count` entries.
    pub fn get_price_snapshot_history(
        &self,
        pair: AddressPair,
        count: u32,
    ) -> Result<(Vec<u64>, Vec<U256>, Vec<U256>)> {
        let write_idx = self.snapshot_write_idx.read()?;
        let oldest_idx = self.snapshot_oldest_idx.read()?;

        let mut timestamps = Vec::new();
        let mut rates = Vec::new();
        let mut volumes = Vec::new();

        let mut idx = write_idx;
        while idx > oldest_idx && timestamps.len() < count as usize {
            idx -= 1;
            let ts = self.snapshot_timestamp.read(&idx)?;
            let pc = self.snapshot_pair_count.read(&idx)?;
            let entries = self.snapshot_entries(idx);

            for p in 0..pc {
                if entries.pair(p)?.same_market(&pair) {
                    timestamps.push(ts);
                    rates.push(entries.rate(p)?);
                    volumes.push(entries.volume(p)?);
                    break;
                }
            }
        }

        Ok((timestamps, rates, volumes))
    }

    /// Returns flattened snapshot history across all pairs.
    ///
    /// `count` limits the number of snapshots scanned, newest first. Return arrays
    /// are aligned by item, so one snapshot with N pairs produces N output rows.
    pub fn get_all_price_snapshot_history(&self, count: u32) -> Result<SnapshotHistory> {
        let write_idx = self.snapshot_write_idx.read()?;
        let oldest_idx = self.snapshot_oldest_idx.read()?;

        let mut snapshot_ids = Vec::new();
        let mut timestamps = Vec::new();
        let mut bases = Vec::new();
        let mut quotes = Vec::new();
        let mut rates = Vec::new();
        let mut volumes = Vec::new();

        let mut snapshots_seen = 0u32;
        let mut idx = write_idx;
        while idx > oldest_idx && snapshots_seen < count {
            idx -= 1;
            snapshots_seen += 1;

            let ts = self.snapshot_timestamp.read(&idx)?;
            let pc = self.snapshot_pair_count.read(&idx)?;
            for (entry, rate, volume) in self.snapshot_entries(idx).read_all(pc)? {
                snapshot_ids.push(idx);
                timestamps.push(ts);
                bases.push(entry.address1());
                quotes.push(entry.address2());
                rates.push(rate);
                volumes.push(volume);
            }
        }

        Ok((snapshot_ids, timestamps, bases, quotes, rates, volumes))
    }
}
