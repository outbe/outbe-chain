use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_common::WorldwideDay;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::time::{date_key_to_utc_timestamp, SECONDS_PER_DAY};
use std::collections::HashSet;

use crate::contract::{OracleContract, SCALE_1E18};
use crate::precompile::IOracle;

/// `(exists, pair_ids, rates, volumes)` — pending aggregate vote for a validator.
type AggregateVote = (bool, Vec<u32>, Vec<U256>, Vec<U256>);

/// `(snapshot_ids, timestamps, pair_ids, rates, volumes)` — flattened snapshot history.
type SnapshotHistory = (Vec<u64>, Vec<u64>, Vec<u32>, Vec<U256>, Vec<U256>);

/// `(iso_codes, denoms, denom_hashes, pair_hashes)` — settlement currency metadata.
type SettlementCurrencies = (Vec<u16>, Vec<String>, Vec<B256>, Vec<B256>);

/// `(start_time, end_time, pair_ids, vwaps, lookbacks)` — stored worldwide-day VWAP snapshot.
type WorldwideDayVwapSnapshot = (u64, u64, Vec<u32>, Vec<U256>, Vec<u64>);

// ---------------------------------------------------------------------------
// Genesis Configuration
// ---------------------------------------------------------------------------

/// A price snapshot entry for genesis import/export.
#[derive(Clone, Debug)]
pub struct GenesisSnapshot {
    /// Unix timestamp of the snapshot.
    pub timestamp: u64,
    /// Entries as `(pair_id, rate_1e18, volume_1e18)`.
    pub entries: Vec<(u32, U256, U256)>,
}

/// An S-curve entry for genesis import/export.
#[derive(Clone, Debug)]
pub struct GenesisScurveEntry {
    /// Pair identifier (1-indexed).
    pub pair_id: u32,
    /// UTC midnight timestamp of the peak day.
    pub peak_day: u64,
    /// Peak price at 1e18 scale.
    pub peak_price: U256,
}

/// A pending aggregate vote for genesis import/export.
#[derive(Clone, Debug)]
pub struct GenesisAggregateVote {
    /// Validator address that owns this pending vote.
    pub validator: Address,
    /// Entries as `(pair_id, rate_1e18, volume_1e18)`.
    pub entries: Vec<(u32, U256, U256)>,
}

/// Configurable genesis parameters for the Oracle contract.
///
/// All `U256` values use the 1e18 scale factor (`SCALE_1E18`).
pub struct OracleGenesisConfig {
    /// Vote period in blocks (default: 2).
    pub vote_period: u64,
    /// Reward band width (1e18 scaled, default: 0.02 * 1e18 = 2e16).
    pub reward_band: U256,
    /// Slash window in blocks (default: 96).
    pub slash_window: u64,
    /// Minimum valid-vote ratio per window (1e18 scaled, default: 0.05 * 1e18 = 5e16).
    pub min_valid_per_window: U256,
    /// Slash fraction (1e18 scaled, default: 0).
    pub slash_fraction: U256,
    /// Lookback duration in seconds for VWAP (default: 86400).
    pub lookback_duration: u64,
    /// Trading pairs to register at genesis as `(base, quote)`.
    pub pairs: Vec<(String, String)>,
    /// Initial exchange rates as `(base, quote, rate_1e18)`.
    pub initial_rates: Vec<(String, String, U256)>,
    /// Feeder delegations as `(validator, feeder)`.
    pub feeder_delegations: Vec<(Address, Address)>,
    /// Settlement currencies as `(iso_code, denom, pair_base, pair_quote)`.
    /// iso_code: ISO 4217 numeric code (e.g., 840 = USD).
    /// denom: stablecoin denom string (e.g., "0xUSD").
    /// pair_base/pair_quote: trading pair for this settlement currency.
    pub settlement_currencies: Vec<(u16, String, String, String)>,
    /// Reference currencies as ISO 4217 numeric codes (e.g., 840 = USD).
    /// These codes identify currencies that are valid for off-chain pricing
    /// references. Pre-filled at genesis with `[840]`.
    pub reference_currencies: Vec<u16>,
    /// Penalty counters as `(validator, success, abstain, miss)`.
    pub penalty_counters: Vec<(Address, u64, u64, u64)>,
    /// Pending aggregate votes that have not yet been tallied.
    pub aggregate_votes: Vec<GenesisAggregateVote>,
    /// Price snapshots for the circular buffer.
    pub snapshots: Vec<GenesisSnapshot>,
    /// Active S-curve entries.
    pub scurve_entries: Vec<GenesisScurveEntry>,
    /// Validators protected from slashing.
    pub protected_validators: Vec<Address>,
}

