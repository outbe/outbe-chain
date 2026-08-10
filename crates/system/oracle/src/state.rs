//! Storage-level reads and writes for Oracle state.
//!
//! CRUD over the pair registry, exchange rates, votes, penalty counters, the
//! price-snapshot ring buffer, and the stored VWAP snapshots. Computation and
//! orchestration live in `runtime.rs`.

use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::{PrecompileError, Result};

use crate::constants::MAX_SNAPSHOT_RETENTION_SECONDS;
use crate::schema::{OracleContract, PairIndex, SCALE_1E18};
use outbe_primitives::units::ONE_COEN;

/// `(exists, bases, quotes, rates, volumes)` — pending aggregate vote.
type AggregateVote = (bool, Vec<Address>, Vec<Address>, Vec<U256>, Vec<U256>);

/// `(snapshot_ids, timestamps, bases, quotes, rates, volumes)` — flattened history.
type SnapshotHistory = (
    Vec<u64>,
    Vec<u64>,
    Vec<Address>,
    Vec<Address>,
    Vec<U256>,
    Vec<U256>,
);

/// `(start_time, end_time, bases, quotes, vwaps, lookbacks)` — stored WWD VWAP snapshot.
type WorldwideDayVwapSnapshot = (u64, u64, Vec<Address>, Vec<Address>, Vec<U256>, Vec<u64>);

/// The reciprocal of a 1e18-scaled rate, in the same scale.
///
/// Zero has no reciprocal and stays zero, so "no rate published" reads the same
/// from either side of the market instead of dividing by zero. A rate above
/// `1e36` floors to zero for the same reason it would in Solidity; nothing that
/// large is a real price.
fn invert_rate(rate: U256) -> U256 {
    if rate.is_zero() {
        U256::ZERO
    } else {
        SCALE_1E18 * ONE_COEN / rate
    }
}

