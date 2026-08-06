//! Genesis import/export for the Oracle contract.
//!
//! Owns the `OracleGenesisConfig` shape plus the `init_from_genesis` /
//! `export_genesis` round-trip used for chain bootstrap and state migration.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::{PrecompileError, Result};
use std::collections::BTreeSet;

use crate::constants::{DEFAULT_USD_CURRENCY_RATE, MAX_SNAPSHOT_RETENTION_SECONDS};
use crate::schema::OracleContract;

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

/// A reference currency for genesis import/export: an ISO 4217 numeric code
/// plus its annualized currency rate (1e18 scaled). The currency rate is
/// read by the Credis Factory at issuance and pinned onto the Anadosis
/// schedule. Currencies used purely as pricing references (no credis) may carry
/// a zero rate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceCurrency {
    /// ISO 4217 numeric code (e.g., 840 = USD).
    pub iso_code: u16,
    /// Annualized currency rate at 1e18 scale (e.g., 0.043 -> 43e15).
    pub currency_rate: U256,
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
    /// Settlement currencies as `(iso_code, pair_base, pair_quote)`.
    /// iso_code: ISO 4217 numeric code (e.g., 840 = USD).
    /// pair_base/pair_quote: trading pair for this settlement currency.
    pub settlement_currencies: Vec<(u16, String, String)>,
    /// Reference currencies with their annualized currency rate (1e18
    /// scaled). These ISO 4217 codes identify currencies valid for off-chain
    /// pricing references; the currency rate is read by the Credis Factory
    /// at issuance. Pre-filled at genesis with USD (840) at the current SOFR.
    pub reference_currencies: Vec<ReferenceCurrency>,
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
            pairs: vec![("COEN".into(), "840".into())],
            initial_rates: vec![],
            feeder_delegations: vec![],
            settlement_currencies: vec![],
            reference_currencies: vec![ReferenceCurrency {
                iso_code: 840,
                currency_rate: DEFAULT_USD_CURRENCY_RATE,
            }],
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

    // Record role-scoped feeder delegations in ValidatorSet.
    let mut validator_set = outbe_validatorset::contract::ValidatorSet::new(oracle.storage.clone());
    for (validator, feeder) in &config.feeder_delegations {
        validator_set.set_delegate(
            *validator,
            outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
            *feeder,
        )?;
    }

    // Import settlement currencies.
    for (iso_code, pair_base, pair_quote) in &config.settlement_currencies {
        if *iso_code == 0 {
            return Err(PrecompileError::Revert(
                "settlement iso_code must be non-zero".into(),
            ));
        }

        let pair_hash = OracleContract::pair_hash(pair_base, pair_quote);
        let pair_id = oracle.pair_hash_to_id.read(&pair_hash)?;
        if pair_id == 0 {
            return Err(PrecompileError::Revert(
                "settlement pair must be registered".into(),
            ));
        }

        if oracle.settlement_iso_to_pair.read(iso_code)? != B256::ZERO {
            return Err(PrecompileError::Revert(
                "settlement iso_code already registered".into(),
            ));
        }

        let count = oracle.settlement_count.read()?;
        oracle.settlement_iso_to_pair.write(iso_code, pair_hash)?;
        oracle.settlement_index_to_iso.write(&count, *iso_code)?;
        oracle.settlement_count.write(count + 1)?;
    }

    // Import reference currencies and their currency rates.
    let mut seen_reference_iso: BTreeSet<u16> = BTreeSet::new();
    for reference in &config.reference_currencies {
        let iso_code = reference.iso_code;
        if iso_code == 0 {
            return Err(PrecompileError::Revert(
                "reference iso_code must be non-zero".into(),
            ));
        }
        if !seen_reference_iso.insert(iso_code) {
            return Err(PrecompileError::Revert(format!(
                "duplicate reference iso_code: {iso_code}"
            )));
        }
        oracle.reference_currencies.push(iso_code)?;
        oracle
            .reference_currency_rate
            .write(&iso_code, reference.currency_rate)?;
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

    // Export authoritative role-scoped feeder delegations.
    let validator_set = outbe_validatorset::contract::ValidatorSet::new(oracle.storage.clone());
    let mut feeder_delegations = Vec::new();
    for validator in validators {
        let feeder = validator_set.get_delegate(
            *validator,
            outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
        )?;
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

        let pair_hash = oracle.settlement_iso_to_pair.read(&iso_code)?;
        let pair_id = oracle.pair_hash_to_id.read(&pair_hash)?;
        if pair_id == 0 {
            return Err(PrecompileError::Revert(format!(
                "settlement pair metadata missing for iso_code {iso_code}"
            )));
        }
        let (base, quote, _) = export_pair_metadata(oracle, pair_id)?;
        settlement_currencies.push((iso_code, base, quote));
    }

    // Export reference currencies with their currency rates (bounded list;
    // read_all OK).
    let reference_iso_codes = oracle.reference_currencies.read_all()?;
    let mut reference_currencies = Vec::with_capacity(reference_iso_codes.len());
    for iso_code in reference_iso_codes {
        let currency_rate = oracle.reference_currency_rate.read(&iso_code)?;
        reference_currencies.push(ReferenceCurrency {
            iso_code,
            currency_rate,
        });
    }

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

fn import_aggregate_votes(
    oracle: &mut OracleContract,
    aggregate_votes: &[GenesisAggregateVote],
) -> Result<()> {
    let pair_count = oracle.pair_count.read()?;
    let mut seen_validators = BTreeSet::new();
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

        let mut seen_pairs = BTreeSet::new();
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
    let mut seen_validators = BTreeSet::new();
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
        let mut seen_pairs = BTreeSet::new();
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