impl OracleGenesisConfig {
    /// Returns the config that matches the current hard-coded genesis values.
    pub fn default_config() -> Self {
        Self {
            vote_period: 2,
            reward_band: U256::from(20_000_000_000_000_000u128), // 0.02
            slash_window: 96,
            min_valid_per_window: U256::from(50_000_000_000_000_000u128), // 0.05
            slash_fraction: U256::ZERO,
            lookback_duration: 86400,
            pairs: vec![("COEN".into(), "0xUSD".into())],
            initial_rates: vec![],
            feeder_delegations: vec![],
            settlement_currencies: vec![],
            reference_currencies: vec![840],
            penalty_counters: vec![],
            aggregate_votes: vec![],
            snapshots: vec![],
            scurve_entries: vec![],
            protected_validators: vec![],
        }
    }
}

/// Initializes all oracle state from a genesis configuration.
///
/// Writes config slots, registers pairs, sets initial exchange rates, and
/// records feeder delegations. The oracle is marked as enabled and
/// initialized on success.
pub fn init_from_genesis(oracle: &mut OracleContract, config: &OracleGenesisConfig) -> Result<()> {
    // Idempotency guard: skip if already initialized (safe for block 0 replay).
    if oracle.config_is_initialized.read()? {
        return Ok(());
    }

    // Validate config parameters
    if config.vote_period == 0 {
        return Err(PrecompileError::Revert("vote_period must be > 0".into()));
    }
    if config.slash_window == 0 {
        return Err(PrecompileError::Revert("slash_window must be > 0".into()));
    }
    if config.lookback_duration > MAX_SNAPSHOT_RETENTION_SECONDS {
        return Err(PrecompileError::Revert(
            "lookback_duration exceeds snapshot retention window".into(),
        ));
    }

    oracle.config_vote_period.write(config.vote_period)?;
    oracle.config_reward_band.write(config.reward_band)?;
    oracle.config_slash_window.write(config.slash_window)?;
    oracle
        .config_min_valid_per_window
        .write(config.min_valid_per_window)?;
    oracle.config_slash_fraction.write(config.slash_fraction)?;
    oracle
        .config_lookback_duration
        .write(config.lookback_duration)?;

    // Register trading pairs.
    for (base, quote) in &config.pairs {
        oracle.register_pair(base, quote)?;
    }

    // Set initial exchange rates (system caller = Address::ZERO).
    for (base, quote, rate) in &config.initial_rates {
        oracle.set_exchange_rate(Address::ZERO, base, quote, *rate, 0, 0)?;
    }

    // Record feeder delegations.
    for (validator, feeder) in &config.feeder_delegations {
        oracle.feeder_delegation.write(validator, *feeder)?;
    }

    // Import settlement currencies.
    for (iso_code, denom, pair_base, pair_quote) in &config.settlement_currencies {
        if *iso_code == 0 {
            return Err(PrecompileError::Revert(
                "settlement iso_code must be non-zero".into(),
            ));
        }
        if denom.is_empty() {
            return Err(PrecompileError::Revert(
                "settlement denom must not be empty".into(),
            ));
        }

        let pair_hash = OracleContract::pair_hash(pair_base, pair_quote);
        let pair_id = oracle.pair_hash_to_id.read(&pair_hash)?;
        if pair_id == 0 {
            return Err(PrecompileError::Revert(
                "settlement pair must be registered".into(),
            ));
        }

        let existing_denom_hash = oracle.settlement_iso_to_denom.read(iso_code)?;
        if existing_denom_hash != B256::ZERO {
            return Err(PrecompileError::Revert(
                "settlement iso_code already registered".into(),
            ));
        }

        let denom_hash = keccak256(denom.as_bytes());
        let count = oracle.settlement_count.read()?;
        oracle.settlement_iso_to_denom.write(iso_code, denom_hash)?;
        oracle.settlement_iso_to_pair.write(iso_code, pair_hash)?;
        oracle.settlement_index_to_iso.write(&count, *iso_code)?;
        oracle
            .settlement_iso_to_denom_string
            .write_string(iso_code, denom)?;
        oracle.settlement_count.write(count + 1)?;
    }

    // Import reference currencies.
    let mut seen_reference_iso: std::collections::BTreeSet<u16> = std::collections::BTreeSet::new();
    for iso_code in &config.reference_currencies {
        if *iso_code == 0 {
            return Err(PrecompileError::Revert(
                "reference iso_code must be non-zero".into(),
            ));
        }
        if !seen_reference_iso.insert(*iso_code) {
            return Err(PrecompileError::Revert(format!(
                "duplicate reference iso_code: {iso_code}"
            )));
        }
        oracle.reference_currencies.push(*iso_code)?;
    }

    // Import penalty counters.
    for (validator, success, abstain, miss) in &config.penalty_counters {
        oracle.penalty_success_count.write(validator, *success)?;
        oracle.penalty_abstain_count.write(validator, *abstain)?;
        oracle.penalty_miss_count.write(validator, *miss)?;
    }

    // Import pending aggregate votes.
    import_aggregate_votes(oracle, &config.aggregate_votes)?;

    // Import price snapshots into the circular buffer.
    for snapshot in &config.snapshots {
        oracle.write_snapshot(snapshot.timestamp, &snapshot.entries)?;
    }

    // Import S-curve entries.
    for entry in &config.scurve_entries {
        crate::scurve::store_scurve_entry(oracle, entry.pair_id, entry.peak_day, entry.peak_price)?;
    }

    // Import protected validators.
    if !config.protected_validators.is_empty() {
        oracle.config_allow_protected.write(true)?;
        for validator in &config.protected_validators {
            oracle.protected_validator.write(validator, true)?;
        }
    }

    oracle.config_enabled.write(true)?;
    oracle.config_is_initialized.write(true)?;

    Ok(())
}

