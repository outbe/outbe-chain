//! Oracle tally algorithm: weighted median, standard deviation, reward band.
//!
//! Ported from Cosmos SDK `x/oracle/tally.go` and `x/oracle/types/ballot.go`.
//! Rates and volumes remain in each pair's registered scale. Dimensionless
//! reward/validity ratios and the unchanged generic cross-rate use FP18.

use alloy_primitives::{aliases::U1024, Address, U256, U512};
use alloy_sol_types::SolEvent;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::error::Result;

use crate::constants::reciprocal_scale;
use crate::errors::OracleError;
use crate::precompile::IOracle;
use crate::schema::{OracleContract, PairIndex, SCALE_1E18};

/// Maximum validator records processed by the receipt-visible Oracle slash-window
/// system transaction. The configured genesis maximum is 128; keeping the cap
/// explicit makes the mandatory phase's gas bound protocol-visible.
pub const MAX_ORACLE_SLASH_WINDOW_VALIDATORS: usize = 128;

/// A single vote entry in a ballot for one trading pair.
#[derive(Clone, Debug)]
pub struct VoteForTally {
    /// Exchange rate in the pair's registered scale.
    pub exchange_rate: U256,
    /// Volume in the pair's registered scale.
    pub volume: U256,
    /// Validator address.
    pub voter: Address,
    /// Consensus power (stake-proportional weight).
    pub power: U256,
}

/// Per-validator claim tracking across all pairs during a tally round.
#[derive(Clone, Debug, Default)]
pub struct Claim {
    /// Number of pairs where this validator's vote was within reward band.
    pub win_count: u32,
    /// Whether the validator submitted any vote.
    pub did_vote: bool,
}

/// Deterministic integer square root via Newton's method on U256.
///
/// Returns floor(sqrt(n)). Fully deterministic across platforms (no floats).
pub fn isqrt(n: U256) -> U256 {
    if n.is_zero() {
        return U256::ZERO;
    }
    if n == U256::from(1u64) {
        return U256::from(1u64);
    }
    let mut x = n;
    let mut y = (x + U256::from(1u64)) >> 1;
    while y < x {
        x = y;
        y = (x + n / x) >> 1;
    }
    x
}

fn isqrt_u1024(n: U1024) -> U1024 {
    if n.is_zero() {
        return U1024::ZERO;
    }
    if n == U1024::ONE {
        return U1024::ONE;
    }
    let mut x = n;
    let mut y = (x + U1024::ONE) >> 1;
    while y < x {
        x = y;
        y = (x + n / x) >> 1;
    }
    x
}

/// Computes the weighted median of a ballot.
///
/// The ballot must be sorted by exchange_rate in ascending order.
/// Returns the exchange rate of the vote where cumulative power crosses
/// the 50% threshold. Returns zero if the ballot is empty.
pub fn weighted_median(ballot: &[VoteForTally]) -> U256 {
    if ballot.is_empty() {
        return U256::ZERO;
    }

    let total_power: U512 = ballot.iter().map(|v| U512::from(v.power)).sum();
    if total_power.is_zero() {
        return U256::ZERO;
    }

    let mut cumulative = U512::ZERO;

    for (index, vote) in ballot.iter().enumerate() {
        cumulative += U512::from(vote.power);
        let doubled = cumulative * U512::from(2u64);
        if doubled > total_power {
            return vote.exchange_rate;
        }
        if doubled == total_power {
            let Some(upper) = ballot.get(index + 1) else {
                return vote.exchange_rate;
            };
            return vote.exchange_rate
                + (upper.exchange_rate - vote.exchange_rate) / U256::from(2u64);
        }
    }

    // Should not reach here if ballot is non-empty with non-zero power
    ballot.last().map_or(U256::ZERO, |v| v.exchange_rate)
}

/// Computes the population standard deviation of a ballot around a given median.
///
/// Formula: sqrt(sum((rate_i - median)^2) / count)
/// Uses integer sqrt (Newton's method) for determinism.
/// The result remains in the ballot's rate scale; squared deviations use the
/// square of that scale.
pub fn standard_deviation(ballot: &[VoteForTally], median: U256) -> Result<U256> {
    if ballot.is_empty() {
        return Ok(U256::ZERO);
    }

    let count = U1024::from(ballot.len());
    let mut sum_sq = U1024::ZERO;

    for vote in ballot {
        // deviation = |rate - median| (unsigned arithmetic)
        let deviation = if vote.exchange_rate > median {
            vote.exchange_rate - median
        } else {
            median - vote.exchange_rate
        };
        let wide = U1024::from(deviation);
        let sq = wide * wide;
        sum_sq = sum_sq
            .checked_add(sq)
            .ok_or(OracleError::TallyArithmeticOverflow(
                "standard deviation sum",
            ))?;
    }

    // variance = sum_sq / count (at the square of the rate scale)
    let variance = sum_sq / count;

    // sqrt(variance) -> result in the original rate scale
    let result = isqrt_u1024(variance);
    if result > U1024::from(U256::MAX) {
        return Err(OracleError::TallyArithmeticOverflow("standard deviation result").into());
    }
    Ok(result.wrapping_to::<U256>())
}

