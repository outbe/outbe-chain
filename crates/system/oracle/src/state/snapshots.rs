//! Raw price snapshot ring: capacity checks, writes, VWAP aggregates and
//! eviction.

use alloy_primitives::U256;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;
use outbe_primitives::math::reference_price::is_coen_iso_market;
use outbe_primitives::storage::types::Mapping;

use crate::constants::{
    zero_volume_weight, HOURLY_VWAP_CELLS, MAX_SNAPSHOT_RETENTION_SECONDS, VWAP_HOUR_SECONDS,
};
use crate::errors::OracleError;
use crate::schema::OracleContract;

const WWD_SUFFIX_START_SECONDS: u64 = 10 * 60 * 60;
const WWD_PREFIX_END_SECONDS: u64 = 12 * 60 * 60;

/// Price-volume and volume sums of one VWAP aggregation family, keyed by
/// period start (or by ring cell for the hourly family).
struct VwapBucket<'storage> {
    pv_sum: Mapping<'storage, u64, U256>,
    vol_sum: Mapping<'storage, u64, U256>,
    /// Overflow labels for the price-volume sum and the volume sum.
    errors: (&'static str, &'static str),
}

impl VwapBucket<'_> {
    /// Adds `pv` and `volume` to the sums at `key`. COEN/ISO markets reject an
    /// overflow. Generic markets saturate.
    fn add(&self, pair: AddressPair, key: u64, pv: U256, volume: U256) -> Result<()> {
        let previous_pv = self.pv_sum.read(&key)?;
        let previous_volume = self.vol_sum.read(&key)?;
        if is_coen_iso_market(pair) {
            self.pv_sum.write(
                &key,
                previous_pv
                    .checked_add(pv)
                    .ok_or(OracleError::VwapOverflow(self.errors.0))?,
            )?;
            self.vol_sum.write(
                &key,
                previous_volume
                    .checked_add(volume)
                    .ok_or(OracleError::VwapOverflow(self.errors.1))?,
            )?;
        } else {
            self.pv_sum.write(&key, previous_pv.saturating_add(pv))?;
            self.vol_sum
                .write(&key, previous_volume.saturating_add(volume))?;
        }
        Ok(())
    }
}

/// Ring cell holding the hour that starts at `hour_start`.
pub(crate) fn hourly_vwap_cell(hour_start: u64) -> u64 {
    (hour_start / VWAP_HOUR_SECONDS) % HOURLY_VWAP_CELLS
}