/// Exports the full oracle state into an `OracleGenesisConfig`.
///
/// This reads all config slots, pair registry, exchange rates, delegations,
/// penalty counters, pending aggregate votes, snapshots, S-curve entries, and
/// protected validators.
///
/// The exported config can be used to re-initialize a fresh oracle via
/// `init_from_genesis`, enabling full state migration.
pub fn export_genesis(
    oracle: &OracleContract,
    validators: &[Address],
) -> Result<OracleGenesisConfig> {
    let vote_period = oracle.config_vote_period.read()?;
    let reward_band = oracle.config_reward_band.read()?;
    let slash_window = oracle.config_slash_window.read()?;
    let min_valid_per_window = oracle.config_min_valid_per_window.read()?;
    let slash_fraction = oracle.config_slash_fraction.read()?;
    let lookback_duration = oracle.config_lookback_duration.read()?;

    // Export pairs and non-zero initial rates.
    let pair_count = oracle.pair_count.read()?;
    let mut pairs = Vec::with_capacity(pair_count as usize);
    let mut initial_rates = Vec::new();

    for pair_id in 1..=pair_count {
        let (base, quote, pair_hash) = export_pair_metadata(oracle, pair_id)?;
        let rate = oracle.exchange_rate.read(&pair_hash)?;
        if !rate.is_zero() {
            initial_rates.push((base.clone(), quote.clone(), rate));
        }
        pairs.push((base, quote));
    }

    // Export feeder delegations.
    let mut feeder_delegations = Vec::new();
    for validator in validators {
        let feeder = oracle.feeder_delegation.read(validator)?;
        if feeder != Address::ZERO {
            feeder_delegations.push((*validator, feeder));
        }
    }

    // Export penalty counters.
    let mut penalty_counters = Vec::new();
    for validator in validators {
        let success = oracle.penalty_success_count.read(validator)?;
        let abstain = oracle.penalty_abstain_count.read(validator)?;
        let miss = oracle.penalty_miss_count.read(validator)?;
        if success > 0 || abstain > 0 || miss > 0 {
            penalty_counters.push((*validator, success, abstain, miss));
        }
    }

    // Export pending aggregate votes.
    let aggregate_votes = export_aggregate_votes(oracle)?;

    // Export snapshots.
    let write_idx = oracle.snapshot_write_idx.read()?;
    let oldest_idx = oracle.snapshot_oldest_idx.read()?;
    let mut snapshots = Vec::new();
    for idx in oldest_idx..write_idx {
        let timestamp = oracle.snapshot_timestamp.read(&idx)?;
        let pc = oracle.snapshot_pair_count.read(&idx)?;
        let pair_id_map = oracle.snapshot_pair_id.get_nested(&idx);
        let rate_map = oracle.snapshot_rate.get_nested(&idx);
        let volume_map = oracle.snapshot_volume.get_nested(&idx);

        let mut entries = Vec::with_capacity(pc as usize);
        for p in 0..pc {
            let pid = pair_id_map.read(&p)?;
            let rate = rate_map.read(&p)?;
            let volume = volume_map.read(&p)?;
            entries.push((pid, rate, volume));
        }
        snapshots.push(GenesisSnapshot { timestamp, entries });
    }

    // Export S-curve entries.
    let scurve_count = oracle.scurve_count.read()?;
    let scurve_oldest = oracle.scurve_oldest_idx.read()?;
    let mut scurve_entries = Vec::new();
    for idx in scurve_oldest..scurve_count {
        let pair_id = oracle.scurve_pair_id.read(&idx)?;
        let peak_day = oracle.scurve_peak_day.read(&idx)?;
        let peak_price = oracle.scurve_peak_price.read(&idx)?;
        scurve_entries.push(GenesisScurveEntry {
            pair_id,
            peak_day,
            peak_price,
        });
    }

    // Export protected validators.
    let allow_protected = oracle.config_allow_protected.read()?;
    let mut protected_validators = Vec::new();
    if allow_protected {
        for validator in validators {
            let is_protected = oracle.protected_validator.read(validator)?;
            if is_protected {
                protected_validators.push(*validator);
            }
        }
    }

    // Export settlement currencies.
    let settlement_count = oracle.settlement_count.read()?;
    let mut settlement_currencies = Vec::with_capacity(settlement_count as usize);
    for idx in 0..settlement_count {
        let iso_code = oracle.settlement_index_to_iso.read(&idx)?;
        if iso_code == 0 {
            return Err(PrecompileError::Revert(format!(
                "missing settlement iso metadata at index {idx}"
            )));
        }

        let denom = oracle
            .settlement_iso_to_denom_string
            .read_string(&iso_code)?;
        if denom.is_empty() {
            return Err(PrecompileError::Revert(format!(
                "missing settlement denom metadata for iso_code {iso_code}"
            )));
        }

        let denom_hash = oracle.settlement_iso_to_denom.read(&iso_code)?;
        if denom_hash != keccak256(denom.as_bytes()) {
            return Err(PrecompileError::Revert(format!(
                "settlement denom metadata hash mismatch for iso_code {iso_code}"
            )));
        }

        let pair_hash = oracle.settlement_iso_to_pair.read(&iso_code)?;
        let pair_id = oracle.pair_hash_to_id.read(&pair_hash)?;
        if pair_id == 0 {
            return Err(PrecompileError::Revert(format!(
                "settlement pair metadata missing for iso_code {iso_code}"
            )));
        }
        let (base, quote, _) = export_pair_metadata(oracle, pair_id)?;
        settlement_currencies.push((iso_code, denom, base, quote));
    }

    // Export reference currencies (bounded list of fiat codes; read_all OK).
    let reference_currencies = oracle.reference_currencies.read_all()?;

    Ok(OracleGenesisConfig {
        vote_period,
        reward_band,
        slash_window,
        min_valid_per_window,
        slash_fraction,
        lookback_duration,
        pairs,
        initial_rates,
        feeder_delegations,
        settlement_currencies,
        reference_currencies,
        penalty_counters,
        aggregate_votes,
        snapshots,
        scurve_entries,
        protected_validators,
    })
}

