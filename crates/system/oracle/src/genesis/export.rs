//! Genesis export: reads the full oracle state back into an
//! `OracleGenesisConfig`.

use alloy_primitives::{Address, U256};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;
use std::collections::BTreeSet;

use super::{
    GenesisAggregateVote, GenesisScurveEntry, GenesisSnapshot, OracleGenesisConfig, PolicyRate,
};
use crate::errors::OracleError;
use crate::schema::OracleContract;

/// `(pairs, initial_rates)` - registered pairs and their nonzero rates.
type PairExport = (Vec<(Address, Address)>, Vec<(Address, Address, U256)>);

/// Exports the full oracle state into an `OracleGenesisConfig`.
///
/// This reads all config slots, pair registry, exchange rates, delegations,
/// penalty counters, pending aggregate votes, snapshots, S-curve entries, and
/// protected validators.
///
/// `init_from_genesis` can use the exported config to re-initialize a fresh
/// oracle. This enables full state migration.
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

    let (pairs, initial_rates) = export_pairs(oracle)?;
    let feeder_delegations = export_feeder_delegations(oracle, validators)?;
    let penalty_counters = export_penalty_counters(oracle, validators)?;
    let aggregate_votes = export_aggregate_votes(oracle)?;
    let snapshots = export_snapshots(oracle)?;
    let scurve_entries = export_scurve_entries(oracle)?;
    let protected_validators = export_protected_validators(oracle, validators)?;
    let reference_currencies = oracle.reference_currencies.read_all()?;
    let policy_rates = export_policy_rates(oracle)?;

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
        reference_currencies,
        policy_rates,
        penalty_counters,
        aggregate_votes,
        snapshots,
        scurve_entries,
        protected_validators,
    })
}

/// Export pairs and non-zero initial rates.
fn export_pairs(oracle: &OracleContract) -> Result<PairExport> {
    let pair_count = oracle.pair_count.read()?;
    let mut pairs = Vec::with_capacity(pair_count as usize);
    let mut initial_rates = Vec::new();

    for pair_id in 1..=pair_count {
        let pair = export_pair_metadata(oracle, pair_id)?;
        let rate = oracle.exchange_rate.read(&pair_id)?;
        if !rate.is_zero() {
            initial_rates.push((pair.address1(), pair.address2(), rate));
        }
        pairs.push((pair.address1(), pair.address2()));
    }
    Ok((pairs, initial_rates))
}

/// Export authoritative role-scoped feeder delegations.
fn export_feeder_delegations(
    oracle: &OracleContract,
    validators: &[Address],
) -> Result<Vec<(Address, Address)>> {
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
    Ok(feeder_delegations)
}

/// Export nonzero penalty counters.
fn export_penalty_counters(
    oracle: &OracleContract,
    validators: &[Address],
) -> Result<Vec<(Address, u64, u64, u64)>> {
    let mut penalty_counters = Vec::new();
    for validator in validators {
        let success = oracle.penalty_success_count.read(validator)?;
        let abstain = oracle.penalty_abstain_count.read(validator)?;
        let miss = oracle.penalty_miss_count.read(validator)?;
        if success > 0 || abstain > 0 || miss > 0 {
            penalty_counters.push((*validator, success, abstain, miss));
        }
    }
    Ok(penalty_counters)
}

/// Export the retained snapshots, oldest first.
fn export_snapshots(oracle: &OracleContract) -> Result<Vec<GenesisSnapshot>> {
    let write_idx = oracle.snapshot_write_idx.read()?;
    let oldest_idx = oracle.snapshot_oldest_idx.read()?;
    let mut snapshots = Vec::new();
    for idx in oldest_idx..write_idx {
        let timestamp = oracle.snapshot_timestamp.read(&idx)?;
        let pc = oracle.snapshot_pair_count.read(&idx)?;
        let entries = oracle
            .snapshot_entries(idx)
            .read_all(pc)?
            .into_iter()
            .map(|(pair, rate, volume)| (pair.address1(), pair.address2(), rate, volume))
            .collect();
        snapshots.push(GenesisSnapshot { timestamp, entries });
    }
    Ok(snapshots)
}