/// Runs the tally algorithm for a single pair's ballot.
///
/// Computes weighted median, standard deviation, reward spread, and marks
/// winners in the claim map.
///
/// Only positive-rate, positive-power rows participate in price, deviation and
/// winner calculations. Every submitted row still marks participation, so an
/// ineligible row is a miss rather than an abstention at the round level.
///
/// Returns the weighted median exchange rate.
pub fn tally_pair(
    ballot: &mut [VoteForTally],
    reward_band: U256,
    claims: &mut [(Address, Claim)],
) -> Result<U256> {
    if ballot.is_empty() {
        return Ok(U256::ZERO);
    }

    for vote in ballot.iter() {
        mark_participation(claims, vote.voter);
    }

    let mut eligible: Vec<VoteForTally> = ballot
        .iter()
        .filter(|vote| vote_is_eligible(vote))
        .cloned()
        .collect();
    if eligible.is_empty() {
        return Ok(U256::ZERO);
    }

    eligible.sort_by_key(|vote| vote.exchange_rate);

    let median = weighted_median(&eligible);
    let std_dev = standard_deviation(&eligible, median)?;

    // reward_spread = max(std_dev, median * reward_band / (2 * FP18)).
    // The reward band is dimensionless FP18; the result stays in median scale.
    let wide_base_spread =
        (U512::from(median) * U512::from(reward_band)) / U512::from(U256::from(2u64) * SCALE_1E18);
    if wide_base_spread > U512::from(U256::MAX) {
        return Err(OracleError::TallyArithmeticOverflow("reward spread").into());
    }
    let base_spread = wide_base_spread.wrapping_to::<U256>();
    let reward_spread = if std_dev > base_spread {
        std_dev
    } else {
        base_spread
    };

    // Determine lower and upper bounds for winning votes
    let lower = median.saturating_sub(reward_spread);
    let upper = median.saturating_add(reward_spread);

    // Mark winners
    for vote in &eligible {
        if vote.exchange_rate >= lower && vote.exchange_rate <= upper {
            // Find this voter in claims and increment win_count
            for (addr, claim) in claims.iter_mut() {
                if *addr == vote.voter {
                    claim.win_count += 1;
                    break;
                }
            }
        }
    }

    Ok(median)
}

/// Converts a ballot to cross-rates using a reference pair's votes.
///
/// For each voter, the cross-rate is: `reference_rate / vote_rate`.
/// Rows without an eligible reference leg are excluded. Arithmetic overflow is
/// a typed tally error; it never synthesizes a zero-price row.
pub fn to_cross_rate(
    ballot: &[VoteForTally],
    reference_votes: &[(Address, U256)],
    reference_pair: AddressPair,
    target_pair: AddressPair,
) -> Result<Vec<VoteForTally>> {
    let reference_scale = reciprocal_scale(reference_pair);
    let target_scale = reciprocal_scale(target_pair);
    let mut cross_ballot = Vec::with_capacity(ballot.len());
    for vote in ballot.iter().filter(|vote| vote_is_eligible(vote)) {
        let Some(reference_rate) = reference_votes
            .iter()
            .find(|(address, rate)| *address == vote.voter && !rate.is_zero())
            .map(|(_, rate)| *rate)
        else {
            continue;
        };
        let numerator = U512::from(reference_rate)
            .checked_mul(U512::from(target_scale))
            .and_then(|value| value.checked_mul(U512::from(SCALE_1E18)))
            .ok_or(OracleError::CrossRateOverflow)?;
        let denominator = U512::from(reference_scale)
            .checked_mul(U512::from(vote.exchange_rate))
            .ok_or(OracleError::CrossRateOverflow)?;
        let cross = narrow_cross_rate(
            numerator
                .checked_div(denominator)
                .ok_or(OracleError::CrossRateOverflow)?,
        )?;
        cross_ballot.push(VoteForTally {
            exchange_rate: cross,
            volume: vote.volume,
            voter: vote.voter,
            power: vote.power,
        });
    }
    Ok(cross_ballot)
}

fn from_cross_rate(
    reference_rate: U256,
    cross_rate: U256,
    reference_pair: AddressPair,
    target_pair: AddressPair,
) -> Result<U256> {
    let numerator = U512::from(reference_rate)
        .checked_mul(U512::from(reciprocal_scale(target_pair)))
        .and_then(|value| value.checked_mul(U512::from(SCALE_1E18)))
        .ok_or(OracleError::CrossRateOverflow)?;
    let denominator = U512::from(reciprocal_scale(reference_pair))
        .checked_mul(U512::from(cross_rate))
        .ok_or(OracleError::CrossRateOverflow)?;
    narrow_cross_rate(
        numerator
            .checked_div(denominator)
            .ok_or(OracleError::CrossRateOverflow)?,
    )
}

fn narrow_cross_rate(value: U512) -> Result<U256> {
    if value > U512::from(U256::MAX) {
        return Err(OracleError::CrossRateOverflow.into());
    }
    Ok(value.wrapping_to::<U256>())
}

fn vote_is_eligible(vote: &VoteForTally) -> bool {
    !vote.power.is_zero() && !vote.exchange_rate.is_zero()
}

fn eligible_power(ballot: &[VoteForTally]) -> U512 {
    ballot
        .iter()
        .filter(|vote| vote_is_eligible(vote))
        .map(|vote| U512::from(vote.power))
        .sum()
}

/// Number of independent validator observations required for one raw pair.
/// Every active validator contributes at most one vote, regardless of stake.
fn pair_quorum(active_validator_count: usize) -> usize {
    active_validator_count - active_validator_count / 3
}