/// Maximum number of snapshots to retain (approximately 1 year at 2-block vote
/// period with 12-second blocks: ~1.3M snapshots).
const MAX_SNAPSHOT_RETENTION_SECONDS: u64 = 365 * 24 * 3600;

/// Maximum number of closed UTC days the begin-block lifecycle finalizes in a
/// single block. Normal operation finalizes exactly one day per UTC-midnight
/// rollover; this cap only bounds catch-up after a long gap (cold start or
/// extended downtime). Days older than the cap stay unfinalized — their source
/// aggregates are evicted past `MAX_SNAPSHOT_RETENTION_SECONDS` anyway, so they
/// could not be recomputed regardless.
pub const MAX_UTC_DAY_VWAP_BACKFILL_DAYS: u32 = 366;

fn import_aggregate_votes(
    oracle: &mut OracleContract,
    aggregate_votes: &[GenesisAggregateVote],
) -> Result<()> {
    let pair_count = oracle.pair_count.read()?;
    let mut seen_validators = HashSet::with_capacity(aggregate_votes.len());
    let mut validated_votes = Vec::with_capacity(aggregate_votes.len());

    for vote in aggregate_votes {
        if vote.validator == Address::ZERO {
            return Err(PrecompileError::Revert(
                "aggregate vote validator must be non-zero".into(),
            ));
        }
        if !seen_validators.insert(vote.validator) {
            return Err(PrecompileError::Revert(
                "duplicate aggregate vote validator".into(),
            ));
        }
        if oracle.vote_exists.read(&vote.validator)? {
            return Err(PrecompileError::Revert(
                "aggregate vote already exists for validator".into(),
            ));
        }
        if vote.entries.len() > u32::MAX as usize || vote.entries.len() as u32 > pair_count {
            return Err(PrecompileError::Revert(
                "aggregate vote tuple count exceeds registered pair count".into(),
            ));
        }

        let mut seen_pairs = HashSet::with_capacity(vote.entries.len());
        for (pair_id, _, _) in &vote.entries {
            if *pair_id == 0 || *pair_id > pair_count {
                return Err(PrecompileError::Revert(
                    "aggregate vote pair_id must be registered".into(),
                ));
            }
            if !seen_pairs.insert(*pair_id) {
                return Err(PrecompileError::Revert(
                    "duplicate pair in aggregate vote".into(),
                ));
            }

            let pair_hash = oracle.pair_id_to_hash.read(pair_id)?;
            if pair_hash == B256::ZERO {
                return Err(PrecompileError::Revert(
                    "aggregate vote pair metadata missing".into(),
                ));
            }
            if !oracle.vote_target.read(&pair_hash)? {
                return Err(PrecompileError::Revert(
                    "aggregate vote pair is not a vote target".into(),
                ));
            }
        }

        validated_votes.push((vote.validator, vote.entries.clone()));
    }

    for (validator, entries) in validated_votes {
        oracle.vote_exists.write(&validator, true)?;
        oracle
            .vote_tuple_count
            .write(&validator, entries.len() as u32)?;

        let pair_id_map = oracle.vote_pair_id.get_nested(&validator);
        let rate_map = oracle.vote_rate.get_nested(&validator);
        let volume_map = oracle.vote_volume.get_nested(&validator);

        for (idx, (pair_id, rate, volume)) in entries.into_iter().enumerate() {
            let idx = idx as u32;
            pair_id_map.write(&idx, pair_id)?;
            rate_map.write(&idx, rate)?;
            volume_map.write(&idx, volume)?;
        }

        oracle.voter_list.push(validator)?;
    }

    Ok(())
}