impl OracleContract<'_> {
    // -----------------------------------------------------------------------
    // Pair Registry
    // -----------------------------------------------------------------------

    /// Registers a new trading pair and marks it as a vote target.
    ///
    /// Only the canonical orientation is accepted, so every column keyed by a
    /// pair holds exactly one direction and a backwards quote has a single
    /// well-defined reading. The storage key sorts regardless, so registering
    /// the inverse of an existing pair is rejected as a duplicate. Returns the
    /// assigned enumeration index (1-based).
    pub(crate) fn register_pair(&mut self, pair: AddressPair) -> Result<PairIndex> {
        if pair.address1() == pair.address2() {
            return Err(PrecompileError::Revert(
                "pair base and quote must differ".into(),
            ));
        }
        if !pair.is_canonical() {
            return Err(PrecompileError::Revert(
                "pair should be in the canonical form".into(),
            ));
        }

        if self.pair_to_index.read(&pair)? != 0 {
            return Err(PrecompileError::Revert("pair already registered".into()));
        }

        let count = self.pair_count.read()?;
        let new_index = count + 1;

        self.pair_count.write(new_index)?;
        self.pair_to_index.write(&pair, new_index)?;
        self.pair_by_index.write_pair(&new_index, pair)?;
        self.vote_target.write(&pair, true)?;

        Ok(new_index)
    }

    /// The pair registered at `index`, in canonical orientation.
    ///
    /// Reads back as the zero pair for an index that was never written, which is
    /// why membership is decided by [`Self::pair_index_of`] and never by a zero
    /// check — the zero address is a legitimate asset (native COEN). Callers
    /// iterating `1..=pair_count` have already established the bound; anything
    /// taking an index from outside goes through [`Self::require_pair_at`].
    pub fn pair_at(&self, index: PairIndex) -> Result<AddressPair> {
        self.pair_by_index.read_pair(&index)
    }

    /// [`Self::pair_at`] for an index that arrived from a caller.
    ///
    /// An unwritten index reads back as the zero pair, which is indistinguishable
    /// from a genuine COEN/COEN registration, so the bound is checked rather than
    /// the value.
    pub fn require_pair_at(&self, index: PairIndex) -> Result<AddressPair> {
        if index == 0 || index > self.pair_count.read()? {
            return Err(PrecompileError::Revert("pair index out of range".into()));
        }
        self.pair_at(index)
    }

    /// Deactivates a pair's vote target status (system-only).
    pub fn deactivate_vote_target(
        &mut self,
        caller: Address,
        base: Address,
        quote: Address,
    ) -> Result<()> {
        if caller != Address::ZERO {
            return Err(PrecompileError::Revert(
                "only system can deactivate vote target".into(),
            ));
        }
        let pair = self.require_pair_from(base, quote)?;
        self.vote_target.write(&pair, false)?;
        Ok(())
    }

    /// Activates a pair's vote target status (system-only).
    pub fn activate_vote_target(
        &mut self,
        caller: Address,
        base: Address,
        quote: Address,
    ) -> Result<()> {
        if caller != Address::ZERO {
            return Err(PrecompileError::Revert(
                "only system can activate vote target".into(),
            ));
        }
        let pair = self.require_pair_from(base, quote)?;
        self.vote_target.write(&pair, true)?;
        Ok(())
    }

    /// Removes exchange rates for deactivated pairs.
    pub fn remove_excess_feeds(&mut self) -> Result<()> {
        let pair_count = self.pair_count.read()?;
        for pid in 1..=pair_count {
            let pair = self.pair_at(pid)?;
            let is_target = self.vote_target.read(&pair)?;
            if !is_target {
                self.clear_exchange_rate(pid)?;
            }
        }
        Ok(())
    }

    /// Returns the enumeration index for a pair, or 0 if not registered.
    ///
    /// Order-independent: the lookup key sorts, so this answers the same for
    /// either quote direction.
    pub fn pair_index_of(&self, pair: AddressPair) -> Result<PairIndex> {
        self.pair_to_index.read(&pair)
    }

    /// Resolves an ABI-quoted pair that has to be quoted the way it is stored.
    ///
    /// The registry only ever holds canonical pairs — [`Self::register_pair`]
    /// rejects anything else — so a non-canonical quote names a direction no
    /// storage column is keyed for. Every value read through this resolver is a
    /// bare scalar with no direction of its own (a VWAP, an S-curve peak, a
    /// median input), and none of them is its own reciprocal, so a flipped quote
    /// reverts rather than being reinterpreted.
    pub fn require_pair_from(&self, base: Address, quote: Address) -> Result<AddressPair> {
        let pair = AddressPair::from_addresses(base, quote);
        self.require_pair(pair)
    }

    pub fn require_pair(&self, pair: AddressPair) -> Result<AddressPair> {
        self.require_pair_index(pair)?;
        Ok(pair)
    }

    pub fn require_pair_index(&self, pair: AddressPair) -> Result<PairIndex> {
        if !pair.is_canonical() {
            return Err(PrecompileError::Revert(
                "pair must be quoted in canonical form".into(),
            ));
        }
        let index = self.pair_to_index.read(&pair)?;
        if index == 0 {
            return Err(PrecompileError::Revert("pair not registered".into()));
        }
        Ok(index)
    }

    /// Returns whether a market is an active vote target.
    ///
    /// Direction-insensitive: being a vote target is a property of the market,
    /// not of how a caller quotes it, and answering `false` for a direction
    /// [`Self::get_exchange_rate`] happily prices would be an incoherence a
    /// caller could act on. Being a plain boolean query it returns `false` for an
    /// unregistered market rather than reverting; storage faults still propagate.
    pub fn is_vote_target(&self, base: Address, quote: Address) -> Result<bool> {
        let pair = AddressPair::from_addresses(base, quote);
        if self.pair_to_index.read(&pair)? == 0 {
            return Ok(false);
        }
        self.vote_target.read(&pair)
    }

    // -----------------------------------------------------------------------
    // Exchange Rate Read/Write
    // -----------------------------------------------------------------------

    /// Returns the current exchange rate for a market (1e18 scaled), quoted in
    /// the caller's direction.
    ///
    /// Only the canonical direction is stored, so quoting the market backwards
    /// returns the reciprocal. This is the one read that answers either quote
    /// direction; everything else resolves through [`Self::require_pair_from`].
    pub fn get_exchange_rate(&self, base: Address, quote: Address) -> Result<U256> {
        let pair = AddressPair::from_addresses(base, quote);
        let index = self.require_pair_index(pair.to_canonical())?;
        let stored = self.exchange_rate.read(&index)?;
        let rate = if pair.is_canonical() {
            stored
        } else {
            invert_rate(stored)
        };
        Ok(rate)
    }

    /// [`Self::get_exchange_rate`] together with the block and timestamp of the
    /// stored observation, which are the same for either quote direction.
    pub fn get_exchange_rate_data(
        &self,
        base: Address,
        quote: Address,
    ) -> Result<(U256, u64, u64)> {
        // Both quote directions read the same slot, matching `get_exchange_rate`.
        let index =
            self.require_pair_index(AddressPair::from_addresses(base, quote).to_canonical())?;
        let rate = self.get_exchange_rate(base, quote)?;
        let block = self.exchange_rate_block.read(&index)?;
        let ts = self.exchange_rate_timestamp.read(&index)?;
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
    pub(crate) fn set_exchange_rate(
        &mut self,
        caller: Address,
        pair: AddressPair,
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
        let index = self.require_pair_index(pair)?;
        self.update_exchange_rate(index, rate, block_number, timestamp)
    }

    /// Updates the exchange rate from tally results (internal, no caller check).
    ///
    /// `index` is the registry index of an already-registered pair; the rate is
    /// stored in that pair's canonical orientation.
    pub fn update_exchange_rate(
        &mut self,
        index: PairIndex,
        rate: U256,
        block_number: u64,
        timestamp: u64,
    ) -> Result<()> {
        self.exchange_rate.write(&index, rate)?;
        self.exchange_rate_block.write(&index, block_number)?;
        self.exchange_rate_timestamp.write(&index, timestamp)?;
        Ok(())
    }

    /// Zeroes the three rate columns for one registry index.
    fn clear_exchange_rate(&mut self, index: PairIndex) -> Result<()> {
        self.update_exchange_rate(index, U256::ZERO, 0, 0)
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
            let pair_map = self.vote_pair.get_nested(&voter);
            let rate_map = self.vote_rate.get_nested(&voter);
            let volume_map = self.vote_volume.get_nested(&voter);

            for j in 0..tuple_count {
                // Clears to the zero pair, which is never registered; readers
                // stay bounded by `vote_tuple_count` rather than probing for it.
                pair_map.write_pair(&j, AddressPair::ZERO)?;
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
    /// Each entry is (registered pair, rate, volume). The snapshot is appended at
    /// `snapshot_write_idx` and old entries beyond the retention window are evicted.
    pub fn write_snapshot(
        &mut self,
        timestamp: u64,
        entries: &[(AddressPair, U256, U256)],
    ) -> Result<()> {
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
        entries: &[(AddressPair, U256, U256)],
    ) -> Result<()> {
        let idx = self.snapshot_write_idx.read()?;
        let next_snapshot_idx = idx.checked_add(1).ok_or_else(|| {
            PrecompileError::BodyReadCorruption("Oracle snapshot write index overflow".into())
        })?;
        let next_ocomp_version = self.next_ocomp_state_version()?;

        self.snapshot_timestamp.write(&idx, timestamp)?;
        self.snapshot_pair_count.write(&idx, entries.len() as u32)?;

        let pair_map = self.snapshot_pair.get_nested(&idx);
        let rate_map = self.snapshot_rate.get_nested(&idx);
        let volume_map = self.snapshot_volume.get_nested(&idx);

        for (i, (pair, rate, volume)) in entries.iter().enumerate() {
            let pi = i as u32;
            pair_map.write_pair(&pi, *pair)?;
            rate_map.write(&pi, *rate)?;
            volume_map.write(&pi, *volume)?;
        }

        self.snapshot_write_idx.write(next_snapshot_idx)?;

        let utc_day_ts = timestamp - (timestamp % 86_400);
        for (pair, rate, volume) in entries {
            let vol = if volume.is_zero() {
                SCALE_1E18
            } else {
                *volume
            };
            let pv = rate.checked_mul(vol).unwrap_or(U256::MAX);
            let day_pv = self.daily_pv_sum.get_nested(pair);
            let day_vol = self.daily_vol_sum.get_nested(pair);
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

    /// Returns `(bases, quotes)` of all active vote targets.
    pub fn get_vote_targets(&self) -> Result<(Vec<Address>, Vec<Address>)> {
        let count = self.pair_count.read()?;
        let mut bases = Vec::new();
        let mut quotes = Vec::new();

        for pid in 1..=count {
            let pair = self.pair_at(pid)?;
            let (base, quote) = (pair.address1(), pair.address2());
            if self.vote_target.read(&pair)? {
                bases.push(base);
                quotes.push(quote);
            }
        }

        Ok((bases, quotes))
    }

    /// Returns the pending aggregate vote for a validator.
    ///
    /// Returns `(exists, bases, quotes, rates, volumes)`.
    pub fn get_aggregate_vote(&self, validator: &Address) -> Result<AggregateVote> {
        let exists = self.vote_exists.read(validator)?;
        if !exists {
            return Ok((false, vec![], vec![], vec![], vec![]));
        }

        let tuple_count = self.vote_tuple_count.read(validator)?;
        let pair_map = self.vote_pair.get_nested(validator);
        let rate_map = self.vote_rate.get_nested(validator);
        let volume_map = self.vote_volume.get_nested(validator);

        let mut bases = Vec::with_capacity(tuple_count as usize);
        let mut quotes = Vec::with_capacity(tuple_count as usize);
        let mut rates = Vec::with_capacity(tuple_count as usize);
        let mut volumes = Vec::with_capacity(tuple_count as usize);

        for i in 0..tuple_count {
            let pair = pair_map.read_pair(&i)?;
            bases.push(pair.address1());
            quotes.push(pair.address2());
            rates.push(rate_map.read(&i)?);
            volumes.push(volume_map.read(&i)?);
        }

        Ok((true, bases, quotes, rates, volumes))
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
            let pair_map = self.snapshot_pair.get_nested(&idx);
            let rate_map = self.snapshot_rate.get_nested(&idx);
            let volume_map = self.snapshot_volume.get_nested(&idx);

            for p in 0..pc {
                if pair_map.read_pair(&p)?.same_market(&pair) {
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
            let pair_map = self.snapshot_pair.get_nested(&idx);
            let rate_map = self.snapshot_rate.get_nested(&idx);
            let volume_map = self.snapshot_volume.get_nested(&idx);

            for p in 0..pc {
                let entry = pair_map.read_pair(&p)?;
                snapshot_ids.push(idx);
                timestamps.push(ts);
                bases.push(entry.address1());
                quotes.push(entry.address2());
                rates.push(rate_map.read(&p)?);
                volumes.push(volume_map.read(&p)?);
            }
        }

        Ok((snapshot_ids, timestamps, bases, quotes, rates, volumes))
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

        let pair_map = self.worldwide_day_vwap_pair.get_nested(&worldwide_day);
        let value_map = self.worldwide_day_vwap_value.get_nested(&worldwide_day);
        let mut bases = Vec::with_capacity(pair_count as usize);
        let mut quotes = Vec::with_capacity(pair_count as usize);
        let mut vwaps = Vec::with_capacity(pair_count as usize);
        let mut lookbacks = Vec::with_capacity(pair_count as usize);
        for idx in 0..pair_count {
            let entry = pair_map.read_pair(&idx)?;
            bases.push(entry.address1());
            quotes.push(entry.address2());
            vwaps.push(value_map.read(&idx)?);
            lookbacks.push(lookback);
        }

        Ok((start_time, end_time, bases, quotes, vwaps, lookbacks))
    }

    /// Returns a stored WorldwideDay VWAP for a specific pair, if present.
    pub fn get_worldwide_day_vwap_for_pair(
        &self,
        worldwide_day: WorldwideDay,
        pair: AddressPair,
    ) -> Result<Option<U256>> {
        if !self.worldwide_day_vwap_exists.read(&worldwide_day)? {
            return Ok(None);
        }

        let pair_count = self.worldwide_day_vwap_pair_count.read(&worldwide_day)?;
        let pair_map = self.worldwide_day_vwap_pair.get_nested(&worldwide_day);
        let value_map = self.worldwide_day_vwap_value.get_nested(&worldwide_day);
        for idx in 0..pair_count {
            if pair_map.read_pair(&idx)?.same_market(&pair) {
                return Ok(Some(value_map.read(&idx)?));
            }
        }

        Ok(None)
    }

    /// Returns the finalized per-UTC-day VWAP for `pair` on `utc_day`
    /// (yyyymmdd UTC), or `None` if the day is not finalized or had no data for
    /// that pair. To distinguish "not finalized yet" from "finalized, no data",
    /// compare `utc_day` against `utc_day_vwap_last_finalized`.
    pub fn get_utc_day_vwap_for_pair(
        &self,
        utc_day: u32,
        pair: AddressPair,
    ) -> Result<Option<U256>> {
        let pair_count = self.utc_day_vwap_pair_count.read(&utc_day)?;
        let pair_map = self.utc_day_vwap_pair.get_nested(&utc_day);
        let value_map = self.utc_day_vwap_value.get_nested(&utc_day);
        for idx in 0..pair_count {
            if pair_map.read_pair(&idx)?.same_market(&pair) {
                return Ok(Some(value_map.read(&idx)?));
            }
        }
        Ok(None)
    }

    /// Returns the full finalized VWAP set for `utc_day` as
    /// `(bases, quotes, vwaps)`. All vectors are empty when the day is
    /// unfinalized or had no data.
    pub fn get_utc_day_vwap_snapshot(
        &self,
        utc_day: u32,
    ) -> Result<(Vec<Address>, Vec<Address>, Vec<U256>)> {
        let pair_count = self.utc_day_vwap_pair_count.read(&utc_day)?;
        let pair_map = self.utc_day_vwap_pair.get_nested(&utc_day);
        let value_map = self.utc_day_vwap_value.get_nested(&utc_day);
        let mut bases = Vec::with_capacity(pair_count as usize);
        let mut quotes = Vec::with_capacity(pair_count as usize);
        let mut vwaps = Vec::with_capacity(pair_count as usize);
        for idx in 0..pair_count {
            let entry = pair_map.read_pair(&idx)?;
            bases.push(entry.address1());
            quotes.push(entry.address2());
            vwaps.push(value_map.read(&idx)?);
        }
        Ok((bases, quotes, vwaps))
    }
}
