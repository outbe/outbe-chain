//! Oracle business logic: vote submission, VWAP/TWAP computation, WorldwideDay
//! and UTC-day finalization, and the OCOMP projection profile.

mod day_vwap;
mod raw_snapshots;
mod twap;
mod vote;
mod vwap;
mod window_vwap;

use crate::constants::DAY_TYPE_PAIR;
use crate::errors::{OracleError, OracleOcompError};
use crate::schema::OracleContract;
use alloy_primitives::U256;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;
use outbe_primitives::math::reference_price::is_coen_iso_market;

/// `(pairs, values, lookbacks)` - one row per active vote-target pair, each
/// carrying the orientation it was registered in.
type PairSeries = (Vec<AddressPair>, Vec<U256>, Vec<u64>);

#[derive(Clone, Copy, Default)]
struct VwapAccumulator {
    price_volume: U256,
    volume: U256,
}

/// Builds a [`PairSeries`] from per-pair values, one `lookback` per row.
/// `None` when no pair had a value.
fn pair_series(values: Vec<(AddressPair, U256)>, lookback: u64) -> Option<PairSeries> {
    if values.is_empty() {
        return None;
    }
    let lookbacks = vec![lookback; values.len()];
    let (pairs, prices) = values.into_iter().unzip();
    Some((pairs, prices, lookbacks))
}

impl VwapAccumulator {
    pub(super) fn add(
        &mut self,
        pair: AddressPair,
        price_volume: U256,
        volume: U256,
        price_volume_error: &'static str,
        volume_error: &'static str,
    ) -> Result<()> {
        if is_coen_iso_market(pair) {
            self.price_volume = self
                .price_volume
                .checked_add(price_volume)
                .ok_or(OracleError::VwapOverflow(price_volume_error))?;
            self.volume = self
                .volume
                .checked_add(volume)
                .ok_or(OracleError::VwapOverflow(volume_error))?;
        } else {
            self.price_volume = self.price_volume.saturating_add(price_volume);
            self.volume = self.volume.saturating_add(volume);
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Option<U256> {
        (!self.volume.is_zero()).then(|| self.price_volume / self.volume)
    }
}

impl OracleContract<'_> {
    /// Initializes the fixed OCOMP Oracle projection for a fresh devnet.
    pub fn initialize_fresh_ocomp_profile(&mut self) -> Result<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            if self.pair_index_of(DAY_TYPE_PAIR)? == 0 {
                return Err(OracleOcompError::DayTypePairNotRegistered.into());
            }

            if self.ocomp_profile_ready.read()? {
                if self.ocomp_state_version.read()? == 0 {
                    return Err(OracleOcompError::ProfileReadyWithZeroVersion.into());
                }
                return Ok(());
            }
            if self.ocomp_state_version.read()? != 0 {
                return Err(OracleOcompError::PartialPreForkState.into());
            }

            self.ocomp_state_version.write(1)?;
            self.ocomp_profile_ready.write(true)
        })
    }

    /// Reserves the next OCOMP-visible Oracle version before its owner writes.
    ///
    /// Returning `None` keeps every historical pre-fork mutation byte-for-byte
    /// inert. Overflow is rejected before any related owner state changes.
    pub(crate) fn next_ocomp_state_version(&self) -> Result<Option<u64>> {
        if !self.ocomp_profile_ready.read()? {
            return Ok(None);
        }
        let current = self.ocomp_state_version.read()?;
        if current == 0 {
            return Err(OracleOcompError::StateVersionZero.into());
        }
        current
            .checked_add(1)
            .map(Some)
            .ok_or_else(|| OracleOcompError::StateVersionOverflow.into())
    }

    pub(crate) fn commit_ocomp_state_version(&self, next: Option<u64>) -> Result<()> {
        if let Some(version) = next {
            self.ocomp_state_version.write(version)?;
        }
        Ok(())
    }

    /// Evaluates `value_of` for every vote-target pair in registry order and
    /// keeps each pair that has a value.
    fn vote_target_values(
        &self,
        mut value_of: impl FnMut(AddressPair) -> Result<Option<U256>>,
    ) -> Result<Vec<(AddressPair, U256)>> {
        let count = self.pair_count.read()?;
        let mut values = Vec::new();
        for pid in 1..=count {
            let entry = self.pair_at(pid)?;
            if !self.vote_target.read(&entry)? {
                continue;
            }
            if let Some(value) = value_of(entry)? {
                values.push((entry, value));
            }
        }
        Ok(values)
    }
}