fn export_aggregate_votes(oracle: &OracleContract) -> Result<Vec<GenesisAggregateVote>> {
    let voter_count = oracle.voter_list.len()?;
    let mut seen_validators = HashSet::with_capacity(voter_count as usize);
    let mut aggregate_votes = Vec::with_capacity(voter_count as usize);

    for voter_idx in 0..voter_count {
        let validator = oracle.voter_list.get(voter_idx)?.ok_or_else(|| {
            PrecompileError::Revert(format!(
                "missing aggregate vote validator at voter index {voter_idx}"
            ))
        })?;

        if validator == Address::ZERO {
            return Err(PrecompileError::Revert(
                "aggregate vote validator must be non-zero".into(),
            ));
        }
        if !seen_validators.insert(validator) {
            return Err(PrecompileError::Revert(
                "duplicate aggregate vote validator".into(),
            ));
        }
        if !oracle.vote_exists.read(&validator)? {
            return Err(PrecompileError::Revert(
                "voter list contains validator without aggregate vote".into(),
            ));
        }

        let tuple_count = oracle.vote_tuple_count.read(&validator)?;
        let pair_id_map = oracle.vote_pair_id.get_nested(&validator);
        let rate_map = oracle.vote_rate.get_nested(&validator);
        let volume_map = oracle.vote_volume.get_nested(&validator);
        let mut seen_pairs = HashSet::with_capacity(tuple_count as usize);
        let mut entries = Vec::with_capacity(tuple_count as usize);

        for tuple_idx in 0..tuple_count {
            let pair_id = pair_id_map.read(&tuple_idx)?;
            if pair_id == 0 {
                return Err(PrecompileError::Revert(
                    "aggregate vote pair_id must be registered".into(),
                ));
            }
            let pair_hash = oracle.pair_id_to_hash.read(&pair_id)?;
            if pair_hash == B256::ZERO {
                return Err(PrecompileError::Revert(
                    "aggregate vote pair metadata missing".into(),
                ));
            }
            if !seen_pairs.insert(pair_id) {
                return Err(PrecompileError::Revert(
                    "duplicate pair in aggregate vote".into(),
                ));
            }
            entries.push((
                pair_id,
                rate_map.read(&tuple_idx)?,
                volume_map.read(&tuple_idx)?,
            ));
        }

        aggregate_votes.push(GenesisAggregateVote { validator, entries });
    }

    Ok(aggregate_votes)
}