fn mark_participation(claims: &mut [(Address, Claim)], voter: Address) {
    if let Some((_, claim)) = claims.iter_mut().find(|(address, _)| *address == voter) {
        claim.did_vote = true;
    }
}

/// Orchestrates the full tally for all pairs in a vote period.
///
/// 1. Reads all votes from storage
/// 2. Organizes into per-pair ballots
/// 3. Picks reference pair (highest voting power)
/// 4. Tallies reference pair directly, others via cross-rate
/// 5. Updates exchange rates and snapshots
/// 6. Counts miss/success/abstain per validator
/// 7. Clears votes
pub fn run_tally(oracle: &mut OracleContract, block_number: u64, timestamp: u64) -> Result<()> {
    let storage = oracle.storage.clone();
    storage.with_checkpoint(|| run_tally_inner(oracle, block_number, timestamp))
}

fn run_tally_inner(oracle: &mut OracleContract, block_number: u64, timestamp: u64) -> Result<()> {
    let enabled = oracle.config_enabled.read()?;
    if !enabled {
        return Ok(());
    }

    let reward_band = oracle.config_reward_band.read()?;

    // Collect active validators and their power (stake) at TALLY TIME.
    // Intentional divergence from Cosmos (which locks the set at period start):
    // With a 2-block vote period (~24s) and permissioned validators, stake
    // changes between vote and tally are negligible. Snapshotting at vote time
    // would require additional storage per period per validator.
    let vs = outbe_validatorset::contract::ValidatorSet::new(oracle.storage.clone());
    let all_validators = vs.get_active_validators()?;
    if all_validators.is_empty() {
        oracle.clear_votes()?;
        return Ok(());
    }

    // Build claims map: (address, claim)
    let mut claims: Vec<(Address, Claim)> = all_validators
        .iter()
        .map(|v| (v.validator_address, Claim::default()))
        .collect();

    // Read all votes and organize into per-pair ballots
    let voter_count = oracle.voter_list.len()?;
    if voter_count == 0 {
        // No votes this period - all validators get abstain
        for v in &all_validators {
            oracle.increment_abstain(&v.validator_address)?;
        }
        oracle.clear_votes()?;
        return Ok(());
    }

    // Collect vote targets (active pairs) with the registry index the rate
    // columns are keyed by, so the write-back never re-derives it from the pair.
    let pair_count = oracle.pair_count.read()?;
    let mut active_pairs: Vec<(PairIndex, AddressPair)> = Vec::new();
    for pid in 1..=pair_count {
        let pair = oracle.pair_at(pid)?;
        if oracle.vote_target.read(&pair)? {
            active_pairs.push((pid, pair));
        }
    }
    let total_targets = active_pairs.len() as u32;

    if total_targets == 0 {
        oracle.clear_votes()?;
        return Ok(());
    }

    // Organize votes into per-pair ballots
    let mut ballot_map: Vec<(PairIndex, AddressPair, Vec<VoteForTally>)> = active_pairs
        .iter()
        .map(|(index, pair)| (*index, *pair, Vec::new()))
        .collect();

    for vi in 0..voter_count {
        let voter = oracle.voter_list.get(vi)?.unwrap_or(Address::ZERO);
        let tuple_count = oracle.vote_tuple_count.read(&voter)?;

        let pair_map = oracle.vote_pair.get_nested(&voter);
        let rate_map = oracle.vote_rate.get_nested(&voter);
        let volume_map = oracle.vote_volume.get_nested(&voter);

        // Look up validator power from the active set
        let power = all_validators
            .iter()
            .find(|v| v.validator_address == voter)
            .map(|v| v.stake)
            .unwrap_or(U256::ZERO);

        for ti in 0..tuple_count {
            let voted_pair = pair_map.read_pair(&ti)?;
            let rate = rate_map.read(&ti)?;
            let volume = volume_map.read(&ti)?;

            // Find the ballot for this pair
            if let Some((_, _, ballot)) = ballot_map
                .iter_mut()
                .find(|(_, pair, _)| pair.same_market(&voted_pair))
            {
                mark_participation(&mut claims, voter);
                ballot.push(VoteForTally {
                    exchange_rate: rate,
                    volume,
                    voter,
                    power,
                });
            }
        }
    }

    // Qualify each raw pair independently by validator count. Stake still
    // weights medians and reference selection, but it cannot replace a missing
    // validator observation for quorum.
    let quorum = pair_quorum(all_validators.len());
    let mut qualified = vec![false; ballot_map.len()];
    for (index, (_, _, ballot)) in ballot_map.iter().enumerate() {
        let eligible_count = ballot.iter().filter(|vote| vote_is_eligible(vote)).count();
        if eligible_count >= quorum {
            qualified[index] = true;
        } else {
            // Cosmos-style participation credit: a valid observation on a pair
            // that lacks quorum is not punished as an outlier. Missing and
            // zero-rate observations receive no credit for that pair.
            for vote in ballot.iter().filter(|vote| vote_is_eligible(vote)) {
                if let Some((_, claim)) = claims
                    .iter_mut()
                    .find(|(address, _)| *address == vote.voter)
                {
                    claim.win_count += 1;
                }
            }
        }
    }

    // Pick the qualified reference pair with the greatest eligible stake.
    // Iteration follows registry order, and equal stake deliberately keeps the
    // first registered pair.
    let mut ref_pair_idx = qualified.iter().position(|is_qualified| *is_qualified);
    if let Some(mut current) = ref_pair_idx {
        for index in (current + 1)..ballot_map.len() {
            if qualified[index]
                && eligible_power(&ballot_map[index].2) > eligible_power(&ballot_map[current].2)
            {
                current = index;
            }
        }
        ref_pair_idx = Some(current);
    }

    // Snapshot entries to collect.
    let mut snapshot_entries: Vec<(AddressPair, U256, U256)> = Vec::new();
    if let Some(ref_pair_idx) = ref_pair_idx {
        // Tally reference pair directly.
        let (ref_index, ref_pair) = (ballot_map[ref_pair_idx].0, ballot_map[ref_pair_idx].1);
        let ref_median = {
            let ballot = &mut ballot_map[ref_pair_idx].2;
            tally_pair(ballot, reward_band, &mut claims)?
        };

        let reference_votes: Vec<(Address, U256)> = ballot_map[ref_pair_idx]
            .2
            .iter()
            .filter(|vote| vote_is_eligible(vote))
            .map(|v| (v.voter, v.exchange_rate))
            .collect();

        if !ref_median.is_zero() {
            oracle.update_exchange_rate(ref_index, ref_median, block_number, timestamp)?;
            let event = IOracle::ExchangeRateUpdated {
                base: ref_pair.address1(),
                quote: ref_pair.address2(),
                rate: ref_median,
                blockNumber: block_number,
            };
            let _ = oracle
                .storage
                .emit_event(ORACLE_ADDRESS, event.encode_log_data());

            let total_volume: U256 = ballot_map[ref_pair_idx]
                .2
                .iter()
                .filter(|vote| vote_is_eligible(vote))
                .map(|v| v.volume)
                .fold(U256::ZERO, |acc, v| acc.saturating_add(v));
            snapshot_entries.push((ref_pair, ref_median, total_volume));
        }

        // Tally every other quorum-qualified pair via the reference overlap.
        // There is intentionally no second quorum over that intersection.
        for (i, entry) in ballot_map.iter().enumerate() {
            if i == ref_pair_idx || !qualified[i] {
                continue;
            }

            let (index, pair) = (entry.0, entry.1);
            let ballot = &entry.2;
            let mut cross_ballot = to_cross_rate(ballot, &reference_votes, ref_pair, pair)?;
            let cross_median = tally_pair(&mut cross_ballot, reward_band, &mut claims)?;

            if !cross_median.is_zero() && !ref_median.is_zero() {
                let actual_rate = from_cross_rate(ref_median, cross_median, ref_pair, pair)?;

                if !actual_rate.is_zero() {
                    oracle.update_exchange_rate(index, actual_rate, block_number, timestamp)?;
                    let event = IOracle::ExchangeRateUpdated {
                        base: pair.address1(),
                        quote: pair.address2(),
                        rate: actual_rate,
                        blockNumber: block_number,
                    };
                    let _ = oracle
                        .storage
                        .emit_event(ORACLE_ADDRESS, event.encode_log_data());

                    // Volume belongs to the target pair's full eligible raw
                    // ballot, not merely validators in the cross intersection.
                    let total_volume: U256 = ballot
                        .iter()
                        .filter(|vote| vote_is_eligible(vote))
                        .map(|v| v.volume)
                        .fold(U256::ZERO, |acc, v| acc.saturating_add(v));
                    snapshot_entries.push((pair, actual_rate, total_volume));
                }
            }
        }
    }

    // Write price snapshot
    if !snapshot_entries.is_empty() {
        oracle.write_snapshot(timestamp, &snapshot_entries)?;
    }

    // Count miss/success/abstain per validator
    for (addr, claim) in &claims {
        if claim.win_count == total_targets {
            oracle.increment_success(addr)?;
        } else if !claim.did_vote {
            oracle.increment_abstain(addr)?;
        } else {
            oracle.increment_miss(addr)?;
        }
    }

    // Emit TallyCompleted event
    let pairs_updated = snapshot_entries.len() as u32;
    let event = IOracle::TallyCompleted {
        blockNumber: block_number,
        pairsUpdated: pairs_updated,
    };
    let _ = oracle
        .storage
        .emit_event(ORACLE_ADDRESS, event.encode_log_data());

    // Clear all votes
    oracle.clear_votes()?;

    Ok(())
}

