//! Oracle business logic: vote submission, VWAP/TWAP computation, WorldwideDay
//! and UTC-day finalization, and the OCOMP projection profile.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_common::WorldwideDay;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::time::{date_key_to_utc_timestamp, SECONDS_PER_DAY};
use std::collections::BTreeSet;

use crate::constants::DAY_TYPE_PAIR_KEY;
use crate::precompile::IOracle;
use crate::schema::{OracleContract, SCALE_1E18};

impl OracleContract<'_> {
    /// Initializes the fixed OCOMP Oracle projection for a fresh devnet.
    pub fn initialize_fresh_ocomp_profile(&mut self) -> Result<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let expected_pair_id = self.pair_ordinal_of(DAY_TYPE_PAIR_KEY)?;
            if expected_pair_id == 0 {
                return Err(PrecompileError::Fatal(
                    "Oracle OCOMP day-type pair is not registered".into(),
                ));
            }

            if self.ocomp_profile_ready.read()? {
                if self.ocomp_day_type_pair_id.read()? != expected_pair_id
                    || self.ocomp_state_version.read()? == 0
                {
                    return Err(PrecompileError::Fatal(
                        "Oracle OCOMP profile does not match the registered day-type pair".into(),
                    ));
                }
                return Ok(());
            }
            if self.ocomp_day_type_pair_id.read()? != 0 || self.ocomp_state_version.read()? != 0 {
                return Err(PrecompileError::Fatal(
                    "Oracle OCOMP profile contains partial pre-fork state".into(),
                ));
            }

            self.ocomp_day_type_pair_id.write(expected_pair_id)?;
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
            return Err(PrecompileError::BodyReadCorruption(
                "Oracle OCOMP profile is ready with zero state version".into(),
            ));
        }
        current.checked_add(1).map(Some).ok_or_else(|| {
            PrecompileError::BodyReadCorruption("Oracle OCOMP state version overflow".into())
        })
    }

    pub(crate) fn commit_ocomp_state_version(&self, next: Option<u64>) -> Result<()> {
        if let Some(version) = next {
            self.ocomp_state_version.write(version)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Vote Submission
    // -----------------------------------------------------------------------

    /// Submits an aggregate oracle vote on behalf of a validator.
    ///
    /// The caller must be the validator itself or a delegated feeder.
    /// Each tuple contains (base, quote, rate, volume) for one pair, quoted in
    /// the direction the pair was registered in.
    pub fn submit_vote(
        &mut self,
        caller: Address,
        tuples: &[(Address, Address, U256, U256)],
    ) -> Result<Address> {
        let validator = self.resolve_validator_for_feeder(caller)?;

        // Validate tuple count: cannot exceed active pair count
        let pair_count = self.pair_count.read()?;
        if tuples.len() as u32 > pair_count {
            return Err(PrecompileError::Revert(
                "vote tuple count exceeds registered pair count".into(),
            ));
        }

        // Resolve every quoted pair up front. `require_pair` rejects an
        // unregistered pair and one quoted against the registered direction —
        // the rate is a bare scalar, so a flipped quote would otherwise feed an
        // uninverted price into the tally median with nothing downstream able to
        // notice.
        let mut resolved = Vec::with_capacity(tuples.len());
        for (base, quote, rate, volume) in tuples {
            let (pair, ordinal) = self.require_pair(*base, *quote)?;
            resolved.push((pair, ordinal, *rate, *volume));
        }

        // Check for duplicate pairs in the submission. Kept separate from the
        // vote-target loop below so the revert precedence for a submission that
        // is both duplicated and untargeted stays unchanged.
        let mut seen = BTreeSet::new();
        for (pair, _, _, _) in &resolved {
            if !seen.insert(*pair) {
                return Err(PrecompileError::Revert(
                    "duplicate pair in vote submission".into(),
                ));
            }
        }

        // Verify all pairs are vote targets
        for (pair, _, _, _) in &resolved {
            let is_target = self.vote_target.read(pair)?;
            if !is_target {
                return Err(PrecompileError::Revert("pair is not a vote target".into()));
            }
        }

        // Check if already voted this period
        let already_voted = self.vote_exists.read(&validator)?;
        if already_voted {
            return Err(PrecompileError::Revert(
                "validator already voted this period".into(),
            ));
        }

        // Mark as voted FIRST to prevent concurrent overwrite (ORC-AUD-037).
        // EVM executes transactions sequentially within a block, so a second
        // submitVote TX in the same block sees this flag immediately.
        self.vote_exists.write(&validator, true)?;

        // Store vote tuples
        let tuple_count = tuples.len() as u32;
        self.vote_tuple_count.write(&validator, tuple_count)?;

        let pair_id_map = self.vote_pair_id.get_nested(&validator);
        let rate_map = self.vote_rate.get_nested(&validator);
        let volume_map = self.vote_volume.get_nested(&validator);

        for (i, (_, ordinal, rate, volume)) in resolved.iter().enumerate() {
            let idx = i as u32;
            pair_id_map.write(&idx, *ordinal)?;
            rate_map.write(&idx, *rate)?;
            volume_map.write(&idx, *volume)?;
        }

        // Add to voter list for tally iteration
        self.voter_list.push(validator)?;

        Ok(validator)
    }

    fn try_daily_aggregate_vwap(
        &self,
        pair_id: u32,
        start_time: u64,
        end_time: u64,
    ) -> Result<Option<U256>> {
        if end_time.saturating_sub(start_time) < 86_400 {
            return Ok(None);
        }

        let mut pv_total = U256::ZERO;
        let mut vol_total = U256::ZERO;
        // The daily aggregates key on the pair itself; the snapshot columns this
        // module scans elsewhere still key on the ordinal, so translate here.
        let pair = self.pair_at(pair_id)?;
        let day_pv = self.daily_pv_sum.get_nested(&pair);
        let day_vol = self.daily_vol_sum.get_nested(&pair);

        let start_day = start_time - (start_time % 86_400);
        let mut day = start_day;
        while day + 86_400 <= end_time {
            let pv = day_pv.read(&day).unwrap_or(U256::ZERO);
            let vol = day_vol.read(&day).unwrap_or(U256::ZERO);
            if !pv.is_zero() {
                pv_total = pv_total.saturating_add(pv);
                vol_total = vol_total.saturating_add(vol);
            }
            day += 86_400;
        }

        if vol_total.is_zero() {
            return Ok(None);
        }
        Ok(Some(pv_total / vol_total))
    }

    /// Calculates VWAP for a specific pair over a time range.
    ///
    /// VWAP = sum(price_i * volume_i) / sum(volume_i)
    /// All values at 1e18 scale.
    pub fn calculate_vwap(&self, pair_id: u32, start_time: u64, end_time: u64) -> Result<U256> {
        match self.try_daily_aggregate_vwap(pair_id, start_time, end_time) {
            Ok(Some(vwap)) => return Ok(vwap),
            Ok(None) if end_time.saturating_sub(start_time) >= 86_400 => {
                return Err(PrecompileError::Revert(
                    "no VWAP data in the requested time range".into(),
                ));
            }
            Ok(None) => {}
            Err(e) => return Err(e),
        }

        let write_idx = self.snapshot_write_idx.read()?;
        let oldest_idx = self.snapshot_oldest_idx.read()?;

        if write_idx <= oldest_idx {
            return Err(PrecompileError::Revert(
                "no VWAP data in the requested time range".into(),
            ));
        }

        let range_start = self.binary_search_snapshot_idx(start_time, oldest_idx, write_idx)?;
        let range_end = self.binary_search_snapshot_idx(end_time + 1, oldest_idx, write_idx)?;

        let mut price_volume_sum = U256::ZERO;
        let mut volume_sum = U256::ZERO;

        for idx in range_start..range_end {
            let pc = self.snapshot_pair_count.read(&idx)?;
            let pair_id_map = self.snapshot_pair_id.get_nested(&idx);
            let rate_map = self.snapshot_rate.get_nested(&idx);
            let volume_map = self.snapshot_volume.get_nested(&idx);

            for p in 0..pc {
                let snap_pair_id = pair_id_map.read(&p)?;
                if snap_pair_id != pair_id {
                    continue;
                }

                let rate = rate_map.read(&p)?;
                let volume = volume_map.read(&p)?;
                let vol = if volume.is_zero() { SCALE_1E18 } else { volume };

                price_volume_sum = price_volume_sum
                    .checked_add(rate.checked_mul(vol).ok_or_else(|| {
                        PrecompileError::Revert("VWAP overflow: rate * volume".into())
                    })?)
                    .ok_or_else(|| {
                        PrecompileError::Revert("VWAP overflow: sum accumulation".into())
                    })?;
                volume_sum = volume_sum
                    .checked_add(vol)
                    .ok_or_else(|| PrecompileError::Revert("VWAP overflow: volume sum".into()))?;
                break;
            }
        }

        if volume_sum.is_zero() {
            return Err(PrecompileError::Revert(
                "no VWAP data in the requested time range".into(),
            ));
        }

        Ok(price_volume_sum / volume_sum)
    }

    /// Calculates TWAP (time-weighted average price) for a pair.
    ///
    /// TWAP = sum(price_i * duration_i) / sum(duration_i)
    /// where duration_i is the time between consecutive snapshots.
    pub fn calculate_twap(&self, pair_id: u32, now: u64, lookback_seconds: u64) -> Result<U256> {
        let max_lookback = self.config_lookback_duration.read()?;
        if lookback_seconds == 0 || lookback_seconds > max_lookback {
            return Err(PrecompileError::Revert(
                "lookback_seconds must be > 0 and <= lookback_duration".into(),
            ));
        }
        let start_time = now.saturating_sub(lookback_seconds);

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

            let pc = self.snapshot_pair_count.read(&idx)?;
            let pair_id_map = self.snapshot_pair_id.get_nested(&idx);
            let rate_map = self.snapshot_rate.get_nested(&idx);

            for p in 0..pc {
                if pair_id_map.read(&p)? == pair_id {
                    data.push((ts, rate_map.read(&p)?));
                    break;
                }
            }
        }

        if data.is_empty() {
            return Err(PrecompileError::Revert(
                "no TWAP data in the requested time range".into(),
            ));
        }

        if data.len() == 1 {
            return Ok(data[0].1);
        }

        // TWAP: weight each price by time until next price change
        let mut price_time_sum = U256::ZERO;
        let mut time_sum = U256::ZERO;

        for i in 0..data.len() - 1 {
            let duration = U256::from(data[i + 1].0 - data[i].0);
            let pv = data[i]
                .1
                .checked_mul(duration)
                .ok_or_else(|| PrecompileError::Revert("TWAP overflow".into()))?;
            price_time_sum = price_time_sum
                .checked_add(pv)
                .ok_or_else(|| PrecompileError::Revert("TWAP overflow".into()))?;
            time_sum = time_sum
                .checked_add(duration)
                .ok_or_else(|| PrecompileError::Revert("TWAP overflow".into()))?;
        }

        // Include last price until `now`
        let last = data
            .last()
            .ok_or_else(|| PrecompileError::Revert("missing TWAP data".into()))?;
        let last_duration = U256::from(now.saturating_sub(last.0));
        if !last_duration.is_zero() {
            let pv = last
                .1
                .checked_mul(last_duration)
                .ok_or_else(|| PrecompileError::Revert("TWAP overflow".into()))?;
            price_time_sum = price_time_sum
                .checked_add(pv)
                .ok_or_else(|| PrecompileError::Revert("TWAP overflow".into()))?;
            time_sum = time_sum
                .checked_add(last_duration)
                .ok_or_else(|| PrecompileError::Revert("TWAP overflow".into()))?;
        }

        if time_sum.is_zero() {
            return Ok(data[0].1);
        }

        Ok(price_time_sum / time_sum)
    }

    /// Calculates TWAPs for all active vote-target pairs.
    pub fn calculate_twaps(
        &self,
        now: u64,
        lookback_seconds: u64,
    ) -> Result<(Vec<u32>, Vec<U256>, Vec<u64>)> {
        let count = self.pair_count.read()?;
        let mut pair_ids = Vec::new();
        let mut twaps = Vec::new();
        let mut lookbacks = Vec::new();

        for pid in 1..=count {
            if !self.vote_target.read(&self.pair_at(pid)?)? {
                continue;
            }

            match self.calculate_twap(pid, now, lookback_seconds) {
                Ok(twap) => {
                    pair_ids.push(pid);
                    twaps.push(twap);
                    lookbacks.push(lookback_seconds);
                }
                Err(PrecompileError::Revert(msg))
                    if msg.contains("no TWAP data") || msg.contains("no VWAP data") => {}
                Err(err) => return Err(err),
            }
        }

        if pair_ids.is_empty() {
            return Err(PrecompileError::Revert(
                "no TWAP data in the requested time range".into(),
            ));
        }

        Ok((pair_ids, twaps, lookbacks))
    }

    /// Calculates VWAPs for all active vote-target pairs over an explicit range.
    pub fn calculate_vwaps(
        &self,
        start_time: u64,
        end_time: u64,
    ) -> Result<(Vec<u32>, Vec<U256>, Vec<u64>)> {
        if start_time >= end_time {
            return Err(PrecompileError::Revert(
                "start_time must be less than end_time".into(),
            ));
        }

        let count = self.pair_count.read()?;
        let lookback = end_time - start_time;
        let mut pair_ids = Vec::new();
        let mut vwaps = Vec::new();
        let mut lookbacks = Vec::new();

        for pid in 1..=count {
            if !self.vote_target.read(&self.pair_at(pid)?)? {
                continue;
            }

            match self.calculate_vwap(pid, start_time, end_time) {
                Ok(vwap) => {
                    pair_ids.push(pid);
                    vwaps.push(vwap);
                    lookbacks.push(lookback);
                }
                Err(PrecompileError::Revert(msg)) if msg.contains("no VWAP data") => {}
                Err(err) => return Err(err),
            }
        }

        if pair_ids.is_empty() {
            return Err(PrecompileError::Revert(
                "no VWAP data in the requested time range".into(),
            ));
        }

        Ok((pair_ids, vwaps, lookbacks))
    }

    /// Calculates VWAPs for the given WorldwideDay window and stores them in oracle state.
    pub fn store_worldwide_day_vwap_snapshot(
        &mut self,
        worldwide_day: WorldwideDay,
        start_time: u64,
        end_time: u64,
    ) -> Result<()> {
        if self.ocomp_profile_ready.read()? {
            let storage = self.storage.clone();
            storage.with_checkpoint(|| {
                self.store_worldwide_day_vwap_snapshot_inner(worldwide_day, start_time, end_time)
            })
        } else {
            self.store_worldwide_day_vwap_snapshot_inner(worldwide_day, start_time, end_time)
        }
    }

    fn store_worldwide_day_vwap_snapshot_inner(
        &mut self,
        worldwide_day: WorldwideDay,
        start_time: u64,
        end_time: u64,
    ) -> Result<()> {
        let (pair_ids, vwaps, _) = self.calculate_vwaps(start_time, end_time)?;
        let next_ocomp_version = self.next_ocomp_state_version()?;

        self.worldwide_day_vwap_exists.write(&worldwide_day, true)?;
        self.worldwide_day_vwap_start
            .write(&worldwide_day, start_time)?;
        self.worldwide_day_vwap_end
            .write(&worldwide_day, end_time)?;
        self.worldwide_day_vwap_pair_count
            .write(&worldwide_day, pair_ids.len() as u32)?;

        let pair_id_map = self.worldwide_day_vwap_pair_id.get_nested(&worldwide_day);
        let value_map = self.worldwide_day_vwap_value.get_nested(&worldwide_day);
        for (idx, (pair_id, vwap)) in pair_ids.iter().zip(vwaps.iter()).enumerate() {
            let i = idx as u32;
            pair_id_map.write(&i, *pair_id)?;
            value_map.write(&i, *vwap)?;
        }

        self.commit_ocomp_state_version(next_ocomp_version)
    }

    /// Computes and persists the VWAP of every active vote-target pair for the
    /// fully-closed UTC calendar day `utc_day` (yyyymmdd UTC — *not* a
    /// WorldwideDay, which is UTC+14). The window is the canonical
    /// `[date_key_to_utc_timestamp(utc_day), +SECONDS_PER_DAY)`.
    ///
    /// Pairs without data for the day are skipped (mirrors `calculate_vwaps`);
    /// if no pair has data, nothing is written, so the day keeps
    /// `pair_count == 0`. Emits one `VwapCalculated` event per written pair in
    /// ascending `pair_id` order. The method overwrites unconditionally — the
    /// caller gates re-finalization via the `utc_day_vwap_last_finalized`
    /// watermark.
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

        let (pair_ids, vwaps) = match self.calculate_vwaps(day_start, day_end) {
            Ok((pair_ids, vwaps, _)) => (pair_ids, vwaps),
            // No vote-target pair had data for the day — leave it unwritten so
            // `pair_count == 0` reads as finalized-empty against the watermark.
            Err(PrecompileError::Revert(msg)) if msg.contains("no VWAP data") => return Ok(()),
            Err(e) => return Err(e),
        };
        let next_ocomp_version = self.next_ocomp_state_version()?;
        let ocomp_pair_id = self
            .ocomp_profile_ready
            .read()?
            .then(|| self.ocomp_day_type_pair_id.read())
            .transpose()?;

        // `pair_ids.len()` is bounded by the registry's u32 `pair_count`, so the
        // conversion is lossless; `unwrap_or` keeps it panic-free per runtime rules.
        let count = u32::try_from(pair_ids.len()).unwrap_or(u32::MAX);
        self.utc_day_vwap_pair_count.write(&utc_day, count)?;
        let pair_id_map = self.utc_day_vwap_pair_id.get_nested(&utc_day);
        let value_map = self.utc_day_vwap_value.get_nested(&utc_day);
        for i in 0..count {
            let pair_id = pair_ids[i as usize];
            let vwap = vwaps[i as usize];
            pair_id_map.write(&i, pair_id)?;
            value_map.write(&i, vwap)?;
            if Some(pair_id) == ocomp_pair_id {
                self.ocomp_day_type_vwap_by_utc_day.write(&utc_day, vwap)?;
            }
            let (_, base, quote) = self.pair_entry(pair_id)?;
            let event = IOracle::VwapCalculated {
                utcDay: utc_day,
                base,
                quote,
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

    /// Returns `(nominal, vwap, max_scurve, source)` for a pair.
    ///
    /// Nominal price follows the Cosmos port rule: `max(VWAP, S-curve)`.
    /// If no VWAP samples exist for the day, VWAP contributes zero.
    pub fn get_nominal_price_components(
        &self,
        pair_id: u32,
        timestamp: u64,
    ) -> Result<(U256, U256, U256, String)> {
        let day_start = crate::scurve::truncate_to_day(timestamp);
        let day_end = day_start.saturating_add(crate::scurve::DAY_SECONDS);
        let vwap = match self.calculate_vwap(pair_id, day_start, day_end) {
            Ok(vwap) => vwap,
            Err(PrecompileError::Revert(msg)) if msg.contains("no VWAP data") => U256::ZERO,
            Err(err) => return Err(err),
        };
        let max_scurve = crate::scurve::get_max_active_scurve_value(self, pair_id, timestamp)?;

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
        pair_id: u32,
        now: u64,
        lookback_seconds: u64,
    ) -> Result<U256> {
        let max_lookback = self.config_lookback_duration.read()?;
        let effective_lookback = lookback_seconds.min(max_lookback);
        let start_time = now.saturating_sub(effective_lookback);
        self.calculate_vwap(pair_id, start_time, now)
    }
}
