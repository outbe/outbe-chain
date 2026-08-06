//! Storage-level reads and writes for Oracle state.
//!
//! CRUD over the pair registry, exchange rates, votes, penalty counters, the
//! price-snapshot ring buffer, and the stored VWAP snapshots. Computation and
//! orchestration live in `runtime.rs`.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::error::{PrecompileError, Result};

use crate::constants::MAX_SNAPSHOT_RETENTION_SECONDS;
use crate::schema::{OracleContract, SCALE_1E18};

/// `(exists, pair_ids, rates, volumes)` — pending aggregate vote for a validator.
type AggregateVote = (bool, Vec<u32>, Vec<U256>, Vec<U256>);

/// `(snapshot_ids, timestamps, pair_ids, rates, volumes)` — flattened snapshot history.
type SnapshotHistory = (Vec<u64>, Vec<u64>, Vec<u32>, Vec<U256>, Vec<U256>);

/// `(iso_codes, denoms, denom_hashes, pair_hashes)` — settlement currency metadata.
type SettlementCurrencies = (Vec<u16>, Vec<String>, Vec<B256>, Vec<B256>);

/// `(start_time, end_time, pair_ids, vwaps, lookbacks)` — stored worldwide-day VWAP snapshot.
type WorldwideDayVwapSnapshot = (u64, u64, Vec<u32>, Vec<U256>, Vec<u64>);

