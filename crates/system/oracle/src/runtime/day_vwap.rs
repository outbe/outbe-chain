//! WorldwideDay and UTC-day VWAP finalization.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::time::{date_key_to_utc_timestamp, SECONDS_PER_DAY};

use super::VwapAccumulator;
use crate::constants::DAY_TYPE_PAIR;
use crate::errors::OracleError;
use crate::precompile::IOracle;
use crate::schema::OracleContract;

impl OracleContract<'_> {
    fn try_worldwide_day_vwap(&self, pair: AddressPair, start_time: u64) -> Result<Option<U256>> {
        let suffix_day = start_time - start_time % SECONDS_PER_DAY;
        let full_day = suffix_day
            .checked_add(SECONDS_PER_DAY)
            .ok_or(OracleError::InvalidVwapRange)?;
        let prefix_day = full_day
            .checked_add(SECONDS_PER_DAY)
            .ok_or(OracleError::InvalidVwapRange)?;
        let components = [
            (
                self.wwd_suffix_pv_sum.get_nested(&pair).read(&suffix_day)?,
                self.wwd_suffix_vol_sum
                    .get_nested(&pair)
                    .read(&suffix_day)?,
            ),
            (
                self.daily_pv_sum.get_nested(&pair).read(&full_day)?,
                self.daily_vol_sum.get_nested(&pair).read(&full_day)?,
            ),
            (
                self.wwd_prefix_pv_sum.get_nested(&pair).read(&prefix_day)?,
                self.wwd_prefix_vol_sum
                    .get_nested(&pair)
                    .read(&prefix_day)?,
            ),
        ];
        let mut total = VwapAccumulator::default();
        for (price_volume, volume) in components {
            if !volume.is_zero() {
                total.add(
                    pair,
                    price_volume,
                    volume,
                    "WWD sum accumulation",
                    "WWD volume sum",
                )?;
            }
        }
        Ok(total.finish())
    }

    /// Calculates VWAPs for the given WorldwideDay window and stores them in
    /// oracle state. Returns `false` when the window held no oracle data. In that
    /// case nothing is written. This is a deterministic no-op, not an error.
    pub fn store_worldwide_day_vwap_snapshot(
        &mut self,
        worldwide_day: WorldwideDay,
        start_time: u64,
        end_time: u64,
    ) -> Result<bool> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.store_worldwide_day_vwap_snapshot_inner(worldwide_day, start_time, end_time)
        })
    }

    fn store_worldwide_day_vwap_snapshot_inner(
        &mut self,
        worldwide_day: WorldwideDay,
        start_time: u64,
        end_time: u64,
    ) -> Result<bool> {
        require_worldwide_day_window(worldwide_day, start_time, end_time)?;

        let day_vwaps = self.worldwide_day_vwaps(start_time)?;
        if day_vwaps.is_empty() {
            return Ok(false);
        }
        let next_ocomp_version = self.next_ocomp_state_version()?;

        self.worldwide_day_vwap_exists.write(&worldwide_day, true)?;
        self.worldwide_day_vwap_start
            .write(&worldwide_day, start_time)?;
        self.worldwide_day_vwap_end
            .write(&worldwide_day, end_time)?;

        // Keyed by the registry index, so the pair itself is already recorded in
        // `pair_by_index`. Values not written stay zero, which reads back as "no
        // VWAP for this pair on this day". A WorldwideDay is written once, at the
        // Metadosis ResolveForming edge, so a second write with fewer pairs
        // cannot leave a stale entry behind.
        let value_map = self.worldwide_day_vwap_value.get_nested(&worldwide_day);
        for (pair, vwap) in &day_vwaps {
            value_map.write(&self.pair_index_of(*pair)?, *vwap)?;
        }

        self.commit_ocomp_state_version(next_ocomp_version)?;
        Ok(true)
    }

    /// Returns the WorldwideDay VWAP of every vote-target pair with data, in
    /// registry order.
    fn worldwide_day_vwaps(&self, start_time: u64) -> Result<Vec<(AddressPair, U256)>> {
        self.vote_target_values(|pair| self.try_worldwide_day_vwap(pair, start_time))
    }

    /// Computes and persists the VWAP of every active vote-target pair for the
    /// fully-closed UTC calendar day `utc_day` (yyyymmdd UTC - *not* a
    /// WorldwideDay, which is UTC+14). The window is the canonical
    /// `[date_key_to_utc_timestamp(utc_day), +SECONDS_PER_DAY)`.
    ///
    /// Pairs without data for the day are skipped (mirrors `calculate_vwaps`).
    /// If no pair has data, nothing is written. The day then stays empty, and the
    /// `utc_day_vwap_last_finalized` watermark tells it apart from an unfinalized
    /// one. Emits one `VwapCalculated` event per written pair in registration
    /// order. The method overwrites unconditionally. The caller gates
    /// re-finalization via that same watermark.
    pub fn finalize_utc_day_vwap(&mut self, utc_day: u32) -> Result<()> {
        if self.ocomp_profile_ready.read()? {
            let storage = self.storage.clone();
            storage.with_checkpoint(|| self.finalize_utc_day_vwap_inner(utc_day))
        } else {
            self.finalize_utc_day_vwap_inner(utc_day)
        }
    }

    fn finalize_utc_day_vwap_inner(&mut self, utc_day: u32) -> Result<()> {
        let day_start = date_key_to_utc_timestamp(utc_day);
        let day_end = day_start.saturating_add(SECONDS_PER_DAY);

        // No vote-target pair had data for the day. Leave it unwritten so the
        // day reads as finalized-empty against the watermark.
        let Some((pairs, vwaps, _)) = self.try_calculate_vwaps(day_start, day_end)? else {
            return Ok(());
        };
        let next_ocomp_version = self.next_ocomp_state_version()?;
        let profile_ready = self.ocomp_profile_ready.read()?;

        // Keyed by the registry index. Unwritten entries stay zero and read back
        // as "no VWAP for this pair on this day". Re-finalizing a closed day
        // recomputes over the same immutable window, so no stale entry survives.
        for (pair, vwap) in pairs.iter().copied().zip(vwaps.iter().copied()) {
            self.record_utc_day_vwap(utc_day, self.pair_index_of(pair)?, vwap)?;
            if profile_ready && pair.same_market(&DAY_TYPE_PAIR) {
                self.ocomp_day_type_vwap_by_utc_day.write(&utc_day, vwap)?;
            }
            let event = IOracle::VwapCalculated {
                utcDay: utc_day,
                base: pair.address1(),
                quote: pair.address2(),
                vwap,
            };
            let event_result = self
                .storage
                .emit_event(ORACLE_ADDRESS, event.encode_log_data());
            if next_ocomp_version.is_some() {
                event_result?;
            }
        }

        self.commit_ocomp_state_version(next_ocomp_version)
    }
}

/// Checks that `[start_time, end_time)` is the forming window of
/// `worldwide_day`.
fn require_worldwide_day_window(
    worldwide_day: WorldwideDay,
    start_time: u64,
    end_time: u64,
) -> Result<()> {
    // The forming window is a protocol parameter, not an oracle constant.
    // A chain that shortens it (localnet, E2E) still hands Metadosis'
    // real window here. A second hardcoded copy would reject it.
    let expected_start = worldwide_day.start_timestamp();
    let expected_end = expected_start
        .checked_add(outbe_chain_constants::get_metadosis_forming_period_seconds())
        .ok_or(OracleError::InvalidVwapRange)?;
    if !worldwide_day.is_valid() || start_time != expected_start || end_time != expected_end {
        return Err(OracleError::InvalidVwapRange.into());
    }
    Ok(())
}
