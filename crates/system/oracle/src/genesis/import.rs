//! Genesis import: writes a validated `OracleGenesisConfig` into a fresh oracle.

use alloy_primitives::{Address, U256};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;
use std::collections::BTreeSet;

use super::validate::validate_genesis_config;
use super::{
    GenesisAggregateVote, GenesisScurveEntry, GenesisSnapshot, OracleGenesisConfig, PolicyRate,
};
use crate::errors::OracleError;
use crate::schema::OracleContract;

/// Initializes all oracle state from a genesis configuration.
///
/// Writes config slots, registers pairs, sets initial exchange rates, and
/// records feeder delegations. On success, it marks the oracle as enabled and
/// initialized.
pub fn init_from_genesis(oracle: &mut OracleContract, config: &OracleGenesisConfig) -> Result<()> {
    // Idempotency guard: skip if already initialized (safe for block 0 replay).
    if oracle.config_is_initialized.read()? {
        return Ok(());
    }

    validate_genesis_config(config)?;
    write_config_slots(oracle, config)?;
    import_pairs(oracle, config)?;
    import_feeder_delegations(oracle, &config.feeder_delegations)?;
    import_currency_registries(oracle, &config.reference_currencies, &config.policy_rates)?;
    import_penalty_counters(oracle, &config.penalty_counters)?;
    import_aggregate_votes(oracle, &config.aggregate_votes)?;
    import_snapshots(oracle, &config.snapshots)?;
    import_scurve_entries(oracle, &config.scurve_entries)?;
    import_protected_validators(oracle, &config.protected_validators)?;

    oracle.config_enabled.write(true)?;
    oracle.config_is_initialized.write(true)?;

    Ok(())
}

fn write_config_slots(oracle: &mut OracleContract, config: &OracleGenesisConfig) -> Result<()> {
    oracle.config_vote_period.write(config.vote_period)?;
    oracle.config_reward_band.write(config.reward_band)?;
    oracle.config_slash_window.write(config.slash_window)?;
    oracle
        .config_min_valid_per_window
        .write(config.min_valid_per_window)?;
    oracle.config_slash_fraction.write(config.slash_fraction)?;
    oracle
        .config_lookback_duration
        .write(config.lookback_duration)
}

/// Registers the trading pairs, then sets their initial exchange rates.
fn import_pairs(oracle: &mut OracleContract, config: &OracleGenesisConfig) -> Result<()> {
    // Register trading pairs.
    for (base, quote) in &config.pairs {
        oracle.register_pair(AddressPair::from_addresses(*base, *quote))?;
    }

    // Set initial exchange rates (system caller = Address::ZERO).
    for (base, quote, rate) in &config.initial_rates {
        oracle.set_exchange_rate(
            Address::ZERO,
            AddressPair::from_addresses(*base, *quote),
            *rate,
            0,
            0,
        )?;
    }
    Ok(())
}

/// Record role-scoped feeder delegations in ValidatorSet.
fn import_feeder_delegations(
    oracle: &mut OracleContract,
    feeder_delegations: &[(Address, Address)],
) -> Result<()> {
    let mut validator_set = outbe_validatorset::contract::ValidatorSet::new(oracle.storage.clone());
    for (validator, feeder) in feeder_delegations {
        validator_set.set_delegate(
            *validator,
            outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
            *feeder,
        )?;
    }
    Ok(())
}

/// Import the independent reference-currency and policy-rate registries.
fn import_currency_registries(
    oracle: &mut OracleContract,
    reference_currencies: &[u16],
    policy_rates: &[PolicyRate],
) -> Result<()> {
    for iso_code in reference_currencies {
        oracle.reference_currencies.push(*iso_code)?;
    }
    for policy in policy_rates {
        oracle.policy_rate_currencies.push(policy.iso_code)?;
        oracle
            .policy_rate
            .write(&policy.iso_code, policy.annual_rate_1e6)?;
    }
    Ok(())
}

fn import_penalty_counters(
    oracle: &mut OracleContract,
    penalty_counters: &[(Address, u64, u64, u64)],
) -> Result<()> {
    for (validator, success, abstain, miss) in penalty_counters {
        oracle.penalty_success_count.write(validator, *success)?;
        oracle.penalty_abstain_count.write(validator, *abstain)?;
        oracle.penalty_miss_count.write(validator, *miss)?;
    }
    Ok(())
}