impl OracleContract<'_> {
    // -----------------------------------------------------------------------
    // Pair Registry
    // -----------------------------------------------------------------------

    /// Computes pair hash from base/quote strings: `keccak256("BASE/QUOTE")`.
    pub fn pair_hash(base: &str, quote: &str) -> B256 {
        let key = format!("{base}/{quote}");
        keccak256(key.as_bytes())
    }

    /// Registers a new trading pair and marks it as a vote target.
    /// Returns the assigned pair_id (1-indexed).
    pub fn register_pair(&mut self, base: &str, quote: &str) -> Result<u32> {
        // Validate: base and quote must not contain "/" to prevent hash collision
        // (e.g., "A/B","C" and "A","B/C" would both hash to "A/B/C").
        if base.contains('/') || quote.contains('/') {
            return Err(PrecompileError::Revert(
                "pair base/quote must not contain '/' separator".into(),
            ));
        }
        if base.is_empty() || quote.is_empty() {
            return Err(PrecompileError::Revert(
                "pair base/quote must not be empty".into(),
            ));
        }

        let hash = Self::pair_hash(base, quote);

        // Check not already registered
        let existing = self.pair_hash_to_id.read(&hash)?;
        if existing != 0 {
            return Err(PrecompileError::Revert("pair already registered".into()));
        }

        let count = self.pair_count.read()?;
        let new_id = count + 1;

        self.pair_count.write(new_id)?;
        self.pair_id_to_hash.write(&new_id, hash)?;
        self.pair_hash_to_id.write(&hash, new_id)?;
        self.vote_target.write(&hash, true)?;
        self.pair_id_to_base.write_string(&new_id, base)?;
        self.pair_id_to_quote.write_string(&new_id, quote)?;

        Ok(new_id)
    }

    /// Deactivates a pair's vote target status (system-only).
    pub fn deactivate_vote_target(
        &mut self,
        caller: Address,
        base: &str,
        quote: &str,
    ) -> Result<()> {
        if caller != Address::ZERO {
            return Err(PrecompileError::Revert(
                "only system can deactivate vote target".into(),
            ));
        }
        let hash = Self::pair_hash(base, quote);
        let id = self.pair_hash_to_id.read(&hash)?;
        if id == 0 {
            return Err(PrecompileError::Revert("pair not registered".into()));
        }
        self.vote_target.write(&hash, false)?;
        Ok(())
    }

    /// Activates a pair's vote target status (system-only).
    pub fn activate_vote_target(&mut self, caller: Address, base: &str, quote: &str) -> Result<()> {
        if caller != Address::ZERO {
            return Err(PrecompileError::Revert(
                "only system can activate vote target".into(),
            ));
        }
        let hash = Self::pair_hash(base, quote);
        let id = self.pair_hash_to_id.read(&hash)?;
        if id == 0 {
            return Err(PrecompileError::Revert("pair not registered".into()));
        }
        self.vote_target.write(&hash, true)?;
        Ok(())
    }

    /// Removes exchange rates for deactivated pairs.
    pub fn remove_excess_feeds(&mut self) -> Result<()> {
        let pair_count = self.pair_count.read()?;
        for pid in 1..=pair_count {
            let hash = self.pair_id_to_hash.read(&pid)?;
            let is_target = self.vote_target.read(&hash)?;
            if !is_target {
                self.exchange_rate.write(&hash, U256::ZERO)?;
                self.exchange_rate_block.write(&hash, 0)?;
                self.exchange_rate_timestamp.write(&hash, 0)?;
            }
        }
        Ok(())
    }

    /// Returns the pair_id for a base/quote pair, or 0 if not registered.
    pub fn get_pair_id(&self, base: &str, quote: &str) -> Result<u32> {
        let hash = Self::pair_hash(base, quote);
        self.pair_hash_to_id.read(&hash)
    }

    /// [`Self::get_pair_id`] with the "0 means unregistered" revert that every
    /// pair-scoped ABI view repeats.
    pub fn require_pair_id(&self, base: &str, quote: &str) -> Result<u32> {
        let id = self.get_pair_id(base, quote)?;
        if id == 0 {
            return Err(PrecompileError::Revert("pair not registered".into()));
        }
        Ok(id)
    }

    /// Returns whether a pair is an active vote target.
    pub fn is_vote_target(&self, base: &str, quote: &str) -> Result<bool> {
        let hash = Self::pair_hash(base, quote);
        self.vote_target.read(&hash)
    }

    // -----------------------------------------------------------------------
    // Exchange Rate Read/Write
    // -----------------------------------------------------------------------

    /// Returns the current exchange rate for a pair (1e18 scaled).
    pub fn get_exchange_rate(&self, base: &str, quote: &str) -> Result<(U256, u64, u64)> {
        let hash = Self::pair_hash(base, quote);
        let id = self.pair_hash_to_id.read(&hash)?;
        if id == 0 {
            return Err(PrecompileError::Revert("pair not registered".into()));
        }
        let rate = self.exchange_rate.read(&hash)?;
        let block = self.exchange_rate_block.read(&hash)?;
        let ts = self.exchange_rate_timestamp.read(&hash)?;
        Ok((rate, block, ts))
    }

    /// Annualized currency rate (1e18 scaled) for an ISO 4217 code, as pinned
    /// on the reference-currency collection at genesis. Reverts when the code is
    /// not a registered reference currency or carries no (non-zero) rate.
    pub fn get_currency_rate(&self, iso_code: u16) -> Result<U256> {
        let rate = self.reference_currency_rate.read(&iso_code)?;
        if rate.is_zero() {
            return Err(PrecompileError::Revert(format!(
                "no currency rate for iso_code {iso_code}"
            )));
        }
        Ok(rate)
    }

    /// Sets the exchange rate for a pair (system-only bootstrap write).
    pub fn set_exchange_rate(
        &mut self,
        caller: Address,
        base: &str,
        quote: &str,
        rate: U256,
        block_number: u64,
        timestamp: u64,
    ) -> Result<()> {
        // Bootstrap write path: only callable by system (Address::ZERO)
        if caller != Address::ZERO {
            return Err(PrecompileError::Revert(
                "only system can set exchange rate directly".into(),
            ));
        }

        let hash = Self::pair_hash(base, quote);
        let id = self.pair_hash_to_id.read(&hash)?;
        if id == 0 {
            return Err(PrecompileError::Revert("pair not registered".into()));
        }

        self.exchange_rate.write(&hash, rate)?;
        self.exchange_rate_block.write(&hash, block_number)?;
        self.exchange_rate_timestamp.write(&hash, timestamp)?;

        Ok(())
    }

    /// Updates the exchange rate from tally results (internal, no caller check).
    pub fn update_exchange_rate(
        &mut self,
        pair_hash: B256,
        rate: U256,
        block_number: u64,
        timestamp: u64,
    ) -> Result<()> {
        self.exchange_rate.write(&pair_hash, rate)?;
        self.exchange_rate_block.write(&pair_hash, block_number)?;
        self.exchange_rate_timestamp.write(&pair_hash, timestamp)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Feeder Delegation
    // -----------------------------------------------------------------------

    /// Returns the feeder address for a validator. Address::ZERO means self-delegation.
    pub fn get_feeder(&self, validator: &Address) -> Result<Address> {
        let vs = outbe_validatorset::contract::ValidatorSet::new(self.storage.clone());
        vs.get_delegate(
            *validator,
            outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
        )
    }

    /// Delegates feeder consent from validator to feeder.
    pub fn delegate_feeder(&mut self, validator: Address, feeder: Address) -> Result<()> {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(self.storage.clone());
        if feeder.is_zero() {
            return vs.revoke_delegate(
                validator,
                outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
            );
        }
        vs.set_delegate(
            validator,
            outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
            feeder,
        )
    }

    /// Resolves which validator a feeder is acting for.
    /// Returns the validator address if the caller is a valid feeder.
    pub fn resolve_validator_for_feeder(&self, caller: Address) -> Result<Address> {
        let vs = outbe_validatorset::contract::ValidatorSet::new(self.storage.clone());
        vs.resolve_validator_for_role(
            caller,
            outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
        )?
        .ok_or_else(|| PrecompileError::Revert("caller is not an active ORACLE signer".into()))
    }

    /// Clears all votes and resets the voter list. Called after tally.
    pub fn clear_votes(&mut self) -> Result<()> {
        let count = self.voter_list.len()?;

        for i in 0..count {
            let voter = self.voter_list.get(i)?.unwrap_or(Address::ZERO);
            self.vote_exists.write(&voter, false)?;

            let tuple_count = self.vote_tuple_count.read(&voter)?;
            let pair_id_map = self.vote_pair_id.get_nested(&voter);
            let rate_map = self.vote_rate.get_nested(&voter);
            let volume_map = self.vote_volume.get_nested(&voter);

            for j in 0..tuple_count {
                pair_id_map.write(&j, 0)?;
                rate_map.write(&j, U256::ZERO)?;
                volume_map.write(&j, U256::ZERO)?;
            }
            self.vote_tuple_count.write(&voter, 0)?;
        }

        self.voter_list.clear()?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Penalty Counters
    // -----------------------------------------------------------------------

    /// Increments success counter for a validator.
    pub fn increment_success(&mut self, validator: &Address) -> Result<()> {
        let c = self.penalty_success_count.read(validator)?;
        self.penalty_success_count.write(validator, c + 1)
    }

    /// Increments abstain counter for a validator.
    pub fn increment_abstain(&mut self, validator: &Address) -> Result<()> {
        let c = self.penalty_abstain_count.read(validator)?;
        self.penalty_abstain_count.write(validator, c + 1)
    }

    /// Increments miss counter for a validator.
    pub fn increment_miss(&mut self, validator: &Address) -> Result<()> {
        let c = self.penalty_miss_count.read(validator)?;
        self.penalty_miss_count.write(validator, c + 1)
    }

    /// Resets all penalty counters for a validator.
    pub fn reset_penalty_counter(&mut self, validator: &Address) -> Result<()> {
        self.penalty_success_count.write(validator, 0)?;
        self.penalty_abstain_count.write(validator, 0)?;
        self.penalty_miss_count.write(validator, 0)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Price Snapshots (circular buffer)
    // -----------------------------------------------------------------------

    /// Writes a price snapshot with rates/volumes for the given pairs.
    ///
    /// Each entry is (pair_id, rate, volume). The snapshot is appended at
    /// `snapshot_write_idx` and old entries beyond the retention window are evicted.
    pub fn write_snapshot(&mut self, timestamp: u64, entries: &[(u32, U256, U256)]) -> Result<()> {
        if self.ocomp_profile_ready.read()? {
            let storage = self.storage.clone();
            storage.with_checkpoint(|| self.write_snapshot_inner(timestamp, entries))
        } else {
            self.write_snapshot_inner(timestamp, entries)
        }
    }

    fn write_snapshot_inner(
        &mut self,
        timestamp: u64,
        entries: &[(u32, U256, U256)],
    ) -> Result<()> {
        let idx = self.snapshot_write_idx.read()?;
        let next_snapshot_idx = idx.checked_add(1).ok_or_else(|| {
            PrecompileError::BodyReadCorruption("Oracle snapshot write index overflow".into())
        })?;
        let next_ocomp_version = self.next_ocomp_state_version()?;

        self.snapshot_timestamp.write(&idx, timestamp)?;
        self.snapshot_pair_count.write(&idx, entries.len() as u32)?;

        let pair_id_map = self.snapshot_pair_id.get_nested(&idx);
        let rate_map = self.snapshot_rate.get_nested(&idx);
        let volume_map = self.snapshot_volume.get_nested(&idx);

        for (i, (pair_id, rate, volume)) in entries.iter().enumerate() {
            let pi = i as u32;
            pair_id_map.write(&pi, *pair_id)?;
            rate_map.write(&pi, *rate)?;
            volume_map.write(&pi, *volume)?;
        }

        self.snapshot_write_idx.write(next_snapshot_idx)?;

        let utc_day_ts = timestamp - (timestamp % 86_400);
        for (pair_id, rate, volume) in entries {
            let vol = if volume.is_zero() {
                SCALE_1E18
            } else {
                *volume
            };
            let pv = rate.checked_mul(vol).unwrap_or(U256::MAX);
            let day_pv = self.daily_pv_sum.get_nested(pair_id);
            let day_vol = self.daily_vol_sum.get_nested(pair_id);
            let prev_pv = day_pv.read(&utc_day_ts).unwrap_or(U256::ZERO);
            let prev_vol = day_vol.read(&utc_day_ts).unwrap_or(U256::ZERO);
            day_pv.write(&utc_day_ts, prev_pv.saturating_add(pv))?;
            day_vol.write(&utc_day_ts, prev_vol.saturating_add(vol))?;
        }

        // Evict old entries beyond retention window
        self.evict_old_snapshots(timestamp)?;

        self.commit_ocomp_state_version(next_ocomp_version)
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

    // -----------------------------------------------------------------------
    // Bulk Read Views
    // -----------------------------------------------------------------------

    /// Returns all pair exchange rates as parallel arrays.
    ///
    /// Iterates `pair_count` and reads rate / block / timestamp for each pair.
    pub fn get_exchange_rates(&self) -> Result<(Vec<U256>, Vec<u64>, Vec<u64>)> {
        let count = self.pair_count.read()?;
        let mut rates = Vec::with_capacity(count as usize);
        let mut blocks = Vec::with_capacity(count as usize);
        let mut timestamps = Vec::with_capacity(count as usize);

        for pid in 1..=count {
            let hash = self.pair_id_to_hash.read(&pid)?;
            rates.push(self.exchange_rate.read(&hash)?);
            blocks.push(self.exchange_rate_block.read(&hash)?);
            timestamps.push(self.exchange_rate_timestamp.read(&hash)?);
        }

        Ok((rates, blocks, timestamps))
    }

    /// Returns pair_ids of all active vote targets.
    pub fn get_vote_targets(&self) -> Result<Vec<u32>> {
        let count = self.pair_count.read()?;
        let mut pair_ids = Vec::new();

        for pid in 1..=count {
            let hash = self.pair_id_to_hash.read(&pid)?;
            if self.vote_target.read(&hash)? {
                pair_ids.push(pid);
            }
        }

        Ok(pair_ids)
    }

    /// Returns the pending aggregate vote for a validator.
    ///
    /// Returns `(exists, pair_ids, rates, volumes)`.
    pub fn get_aggregate_vote(&self, validator: &Address) -> Result<AggregateVote> {
        let exists = self.vote_exists.read(validator)?;
        if !exists {
            return Ok((false, vec![], vec![], vec![]));
        }

        let tuple_count = self.vote_tuple_count.read(validator)?;
        let pair_id_map = self.vote_pair_id.get_nested(validator);
        let rate_map = self.vote_rate.get_nested(validator);
        let volume_map = self.vote_volume.get_nested(validator);

        let mut pair_ids = Vec::with_capacity(tuple_count as usize);
        let mut rates = Vec::with_capacity(tuple_count as usize);
        let mut volumes = Vec::with_capacity(tuple_count as usize);

        for i in 0..tuple_count {
            pair_ids.push(pair_id_map.read(&i)?);
            rates.push(rate_map.read(&i)?);
            volumes.push(volume_map.read(&i)?);
        }

        Ok((true, pair_ids, rates, volumes))
    }

    /// Returns slash window progress for a validator.
    ///
    /// Returns `(success, abstain, miss, slash_window)`.
    pub fn get_slash_window_progress(&self, validator: &Address) -> Result<(u64, u64, u64, u64)> {
        let success = self.penalty_success_count.read(validator)?;
        let abstain = self.penalty_abstain_count.read(validator)?;
        let miss = self.penalty_miss_count.read(validator)?;
        let slash_window = self.config_slash_window.read()?;
        Ok((success, abstain, miss, slash_window))
    }

    /// Returns price snapshot history for a pair (most recent first).
    ///
    /// Returns `(timestamps, rates, volumes)` as parallel arrays,
    /// up to `count` entries.
    pub fn get_price_snapshot_history(
        &self,
        pair_id: u32,
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
            let pair_id_map = self.snapshot_pair_id.get_nested(&idx);
            let rate_map = self.snapshot_rate.get_nested(&idx);
            let volume_map = self.snapshot_volume.get_nested(&idx);

            for p in 0..pc {
                if pair_id_map.read(&p)? == pair_id {
                    timestamps.push(ts);
                    rates.push(rate_map.read(&p)?);
                    volumes.push(volume_map.read(&p)?);
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
        let mut pair_ids = Vec::new();
        let mut rates = Vec::new();
        let mut volumes = Vec::new();

        let mut snapshots_seen = 0u32;
        let mut idx = write_idx;
        while idx > oldest_idx && snapshots_seen < count {
            idx -= 1;
            snapshots_seen += 1;

            let ts = self.snapshot_timestamp.read(&idx)?;
            let pc = self.snapshot_pair_count.read(&idx)?;
            let pair_id_map = self.snapshot_pair_id.get_nested(&idx);
            let rate_map = self.snapshot_rate.get_nested(&idx);
            let volume_map = self.snapshot_volume.get_nested(&idx);

            for p in 0..pc {
                snapshot_ids.push(idx);
                timestamps.push(ts);
                pair_ids.push(pair_id_map.read(&p)?);
                rates.push(rate_map.read(&p)?);
                volumes.push(volume_map.read(&p)?);
            }
        }

        Ok((snapshot_ids, timestamps, pair_ids, rates, volumes))
    }

    /// Returns a stored WorldwideDay VWAP snapshot.
    pub fn get_worldwide_day_vwap_snapshot(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<WorldwideDayVwapSnapshot> {
        if !self.worldwide_day_vwap_exists.read(&worldwide_day)? {
            return Err(PrecompileError::Revert(
                "worldwide day VWAP snapshot not found".into(),
            ));
        }

        let start_time = self.worldwide_day_vwap_start.read(&worldwide_day)?;
        let end_time = self.worldwide_day_vwap_end.read(&worldwide_day)?;
        let pair_count = self.worldwide_day_vwap_pair_count.read(&worldwide_day)?;
        let lookback = end_time.saturating_sub(start_time);

        let pair_id_map = self.worldwide_day_vwap_pair_id.get_nested(&worldwide_day);
        let value_map = self.worldwide_day_vwap_value.get_nested(&worldwide_day);
        let mut pair_ids = Vec::with_capacity(pair_count as usize);
        let mut vwaps = Vec::with_capacity(pair_count as usize);
        let mut lookbacks = Vec::with_capacity(pair_count as usize);
        for idx in 0..pair_count {
            pair_ids.push(pair_id_map.read(&idx)?);
            vwaps.push(value_map.read(&idx)?);
            lookbacks.push(lookback);
        }

        Ok((start_time, end_time, pair_ids, vwaps, lookbacks))
    }

    /// Returns a stored WorldwideDay VWAP for a specific pair id, if present.
    pub fn get_worldwide_day_vwap_for_pair_id(
        &self,
        worldwide_day: WorldwideDay,
        pair_id: u32,
    ) -> Result<Option<U256>> {
        if !self.worldwide_day_vwap_exists.read(&worldwide_day)? {
            return Ok(None);
        }

        let pair_count = self.worldwide_day_vwap_pair_count.read(&worldwide_day)?;
        let pair_id_map = self.worldwide_day_vwap_pair_id.get_nested(&worldwide_day);
        let value_map = self.worldwide_day_vwap_value.get_nested(&worldwide_day);
        for idx in 0..pair_count {
            if pair_id_map.read(&idx)? == pair_id {
                return Ok(Some(value_map.read(&idx)?));
            }
        }

        Ok(None)
    }

    /// Returns the finalized per-UTC-day VWAP for `pair_id` on `utc_day`
    /// (yyyymmdd UTC), or `None` if the day is not finalized or had no data for
    /// that pair. To distinguish "not finalized yet" from "finalized, no data",
    /// compare `utc_day` against `utc_day_vwap_last_finalized`.
    pub fn get_utc_day_vwap_for_pair_id(&self, utc_day: u32, pair_id: u32) -> Result<Option<U256>> {
        let pair_count = self.utc_day_vwap_pair_count.read(&utc_day)?;
        let pair_id_map = self.utc_day_vwap_pair_id.get_nested(&utc_day);
        let value_map = self.utc_day_vwap_value.get_nested(&utc_day);
        for idx in 0..pair_count {
            if pair_id_map.read(&idx)? == pair_id {
                return Ok(Some(value_map.read(&idx)?));
            }
        }
        Ok(None)
    }

    /// Returns the full finalized VWAP set for `utc_day` as
    /// `(pair_ids, vwaps)`. Both vectors are empty when the day is unfinalized
    /// or had no data.
    pub fn get_utc_day_vwap_snapshot(&self, utc_day: u32) -> Result<(Vec<u32>, Vec<U256>)> {
        let pair_count = self.utc_day_vwap_pair_count.read(&utc_day)?;
        let pair_id_map = self.utc_day_vwap_pair_id.get_nested(&utc_day);
        let value_map = self.utc_day_vwap_value.get_nested(&utc_day);
        let mut pair_ids = Vec::with_capacity(pair_count as usize);
        let mut vwaps = Vec::with_capacity(pair_count as usize);
        for idx in 0..pair_count {
            pair_ids.push(pair_id_map.read(&idx)?);
            vwaps.push(value_map.read(&idx)?);
        }
        Ok((pair_ids, vwaps))
    }

    /// Returns all registered pairs as parallel arrays of
    /// (pair_ids, bases, quotes, is_active).
    #[allow(clippy::type_complexity)] // parallel-array view getter; the tuple IS the ABI shape
    pub fn get_pairs(&self) -> Result<(Vec<u32>, Vec<String>, Vec<String>, Vec<bool>)> {
        let count = self.pair_count.read()?;
        let mut pair_ids = Vec::with_capacity(count as usize);
        let mut bases = Vec::with_capacity(count as usize);
        let mut quotes = Vec::with_capacity(count as usize);
        let mut is_active = Vec::with_capacity(count as usize);

        for pid in 1..=count {
            let hash = self.pair_id_to_hash.read(&pid)?;
            pair_ids.push(pid);
            bases.push(self.pair_id_to_base.read_string(&pid)?);
            quotes.push(self.pair_id_to_quote.read_string(&pid)?);
            is_active.push(self.vote_target.read(&hash)?);
        }

        Ok((pair_ids, bases, quotes, is_active))
    }

    /// Returns all settlement currency metadata as parallel arrays.
    pub fn get_settlement_currencies(&self) -> Result<SettlementCurrencies> {
        let count = self.settlement_count.read()?;
        let mut iso_codes = Vec::with_capacity(count as usize);
        let mut denoms = Vec::with_capacity(count as usize);
        let mut denom_hashes = Vec::with_capacity(count as usize);
        let mut pair_hashes = Vec::with_capacity(count as usize);

        for idx in 0..count {
            let iso_code = self.settlement_index_to_iso.read(&idx)?;
            iso_codes.push(iso_code);
            denoms.push(self.settlement_iso_to_denom_string.read_string(&iso_code)?);
            denom_hashes.push(self.settlement_iso_to_denom.read(&iso_code)?);
            pair_hashes.push(self.settlement_iso_to_pair.read(&iso_code)?);
        }

        Ok((iso_codes, denoms, denom_hashes, pair_hashes))
    }
}