impl OracleContract<'_> {
    /// Maximum positive submitted volume that can be accumulated exactly in
    /// every applicable VWAP bucket for this rate.
    pub(crate) fn snapshot_volume_capacity(
        &self,
        timestamp: u64,
        pair: AddressPair,
        rate: U256,
    ) -> Result<U256> {
        if rate.is_zero() {
            return Ok(U256::MAX);
        }

        let mut capacity = U256::MAX / rate;
        let day = timestamp - (timestamp % 86_400);

        let daily_pv = self.daily_pv_sum.get_nested(&pair).read(&day)?;
        let daily_volume = self.daily_vol_sum.get_nested(&pair).read(&day)?;
        capacity = capacity
            .min((U256::MAX - daily_pv) / rate)
            .min(U256::MAX - daily_volume);

        let seconds_since_midnight = timestamp % 86_400;
        if seconds_since_midnight >= WWD_SUFFIX_START_SECONDS {
            let suffix_pv = self.wwd_suffix_pv_sum.get_nested(&pair).read(&day)?;
            let suffix_volume = self.wwd_suffix_vol_sum.get_nested(&pair).read(&day)?;
            capacity = capacity
                .min((U256::MAX - suffix_pv) / rate)
                .min(U256::MAX - suffix_volume);
        }
        if seconds_since_midnight < WWD_PREFIX_END_SECONDS {
            let prefix_pv = self.wwd_prefix_pv_sum.get_nested(&pair).read(&day)?;
            let prefix_volume = self.wwd_prefix_vol_sum.get_nested(&pair).read(&day)?;
            capacity = capacity
                .min((U256::MAX - prefix_pv) / rate)
                .min(U256::MAX - prefix_volume);
        }

        Ok(capacity)
    }

    /// Returns whether a tally-produced snapshot entry can be accumulated
    /// exactly. A zero submitted volume uses the existing per-pair sentinel.
    pub(crate) fn snapshot_can_accept(
        &self,
        timestamp: u64,
        pair: AddressPair,
        rate: U256,
        submitted_volume: U256,
    ) -> Result<bool> {
        let required_volume = if submitted_volume.is_zero() {
            zero_volume_weight(pair)
        } else {
            submitted_volume
        };
        Ok(required_volume <= self.snapshot_volume_capacity(timestamp, pair, rate)?)
    }

    /// Writes a price snapshot with rates/volumes for the given pairs.
    ///
    /// Each entry is (registered pair, rate, volume). This function appends the
    /// snapshot at `snapshot_write_idx` and evicts old entries beyond the
    /// retention window.
    pub fn write_snapshot(
        &mut self,
        timestamp: u64,
        entries: &[(AddressPair, U256, U256)],
    ) -> Result<()> {
        let requires_atomic_scale6_write =
            entries.iter().any(|(pair, _, _)| is_coen_iso_market(*pair));
        if self.ocomp_profile_ready.read()? || requires_atomic_scale6_write {
            let storage = self.storage.clone();
            storage.with_checkpoint(|| self.write_snapshot_inner(timestamp, entries))
        } else {
            self.write_snapshot_inner(timestamp, entries)
        }
    }

    fn write_snapshot_inner(
        &mut self,
        timestamp: u64,
        entries: &[(AddressPair, U256, U256)],
    ) -> Result<()> {
        let idx = self.snapshot_write_idx.read()?;
        let next_snapshot_idx = idx
            .checked_add(1)
            .ok_or(OracleError::SnapshotWriteIndexOverflow)?;
        self.require_snapshot_in_order(idx, timestamp)?;
        let next_ocomp_version = self.next_ocomp_state_version()?;

        self.write_snapshot_entries(idx, timestamp, entries)?;
        self.snapshot_write_idx.write(next_snapshot_idx)?;

        for (pair, rate, volume) in entries {
            self.accumulate_snapshot_entry(timestamp, *pair, *rate, *volume)?;
        }

        // Evict old entries beyond retention window
        self.evict_old_snapshots(timestamp)?;

        self.commit_ocomp_state_version(next_ocomp_version)
    }

    /// Rejects a snapshot older than the newest retained one. Range reads and
    /// the hourly cells both rely on non-decreasing timestamps.
    fn require_snapshot_in_order(&self, idx: u64, timestamp: u64) -> Result<()> {
        if idx > self.snapshot_oldest_idx.read()?
            && timestamp < self.snapshot_timestamp.read(&(idx - 1))?
        {
            return Err(OracleError::SnapshotOutOfOrder.into());
        }
        Ok(())
    }

    /// Writes the timestamp and the `(pair, rate, volume)` entries of snapshot `idx`.
    fn write_snapshot_entries(
        &self,
        idx: u64,
        timestamp: u64,
        entries: &[(AddressPair, U256, U256)],
    ) -> Result<()> {
        self.snapshot_timestamp.write(&idx, timestamp)?;
        self.snapshot_pair_count.write(&idx, entries.len() as u32)?;

        let columns = self.snapshot_entries(idx);
        for (i, (pair, rate, volume)) in entries.iter().enumerate() {
            columns.write(i as u32, *pair, *rate, *volume)?;
        }
        Ok(())
    }

    /// Adds one snapshot entry to the daily, hourly and WorldwideDay suffix or
    /// prefix VWAP sums of its pair.
    fn accumulate_snapshot_entry(
        &self,
        timestamp: u64,
        pair: AddressPair,
        rate: U256,
        volume: U256,
    ) -> Result<()> {
        let utc_day_ts = timestamp - (timestamp % 86_400);
        let seconds_since_midnight = timestamp % 86_400;
        let hour_start = timestamp - (timestamp % VWAP_HOUR_SECONDS);
        let vol = if volume.is_zero() {
            zero_volume_weight(pair)
        } else {
            volume
        };
        let pv = if is_coen_iso_market(pair) {
            rate.checked_mul(vol)
                .ok_or(OracleError::VwapOverflow("rate * volume"))?
        } else {
            rate.checked_mul(vol).unwrap_or(U256::MAX)
        };
        let daily = VwapBucket {
            pv_sum: self.daily_pv_sum.get_nested(&pair),
            vol_sum: self.daily_vol_sum.get_nested(&pair),
            errors: ("daily sum accumulation", "daily volume sum"),
        };
        daily.add(pair, utc_day_ts, pv, vol)?;
        self.accumulate_hourly_vwap(pair, hour_start, pv, vol)?;

        if seconds_since_midnight >= WWD_SUFFIX_START_SECONDS {
            let suffix = VwapBucket {
                pv_sum: self.wwd_suffix_pv_sum.get_nested(&pair),
                vol_sum: self.wwd_suffix_vol_sum.get_nested(&pair),
                errors: ("WWD suffix sum accumulation", "WWD suffix volume sum"),
            };
            suffix.add(pair, utc_day_ts, pv, vol)?;
        }
        if seconds_since_midnight < WWD_PREFIX_END_SECONDS {
            let prefix = VwapBucket {
                pv_sum: self.wwd_prefix_pv_sum.get_nested(&pair),
                vol_sum: self.wwd_prefix_vol_sum.get_nested(&pair),
                errors: ("WWD prefix sum accumulation", "WWD prefix volume sum"),
            };
            prefix.add(pair, utc_day_ts, pv, vol)?;
        }
        Ok(())
    }

    fn accumulate_hourly_vwap(
        &self,
        pair: AddressPair,
        hour_start: u64,
        pv: U256,
        volume: U256,
    ) -> Result<()> {
        let cell = hourly_vwap_cell(hour_start);
        let hours = self.hourly_vwap_hour.get_nested(&pair);
        let hourly = VwapBucket {
            pv_sum: self.hourly_pv_sum.get_nested(&pair),
            vol_sum: self.hourly_vol_sum.get_nested(&pair),
            errors: ("hourly sum accumulation", "hourly volume sum"),
        };
        let count = self.hourly_snapshot_count.get_nested(&pair);
        if hours.read(&cell)? != hour_start {
            hours.write(&cell, hour_start)?;
            hourly.pv_sum.write(&cell, pv)?;
            count.write(&cell, 1)?;
            return hourly.vol_sum.write(&cell, volume);
        }
        count.write(&cell, count.read(&cell)?.saturating_add(1))?;
        hourly.add(pair, cell, pv, volume)
    }

    /// Evicts snapshots older than the retention window.
    fn evict_old_snapshots(&mut self, current_timestamp: u64) -> Result<()> {
        let oldest = self.snapshot_oldest_idx.read()?;
        let write_idx = self.snapshot_write_idx.read()?;

        let cutoff = current_timestamp.saturating_sub(MAX_SNAPSHOT_RETENTION_SECONDS);
        let mut new_oldest = oldest;

        while new_oldest < write_idx {
            let ts = self.snapshot_timestamp.read(&new_oldest)?;
            if ts >= cutoff {
                break;
            }
            new_oldest += 1;
        }

        if new_oldest != oldest {
            self.snapshot_oldest_idx.write(new_oldest)?;
        }

        Ok(())
    }
    /// Index of the first snapshot at or after `target_time`, within `[lo, hi)`.
    pub(crate) fn binary_search_snapshot_idx(
        &self,
        target_time: u64,
        lo: u64,
        hi: u64,
    ) -> Result<u64> {
        let mut low = lo;
        let mut high = hi;
        while low < high {
            let mid = low + (high - low) / 2;
            let ts = self.snapshot_timestamp.read(&mid)?;
            if ts < target_time {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        Ok(low)
    }
}