/// Validates every pending aggregate vote first. Then writes each one as a
/// pending vote of its validator, in config order.
fn import_aggregate_votes(
    oracle: &mut OracleContract,
    aggregate_votes: &[GenesisAggregateVote],
) -> Result<()> {
    let pair_count = oracle.pair_count.read()?;
    let mut seen_validators = BTreeSet::new();

    for vote in aggregate_votes {
        validate_aggregate_vote(oracle, vote, pair_count, &mut seen_validators)?;
    }

    for vote in aggregate_votes {
        write_aggregate_vote(oracle, vote.validator, &vote.entries)?;
    }

    Ok(())
}

fn validate_aggregate_vote(
    oracle: &OracleContract,
    vote: &GenesisAggregateVote,
    pair_count: u32,
    seen_validators: &mut BTreeSet<Address>,
) -> Result<()> {
    if vote.validator == Address::ZERO {
        return Err(OracleError::AggregateVoteValidatorZero.into());
    }
    if !seen_validators.insert(vote.validator) {
        return Err(OracleError::DuplicateAggregateVoteValidator.into());
    }
    if oracle.vote_exists.read(&vote.validator)? {
        return Err(OracleError::AggregateVoteAlreadyExists.into());
    }
    if vote.entries.len() > u32::MAX as usize || vote.entries.len() as u32 > pair_count {
        return Err(OracleError::AggregateVoteTupleCountExceedsPairCount.into());
    }
    validate_aggregate_vote_pairs(oracle, &vote.entries)
}

fn validate_aggregate_vote_pairs(
    oracle: &OracleContract,
    entries: &[(Address, Address, U256, U256)],
) -> Result<()> {
    let mut seen_pairs = BTreeSet::new();
    for (base, quote, _, _) in entries {
        // `require_pair` also rejects a pair quoted against its registered
        // direction, so an imported vote cannot smuggle in an inverted rate.
        let pair = oracle
            .require_pair_from(*base, *quote)
            .map_err(|_| OracleError::AggregateVotePairNotRegistered)?;
        if !seen_pairs.insert(pair) {
            return Err(OracleError::DuplicatePairInAggregateVote.into());
        }
        if !oracle.vote_target.read(&pair)? {
            return Err(OracleError::AggregateVotePairNotVoteTarget.into());
        }
    }
    Ok(())
}

fn write_aggregate_vote(
    oracle: &mut OracleContract,
    validator: Address,
    entries: &[(Address, Address, U256, U256)],
) -> Result<()> {
    oracle.vote_exists.write(&validator, true)?;
    oracle
        .vote_tuple_count
        .write(&validator, entries.len() as u32)?;

    let columns = oracle.vote_entries(&validator);
    for (idx, (base, quote, rate, volume)) in entries.iter().enumerate() {
        columns.write(
            idx as u32,
            AddressPair::from_addresses(*base, *quote),
            *rate,
            *volume,
        )?;
    }

    oracle.voter_list.push(validator)
}

/// Import price snapshots into the circular buffer. Entries name pairs by
/// ordinal, so every one has to resolve against a pair registered above.
fn import_snapshots(oracle: &mut OracleContract, snapshots: &[GenesisSnapshot]) -> Result<()> {
    for snapshot in snapshots {
        let mut entries = Vec::with_capacity(snapshot.entries.len());
        for (base, quote, rate, volume) in &snapshot.entries {
            let pair = oracle.require_pair_from(*base, *quote)?;
            entries.push((pair, *rate, *volume));
        }
        oracle.write_snapshot(snapshot.timestamp, &entries)?;
    }
    Ok(())
}

fn import_scurve_entries(
    oracle: &mut OracleContract,
    scurve_entries: &[GenesisScurveEntry],
) -> Result<()> {
    for entry in scurve_entries {
        let pair = oracle.require_pair_from(entry.base, entry.quote)?;
        crate::scurve::store_scurve_entry(oracle, pair, entry.peak_day, entry.peak_price)?;
    }
    Ok(())
}

/// A nonempty list also turns on protected-validator handling.
fn import_protected_validators(
    oracle: &mut OracleContract,
    protected_validators: &[Address],
) -> Result<()> {
    if !protected_validators.is_empty() {
        oracle.config_allow_protected.write(true)?;
        for validator in protected_validators {
            oracle.protected_validator.write(validator, true)?;
        }
    }
    Ok(())
}