fn export_pair_metadata(oracle: &OracleContract, pair_id: u32) -> Result<(String, String, B256)> {
    let pair_hash = oracle.pair_id_to_hash.read(&pair_id)?;
    if pair_hash == B256::ZERO {
        return Err(PrecompileError::Revert(format!(
            "missing pair hash for pair_id {pair_id}"
        )));
    }

    let base = oracle.pair_id_to_base.read_string(&pair_id)?;
    let quote = oracle.pair_id_to_quote.read_string(&pair_id)?;
    if base.is_empty() || quote.is_empty() {
        return Err(PrecompileError::Revert(format!(
            "missing pair string metadata for pair_id {pair_id}"
        )));
    }

    if OracleContract::pair_hash(&base, &quote) != pair_hash {
        return Err(PrecompileError::Revert(format!(
            "pair string metadata hash mismatch for pair_id {pair_id}"
        )));
    }

    Ok((base, quote, pair_hash))
}

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
        self.feeder_delegation.read(validator)
    }

    /// Delegates feeder consent from validator to feeder.
    pub fn delegate_feeder(&mut self, validator: Address, feeder: Address) -> Result<()> {
        // Verify caller is a registered validator (cross-call)
        let vs = outbe_validatorset::contract::ValidatorSet::new(self.storage.clone());
        let info = vs.get_validator(validator)?;
        if info.is_none() {
            return Err(PrecompileError::Revert("not a registered validator".into()));
        }

        self.feeder_delegation.write(&validator, feeder)?;
        Ok(())
    }

    /// Resolves which validator a feeder is acting for.
    /// Returns the validator address if the caller is a valid feeder.
    pub fn resolve_validator_for_feeder(&self, caller: Address) -> Result<Address> {
        let vs = outbe_validatorset::contract::ValidatorSet::new(self.storage.clone());
        let all = vs.get_all_validators()?;

        // Check if caller is directly a validator (self-delegation)
        for v in &all {
            if v.validator_address == caller {
                let delegated = self.feeder_delegation.read(&caller)?;
                if delegated == Address::ZERO || delegated == caller {
                    return Ok(caller);
                }
            }
        }

        // Check if caller is a delegated feeder for any validator
        for v in &all {
            let delegated = self.feeder_delegation.read(&v.validator_address)?;
            if delegated == caller {
                return Ok(v.validator_address);
            }
        }

        Err(PrecompileError::Revert(
            "caller is not a validator or delegated feeder".into(),
        ))
    }

    // -----------------------------------------------------------------------
    // Vote Submission
    // -----------------------------------------------------------------------

    /// Submits an aggregate oracle vote on behalf of a validator.
    ///
    /// The caller must be the validator itself or a delegated feeder.
    /// Each tuple contains (pair_hash, rate, volume) for one pair.
    pub fn submit_vote(&mut self, caller: Address, tuples: &[(B256, U256, U256)]) -> Result<()> {
        let validator = self.resolve_validator_for_feeder(caller)?;

        // Validate tuple count: cannot exceed active pair count
        let pair_count = self.pair_count.read()?;
        if tuples.len() as u32 > pair_count {
            return Err(PrecompileError::Revert(
                "vote tuple count exceeds registered pair count".into(),
            ));
        }

        // Check for duplicate pair_hashes in the submission
        for i in 0..tuples.len() {
            for j in (i + 1)..tuples.len() {
                if tuples[i].0 == tuples[j].0 {
                    return Err(PrecompileError::Revert(
                        "duplicate pair in vote submission".into(),
                    ));
                }
            }
        }

        // Verify all pairs are vote targets
        for (pair_hash, _, _) in tuples {
            let is_target = self.vote_target.read(pair_hash)?;
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

        for (i, (pair_hash, rate, volume)) in tuples.iter().enumerate() {
            let idx = i as u32;
            let pair_id = self.pair_hash_to_id.read(pair_hash)?;
            pair_id_map.write(&idx, pair_id)?;
            rate_map.write(&idx, *rate)?;
            volume_map.write(&idx, *volume)?;
        }

        // Add to voter list for tally iteration
        self.voter_list.push(validator)?;

        Ok(())
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
        let idx = self.snapshot_write_idx.read()?;

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

        self.snapshot_write_idx.write(idx + 1)?;

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

        Ok(())
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

    /// Calculates VWAP for a specific pair over a time range.
    ///
    /// VWAP = sum(price_i * volume_i) / sum(volume_i)
    /// All values at 1e18 scale.
    fn binary_search_snapshot_idx(&self, target_time: u64, lo: u64, hi: u64) -> Result<u64> {
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
        let day_pv = self.daily_pv_sum.get_nested(&pair_id);
        let day_vol = self.daily_vol_sum.get_nested(&pair_id);

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
            let hash = self.pair_id_to_hash.read(&pid)?;
            if !self.vote_target.read(&hash)? {
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
            let hash = self.pair_id_to_hash.read(&pid)?;
            if !self.vote_target.read(&hash)? {
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
        let (pair_ids, vwaps, _) = self.calculate_vwaps(start_time, end_time)?;

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

        Ok(())
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
        let day_start = date_key_to_utc_timestamp(utc_day);
        let day_end = day_start.saturating_add(SECONDS_PER_DAY);

        let (pair_ids, vwaps) = match self.calculate_vwaps(day_start, day_end) {
            Ok((pair_ids, vwaps, _)) => (pair_ids, vwaps),
            // No vote-target pair had data for the day — leave it unwritten so
            // `pair_count == 0` reads as finalized-empty against the watermark.
            Err(PrecompileError::Revert(msg)) if msg.contains("no VWAP data") => return Ok(()),
            Err(e) => return Err(e),
        };

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
            let event = IOracle::VwapCalculated {
                utcDay: utc_day,
                pairId: pair_id,
                vwap,
            };
            let _ = self
                .storage
                .emit_event(ORACLE_ADDRESS, event.encode_log_data());
        }

        Ok(())
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