/// Processes the slash window: checks vote rates and force-exits underperformers.
pub fn slash_and_reset_counters(oracle: &mut OracleContract, _timestamp: u64) -> Result<()> {
    let min_valid = oracle.config_min_valid_per_window.read()?;
    let allow_protected = oracle.config_allow_protected.read()?;

    let vs = outbe_validatorset::contract::ValidatorSet::new(oracle.storage.clone());
    let validator_addresses = vs.registered_validator_addresses()?;
    if validator_addresses.len() > MAX_ORACLE_SLASH_WINDOW_VALIDATORS {
        return Err(OracleError::SlashWindowValidatorSetExceedsCap {
            actual: validator_addresses.len(),
            cap: MAX_ORACLE_SLASH_WINDOW_VALIDATORS,
        }
        .into());
    }

    for addr in validator_addresses {
        // Skip protected validators
        if allow_protected {
            let is_protected = oracle.protected_validator.read(&addr)?;
            if is_protected {
                oracle.reset_penalty_counter(&addr)?;
                continue;
            }
        }

        let success = oracle.penalty_success_count.read(&addr)?;
        let abstain = oracle.penalty_abstain_count.read(&addr)?;
        let miss = oracle.penalty_miss_count.read(&addr)?;
        let total = success + abstain + miss;

        if total == 0 {
            oracle.reset_penalty_counter(&addr)?;
            continue;
        }

        // valid_rate = success * 1e18 / total
        let valid_rate = U256::from(success) * SCALE_1E18 / U256::from(total);

        if valid_rate < min_valid {
            let storage = oracle.storage.clone();
            storage.with_checkpoint(|| {
                // Force-exit first so validator lifecycle events and status
                // transitions follow the same ordering as slash indicator.
                // Keep the cross-module writes under one checkpoint: any later
                // slash/reset failure must roll back forced-exit state.
                let mut vs_mut =
                    outbe_validatorset::contract::ValidatorSet::new(oracle.storage.clone());
                // Oracle underperformance felony: JAIL (not force-exit) + slash.
                vs_mut.jail_validator(addr)?;
                let event = IOracle::ValidatorForcedExit { validator: addr };
                let _ = oracle
                    .storage
                    .emit_event(ORACLE_ADDRESS, event.encode_log_data());

                let slash_fraction = oracle.config_slash_fraction.read()?;
                if !slash_fraction.is_zero() {
                    // Convert 1e18-scaled fraction to percent: fraction * 100 / 1e18
                    let slash_pct = (slash_fraction * U256::from(100u64) / SCALE_1E18).to::<u64>();
                    if slash_pct > 0 {
                        let mut staking =
                            outbe_staking::contract::Staking::new(oracle.storage.clone());
                        staking.slash_stake(addr, slash_pct)?;
                        let event = IOracle::ValidatorSlashed {
                            validator: addr,
                            slashPercent: slash_pct,
                        };
                        let _ = oracle
                            .storage
                            .emit_event(ORACLE_ADDRESS, event.encode_log_data());
                    }
                }

                oracle.reset_penalty_counter(&addr)?;
                Ok(())
            })?;
            continue;
        }

        oracle.reset_penalty_counter(&addr)?;
    }

    // Remove exchange rates for deactivated pairs (Cosmos: RemoveExcessFeeds)
    oracle.remove_excess_feeds()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed18(whole: u64) -> U256 {
        U256::from(whole) * SCALE_1E18
    }

    #[test]
    fn pair_quorum_is_ceiling_two_thirds_of_active_validators() {
        let expected = [0usize, 1, 2, 2, 3, 4, 4, 5, 6, 6, 7];
        for (active, expected_quorum) in expected.into_iter().enumerate() {
            assert_eq!(pair_quorum(active), expected_quorum, "N={active}");
        }
    }

    #[test]
    fn test_isqrt() {
        assert_eq!(isqrt(U256::ZERO), U256::ZERO);
        assert_eq!(isqrt(U256::from(1u64)), U256::from(1u64));
        assert_eq!(isqrt(U256::from(4u64)), U256::from(2u64));
        assert_eq!(isqrt(U256::from(9u64)), U256::from(3u64));
        assert_eq!(isqrt(U256::from(16u64)), U256::from(4u64));
        assert_eq!(isqrt(U256::from(100u64)), U256::from(10u64));
        // floor(sqrt(2)) = 1
        assert_eq!(isqrt(U256::from(2u64)), U256::from(1u64));
        // floor(sqrt(15)) = 3
        assert_eq!(isqrt(U256::from(15u64)), U256::from(3u64));
        // Large value: sqrt(1e36) = 1e18
        let val = SCALE_1E18 * SCALE_1E18;
        assert_eq!(isqrt(val), SCALE_1E18);
    }

    #[test]
    fn test_weighted_median_single() {
        let ballot = vec![VoteForTally {
            exchange_rate: fixed18(100u64),
            volume: SCALE_1E18,
            voter: Address::new([1u8; 20]),
            power: U256::from(10u64),
        }];
        assert_eq!(weighted_median(&ballot), fixed18(100u64));
    }

    #[test]
    fn test_weighted_median_exact_half_averages_adjacent_rates() {
        // Three voters: powers 10, 20, 30. Total=60, half=30.
        // Sorted by rate: 100, 200, 300
        // Cumsum lands exactly at 30 after 200, so the two central rates are
        // averaged: (200 + 300) / 2 = 250.
        let ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(100u64),
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
                power: U256::from(10u64),
            },
            VoteForTally {
                exchange_rate: fixed18(200u64),
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
                power: U256::from(20u64),
            },
            VoteForTally {
                exchange_rate: fixed18(300u64),
                volume: SCALE_1E18,
                voter: Address::new([3u8; 20]),
                power: U256::from(30u64),
            },
        ];
        assert_eq!(weighted_median(&ballot), fixed18(250u64));
    }

    #[test]
    fn test_weighted_median_equal_power() {
        // Equal power: U256::from(10u64), 10, 10. Total=30, half=15.
        // Cumsum: 10 (<15), 20 (>=15) -> median = 200
        let ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(100u64),
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
                power: U256::from(10u64),
            },
            VoteForTally {
                exchange_rate: fixed18(200u64),
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
                power: U256::from(10u64),
            },
            VoteForTally {
                exchange_rate: fixed18(300u64),
                volume: SCALE_1E18,
                voter: Address::new([3u8; 20]),
                power: U256::from(10u64),
            },
        ];
        assert_eq!(weighted_median(&ballot), fixed18(200u64));
    }

    #[test]
    fn weighted_median_uses_the_ceiling_half_for_odd_power() {
        let ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(100),
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
                power: U256::from(1u64),
            },
            VoteForTally {
                exchange_rate: fixed18(200),
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
                power: U256::from(1u64),
            },
            VoteForTally {
                exchange_rate: fixed18(300),
                volume: SCALE_1E18,
                voter: Address::new([3u8; 20]),
                power: U256::from(1u64),
            },
        ];

        assert_eq!(weighted_median(&ballot), fixed18(200));
    }

    #[test]
    fn weighted_median_averages_the_two_central_exact_half_results() {
        let ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(100),
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
                power: U256::from(1u64),
            },
            VoteForTally {
                exchange_rate: fixed18(200),
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
                power: U256::from(1u64),
            },
        ];

        assert_eq!(weighted_median(&ballot), fixed18(150));
    }

    #[test]
    fn weighted_median_midpoint_floors_in_the_rate_minor_unit() {
        let ballot = vec![
            VoteForTally {
                exchange_rate: U256::from(100u64),
                volume: U256::ONE,
                voter: Address::new([1u8; 20]),
                power: U256::ONE,
            },
            VoteForTally {
                exchange_rate: U256::from(201u64),
                volume: U256::ONE,
                voter: Address::new([2u8; 20]),
                power: U256::ONE,
            },
        ];

        assert_eq!(weighted_median(&ballot), U256::from(150u64));
    }

    #[test]
    fn weighted_median_preserves_raw_stake_above_u64() {
        let large = U256::from(u64::MAX) + U256::ONE;
        let ballot = vec![
            VoteForTally {
                exchange_rate: U256::from(100u64),
                volume: U256::ONE,
                voter: Address::new([1u8; 20]),
                power: large,
            },
            VoteForTally {
                exchange_rate: U256::from(200u64),
                volume: U256::ONE,
                voter: Address::new([2u8; 20]),
                power: large + U256::ONE,
            },
        ];

        assert_eq!(weighted_median(&ballot), U256::from(200u64));
    }

    #[test]
    fn test_weighted_median_empty() {
        let ballot: Vec<VoteForTally> = vec![];
        assert_eq!(weighted_median(&ballot), U256::ZERO);
    }

    #[test]
    fn test_standard_deviation_identical() {
        // All same rate -> std dev = 0
        let ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(100u64),
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
                power: U256::from(10u64),
            },
            VoteForTally {
                exchange_rate: fixed18(100u64),
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
                power: U256::from(10u64),
            },
        ];
        let median = fixed18(100u64);
        assert_eq!(standard_deviation(&ballot, median).unwrap(), U256::ZERO);
    }

    #[test]
    fn test_standard_deviation_known() {
        // Rates: 100, 200. Median = 150.
        // Deviations: 50, 50. Squared: 2500, 2500.
        // Variance = 5000/2 = 2500. StdDev = 50.
        // At 1e18 scale: deviations are 50e18 each.
        // Squared = 2500e36. Variance = 2500e36/2 = 1250e36. sqrt = ~35.35e18
        // Wait, let me recalculate properly with median being the weighted median.
        // With equal powers: weighted median is 200 (cumsum: 10 >= 10=total/2 at first vote of 100? no)
        // total=20, half=10. cumsum after first: 10 >= 10 -> median = 100.
        // Deviations: |100-100|=0, |200-100|=100e18.
        // Squared: 0, (100e18)^2 = 1e40.
        // Variance = 1e40/2 = 5e39. sqrt(5e39) = sqrt(5)*1e19.5... hmm this gets complicated.
        // Let me use simpler values.

        // Rates: 8e18, 12e18. Median = 10e18 (assume given).
        // Deviations: 2e18, 2e18. Squared: 4e36, 4e36.
        // Variance = 8e36/2 = 4e36. StdDev = sqrt(4e36) = 2e18.
        let rate_a = fixed18(8u64);
        let rate_b = fixed18(12u64);
        let median = fixed18(10u64);
        let ballot = vec![
            VoteForTally {
                exchange_rate: rate_a,
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
                power: U256::from(10u64),
            },
            VoteForTally {
                exchange_rate: rate_b,
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
                power: U256::from(10u64),
            },
        ];
        let std_dev = standard_deviation(&ballot, median).unwrap();
        assert_eq!(std_dev, fixed18(2u64));
    }

    #[test]
    fn standard_deviation_widens_large_squares_instead_of_returning_zero() {
        let ballot = vec![
            VoteForTally {
                exchange_rate: U256::MAX,
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
                power: U256::from(10u64),
            },
            VoteForTally {
                exchange_rate: U256::MAX,
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
                power: U256::from(10u64),
            },
        ];
        let std_dev = standard_deviation(&ballot, U256::ZERO).unwrap();
        assert_eq!(std_dev, U256::MAX);
    }

    #[test]
    fn standard_deviation_depends_on_observations_not_stake() {
        let rates = [fixed18(90), fixed18(100), fixed18(130)];
        let ballot = rates
            .into_iter()
            .enumerate()
            .map(|(index, exchange_rate)| VoteForTally {
                exchange_rate,
                volume: U256::ONE,
                voter: Address::new([index as u8 + 1; 20]),
                power: U256::ONE,
            })
            .collect::<Vec<_>>();
        let mut reweighted = ballot.clone();
        reweighted[0].power = U256::from(1_000_000u64);
        reweighted[1].power = U256::from(7u64);
        reweighted[2].power = U256::from(99u64);

        assert_eq!(
            standard_deviation(&ballot, fixed18(100)).unwrap(),
            standard_deviation(&reweighted, fixed18(100)).unwrap()
        );
    }

    #[test]
    fn reward_band_includes_both_exact_boundaries() {
        let voters = [
            Address::new([1u8; 20]),
            Address::new([2u8; 20]),
            Address::new([3u8; 20]),
        ];
        let mut ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(99),
                volume: U256::ONE,
                voter: voters[0],
                power: U256::ONE,
            },
            VoteForTally {
                exchange_rate: fixed18(100),
                volume: U256::ONE,
                voter: voters[1],
                power: U256::ONE,
            },
            VoteForTally {
                exchange_rate: fixed18(101),
                volume: U256::ONE,
                voter: voters[2],
                power: U256::ONE,
            },
        ];
        let mut claims = voters
            .into_iter()
            .map(|voter| (voter, Claim::default()))
            .collect::<Vec<_>>();

        let median = tally_pair(
            &mut ballot,
            U256::from(20_000_000_000_000_000u64),
            &mut claims,
        )
        .unwrap();

        assert_eq!(median, fixed18(100));
        assert!(claims.iter().all(|(_, claim)| claim.win_count == 1));
    }

    #[test]
    fn test_tally_pair_winners() {
        // 3 validators voting on one pair.
        // Rates: 100, 101, 200 (1e18 scaled).
        // Powers: 10, 20, 10. Total=40, half=20.
        // Sorted: 100(10), 101(20), 200(10).
        // Cumsum: 10(<20), 30(>=20) -> median = 101.
        // StdDev: deviations from 101 = |100-101|=1, |101-101|=0, |200-101|=99
        // Squared: 1, 0, 9801. Sum=9802. Variance=9802/3=3267.33. StdDev=sqrt(3267.33)~=57.16
        // Reward band = 0.02 * 1e18. base_spread = 101 * 0.02 / 2 = 1.01.
        // Since stddev(57.16) > base_spread(1.01), reward_spread = 57.16.
        // Range: [101-57.16, 101+57.16] = [43.84, 158.16]
        // Vote 100 is in range -> win. Vote 101 is in range -> win. Vote 200 is NOT in range -> miss.

        let addr1 = Address::new([1u8; 20]);
        let addr2 = Address::new([2u8; 20]);
        let addr3 = Address::new([3u8; 20]);

        let mut ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(100u64),
                volume: SCALE_1E18,
                voter: addr1,
                power: U256::from(10u64),
            },
            VoteForTally {
                exchange_rate: fixed18(101u64),
                volume: SCALE_1E18,
                voter: addr2,
                power: U256::from(20u64),
            },
            VoteForTally {
                exchange_rate: fixed18(200u64),
                volume: SCALE_1E18,
                voter: addr3,
                power: U256::from(10u64),
            },
        ];

        let reward_band = U256::from(20_000_000_000_000_000u128); // 0.02 * 1e18
        let mut claims = vec![
            (addr1, Claim::default()),
            (addr2, Claim::default()),
            (addr3, Claim::default()),
        ];

        let median = tally_pair(&mut ballot, reward_band, &mut claims).unwrap();
        assert_eq!(median, fixed18(101u64));

        // Voters 1 and 2 should have won, voter 3 should have missed
        assert_eq!(claims[0].1.win_count, 1); // addr1: rate 100, in range
        assert_eq!(claims[1].1.win_count, 1); // addr2: rate 101, in range
        assert_eq!(claims[2].1.win_count, 0); // addr3: rate 200, out of range
        assert!(claims[0].1.did_vote);
        assert!(claims[1].1.did_vote);
        assert!(claims[2].1.did_vote);
    }

    #[test]
    fn tally_pair_excludes_zero_rate_and_zero_power_from_price_and_rewards() {
        let valid = Address::new([1u8; 20]);
        let zero_rate = Address::new([2u8; 20]);
        let zero_power = Address::new([3u8; 20]);
        let mut ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(100),
                volume: SCALE_1E18,
                voter: valid,
                power: U256::from(1u64),
            },
            VoteForTally {
                exchange_rate: U256::ZERO,
                volume: SCALE_1E18,
                voter: zero_rate,
                power: U256::from(1u64),
            },
            VoteForTally {
                exchange_rate: fixed18(1),
                volume: SCALE_1E18,
                voter: zero_power,
                power: U256::from(0u64),
            },
        ];
        let mut claims = vec![
            (valid, Claim::default()),
            (zero_rate, Claim::default()),
            (zero_power, Claim::default()),
        ];

        let median = tally_pair(&mut ballot, U256::ZERO, &mut claims).unwrap();

        assert_eq!(median, fixed18(100));
        assert_eq!(claims[0].1.win_count, 1);
        assert_eq!(claims[1].1.win_count, 0);
        assert_eq!(claims[2].1.win_count, 0);
        assert!(claims.iter().all(|(_, claim)| claim.did_vote));
    }

    #[test]
    fn test_cross_rate() {
        let addr1 = Address::new([1u8; 20]);
        let addr2 = Address::new([2u8; 20]);
        let reference_pair =
            AddressPair::from_addresses(Address::new([3u8; 20]), Address::new([4u8; 20]));
        let target_pair =
            AddressPair::from_addresses(Address::new([5u8; 20]), Address::new([6u8; 20]));

        // Reference pair votes (e.g., ETH/USD): voter1=2000, voter2=2010
        let reference_votes = vec![(addr1, fixed18(2000u64)), (addr2, fixed18(2010u64))];

        // Current pair votes (e.g., BTC/USD): voter1=40000, voter2=40200
        let ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(40000u64),
                volume: SCALE_1E18,
                voter: addr1,
                power: U256::from(10u64),
            },
            VoteForTally {
                exchange_rate: fixed18(40200u64),
                volume: SCALE_1E18,
                voter: addr2,
                power: U256::from(10u64),
            },
        ];

        let cross = to_cross_rate(&ballot, &reference_votes, reference_pair, target_pair).unwrap();

        // Cross rate for voter1: 2000 * 1e18 / 40000 = 0.05 * 1e18
        assert_eq!(
            cross[0].exchange_rate,
            U256::from(50_000_000_000_000_000u128)
        ); // 0.05e18

        // Cross rate for voter2: 2010 * 1e18 / 40200 = 0.05 * 1e18 (approximately)
        // 2010e18 * 1e18 / 40200e18 = 2010/40200 * 1e18 = 0.05 * 1e18
        assert_eq!(
            cross[1].exchange_rate,
            U256::from(50_000_000_000_000_000u128)
        ); // 0.05e18 (exact due to integer division)
    }

    #[test]
    fn cross_rate_keeps_a_dimensionless_fp18_ratio_for_p6_coen_iso_inputs() {
        let voter = Address::new([1u8; 20]);
        let reference_votes = vec![(voter, U256::from(2_000_000u64))];
        let ballot = vec![VoteForTally {
            exchange_rate: U256::from(4_000_000u64),
            volume: U256::from(1_000_000u64),
            voter,
            power: U256::from(10u64),
        }];

        let cross = to_cross_rate(
            &ballot,
            &reference_votes,
            AddressPair::new_coen_to(840),
            AddressPair::new_coen_to(978),
        )
        .unwrap();

        assert_eq!(
            cross[0].exchange_rate,
            U256::from(500_000_000_000_000_000u64)
        );
    }

    #[test]
    fn cross_rate_excludes_a_vote_without_an_eligible_reference_leg() {
        let included = Address::new([1u8; 20]);
        let missing = Address::new([2u8; 20]);
        let reference_votes = vec![(included, U256::from(2_000_000u64))];
        let ballot = vec![
            VoteForTally {
                exchange_rate: U256::from(4_000_000u64),
                volume: U256::from(1_000_000u64),
                voter: included,
                power: U256::from(10u64),
            },
            VoteForTally {
                exchange_rate: U256::from(5_000_000u64),
                volume: U256::from(1_000_000u64),
                voter: missing,
                power: U256::from(10u64),
            },
        ];

        let cross = to_cross_rate(
            &ballot,
            &reference_votes,
            AddressPair::new_coen_to(840),
            AddressPair::new_coen_to(978),
        )
        .unwrap();

        assert_eq!(cross.len(), 1);
        assert_eq!(cross[0].voter, included);
    }

    #[test]
    fn mixed_scale_cross_rate_round_trips_from_coen_iso_to_generic() {
        let voter = Address::new([1u8; 20]);
        let reference_pair = AddressPair::new_coen_to(840);
        let target_pair =
            AddressPair::from_addresses(Address::new([2u8; 20]), Address::new([3u8; 20]));
        let reference_rate = U256::from(2_000_000u64);
        let target_rate = fixed18(40_000);
        let ballot = vec![VoteForTally {
            exchange_rate: target_rate,
            volume: U256::ONE,
            voter,
            power: U256::ONE,
        }];

        let cross = to_cross_rate(
            &ballot,
            &[(voter, reference_rate)],
            reference_pair,
            target_pair,
        )
        .unwrap();

        assert_eq!(cross[0].exchange_rate, U256::from(50_000_000_000_000u64));
        assert_eq!(
            from_cross_rate(
                reference_rate,
                cross[0].exchange_rate,
                reference_pair,
                target_pair,
            )
            .unwrap(),
            target_rate
        );
    }

    #[test]
    fn mixed_scale_cross_rate_round_trips_from_generic_to_coen_iso() {
        let voter = Address::new([1u8; 20]);
        let reference_pair =
            AddressPair::from_addresses(Address::new([2u8; 20]), Address::new([3u8; 20]));
        let target_pair = AddressPair::new_coen_to(978);
        let reference_rate = fixed18(2_000);
        let target_rate = U256::from(4_000_000u64);
        let ballot = vec![VoteForTally {
            exchange_rate: target_rate,
            volume: U256::ONE,
            voter,
            power: U256::ONE,
        }];

        let cross = to_cross_rate(
            &ballot,
            &[(voter, reference_rate)],
            reference_pair,
            target_pair,
        )
        .unwrap();

        assert_eq!(cross[0].exchange_rate, fixed18(500));
        assert_eq!(
            from_cross_rate(
                reference_rate,
                cross[0].exchange_rate,
                reference_pair,
                target_pair,
            )
            .unwrap(),
            target_rate
        );
    }
}