/// Export the active S-curve entries.
fn export_scurve_entries(oracle: &OracleContract) -> Result<Vec<GenesisScurveEntry>> {
    let scurve_count = oracle.scurve_count.read()?;
    let scurve_oldest = oracle.scurve_oldest_idx.read()?;
    let mut scurve_entries = Vec::new();
    for idx in scurve_oldest..scurve_count {
        let pair = oracle.scurve_pair.read_pair(&idx)?;
        let peak_day = oracle.scurve_peak_day.read(&idx)?;
        let peak_price = oracle.scurve_peak_price.read(&idx)?;
        scurve_entries.push(GenesisScurveEntry {
            base: pair.address1(),
            quote: pair.address2(),
            peak_day,
            peak_price,
        });
    }
    Ok(scurve_entries)
}

/// Export protected validators. Reads nothing more when protection is off.
fn export_protected_validators(
    oracle: &OracleContract,
    validators: &[Address],
) -> Result<Vec<Address>> {
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
    Ok(protected_validators)
}

/// Export the policy-rate registry in its stored order.
fn export_policy_rates(oracle: &OracleContract) -> Result<Vec<PolicyRate>> {
    let policy_iso_codes = oracle.policy_rate_currencies.read_all()?;
    let mut policy_rates = Vec::with_capacity(policy_iso_codes.len());
    for iso_code in policy_iso_codes {
        let annual_rate_1e6 = oracle.policy_rate.read(&iso_code)?;
        policy_rates.push(PolicyRate {
            iso_code,
            annual_rate_1e6,
        });
    }
    Ok(policy_rates)
}

fn export_aggregate_votes(oracle: &OracleContract) -> Result<Vec<GenesisAggregateVote>> {
    let voter_count = oracle.voter_list.len()?;
    let mut seen_validators = BTreeSet::new();
    let mut aggregate_votes = Vec::with_capacity(voter_count as usize);

    for voter_idx in 0..voter_count {
        let validator = oracle.voter_list.get(voter_idx)?.ok_or(
            OracleError::MissingAggregateVoteValidator {
                voter_index: voter_idx,
            },
        )?;

        if validator == Address::ZERO {
            return Err(OracleError::AggregateVoteValidatorZero.into());
        }
        if !seen_validators.insert(validator) {
            return Err(OracleError::DuplicateAggregateVoteValidator.into());
        }
        if !oracle.vote_exists.read(&validator)? {
            return Err(OracleError::VoterListEntryWithoutVote.into());
        }

        let tuple_count = oracle.vote_tuple_count.read(&validator)?;
        let pair_map = oracle.vote_pair.get_nested(&validator);
        let rate_map = oracle.vote_rate.get_nested(&validator);
        let volume_map = oracle.vote_volume.get_nested(&validator);
        let mut seen_pairs = BTreeSet::new();
        let mut entries = Vec::with_capacity(tuple_count as usize);

        for tuple_idx in 0..tuple_count {
            let pair = pair_map.read_pair(&tuple_idx)?;
            if oracle.pair_index_of(pair)? == 0 {
                return Err(OracleError::AggregateVotePairNotRegistered.into());
            }
            // Deduplicate on the market, not the quote direction, so the same
            // pair submitted both ways round is still caught.
            if !seen_pairs.insert(pair.to_canonical()) {
                return Err(OracleError::DuplicatePairInAggregateVote.into());
            }
            entries.push((
                pair.address1(),
                pair.address2(),
                rate_map.read(&tuple_idx)?,
                volume_map.read(&tuple_idx)?,
            ));
        }

        aggregate_votes.push(GenesisAggregateVote { validator, entries });
    }

    Ok(aggregate_votes)
}

/// The registered pair at `index`, checked against the forward map.
///
/// The zero address is a legitimate asset (native COEN), so a zero check cannot
/// spot an unwritten entry. Round-tripping the pair back through `pair_index`
/// proves it is really there. It also acts as the corruption check that the old
/// pair-hash comparison provided.
fn export_pair_metadata(oracle: &OracleContract, pair_id: u32) -> Result<AddressPair> {
    let pair = oracle.pair_at(pair_id)?;
    if oracle.pair_index_of(pair)? != pair_id {
        return Err(OracleError::MissingPairMetadata { pair_id }.into());
    }
    Ok(pair)
}
