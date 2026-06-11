//! Oracle tally algorithm: weighted median, standard deviation, reward band.
//!
//! Ported from Cosmos SDK `x/oracle/tally.go` and `x/oracle/types/ballot.go`.
//! All arithmetic uses U256 fixed-point with 1e18 scale factor.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::units::Units;

use crate::contract::{OracleContract, SCALE_1E18};
use crate::precompile::IOracle;

/// Maximum validator records processed by the receipt-visible Oracle slash-window
/// system transaction. The configured genesis maximum is 128; keeping the cap
/// explicit makes the mandatory phase's gas bound protocol-visible.
pub const MAX_ORACLE_SLASH_WINDOW_VALIDATORS: usize = 128;

/// A single vote entry in a ballot for one trading pair.
#[derive(Clone, Debug)]
pub struct VoteForTally {
    /// Exchange rate (1e18 scaled).
    pub exchange_rate: U256,
    /// Volume (1e18 scaled).
    pub volume: U256,
    /// Validator address.
    pub voter: Address,
    /// Consensus power (stake-proportional weight).
    pub power: u64,
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

/// Computes the weighted median of a ballot.
///
/// The ballot must be sorted by exchange_rate in ascending order.
/// Returns the exchange rate of the vote where cumulative power crosses
/// the 50% threshold. Returns zero if the ballot is empty.
pub fn weighted_median(ballot: &[VoteForTally]) -> U256 {
    if ballot.is_empty() {
        return U256::ZERO;
    }

    let total_power: u64 = ballot.iter().map(|v| v.power).sum();
    if total_power == 0 {
        return U256::ZERO;
    }

    let half = total_power / 2;
    let mut cumulative: u64 = 0;

    for vote in ballot {
        cumulative += vote.power;
        if cumulative >= half {
            return vote.exchange_rate;
        }
    }

    // Should not reach here if ballot is non-empty with non-zero power
    ballot.last().map_or(U256::ZERO, |v| v.exchange_rate)
}

/// Computes the population standard deviation of a ballot around a given median.
///
/// Formula: sqrt(sum((rate_i - median)^2) / count)
/// Uses integer sqrt (Newton's method) for determinism.
/// All values at 1e18 scale, so squared deviations are at 1e36 scale.
pub fn standard_deviation(ballot: &[VoteForTally], median: U256) -> U256 {
    if ballot.is_empty() {
        return U256::ZERO;
    }

    let count = U256::from(ballot.len() as u64);
    let mut sum_sq = U256::ZERO;

    for vote in ballot {
        // deviation = |rate - median| (unsigned arithmetic)
        let deviation = if vote.exchange_rate > median {
            vote.exchange_rate - median
        } else {
            median - vote.exchange_rate
        };
        // deviation^2 (at 1e36 scale since each operand is 1e18)
        // To keep the result at 1e18 scale after sqrt, we need variance at 1e36.
        // Since deviation is already at 1e18, deviation^2 is at 1e36. Good.
        // On overflow: return ZERO (deterministic fallback — base_spread is used instead).
        let sq = match deviation.checked_mul(deviation) {
            Some(v) => v,
            None => return U256::ZERO,
        };
        sum_sq = match sum_sq.checked_add(sq) {
            Some(v) => v,
            None => return U256::ZERO,
        };
    }

    // variance = sum_sq / count (at 1e36 scale)
    let variance = sum_sq / count;

    // sqrt(variance) → result at 1e18 scale (sqrt of 1e36 = 1e18)
    isqrt(variance)
}

/// Runs the tally algorithm for a single pair's ballot.
///
/// Computes weighted median, standard deviation, reward spread, and marks
/// winners in the claim map.
///
/// Edge case: if median=0 (all votes are zero), reward_spread=0 and only
/// zero-rate votes are marked as winners. This is deterministic and intentional —
/// an all-zero ballot means no price data, so no validator should be rewarded.
///
/// Returns the weighted median exchange rate.
pub fn tally_pair(
    ballot: &mut [VoteForTally],
    reward_band: U256,
    claims: &mut [(Address, Claim)],
) -> U256 {
    if ballot.is_empty() {
        return U256::ZERO;
    }

    // Sort ballot by exchange rate (ascending)
    ballot.sort_by_key(|a| a.exchange_rate);

    let median = weighted_median(ballot);
    let std_dev = standard_deviation(ballot, median);

    // reward_spread = max(std_dev, median * reward_band / 2 / 1e18)
    // reward_band is at 1e18 scale, so median * reward_band gives 1e36.
    // Divide by 2 * 1e18 to get back to 1e18.
    let base_spread = median.checked_mul(reward_band).unwrap_or(U256::MAX) / (U256::in_units(2u64));
    let reward_spread = if std_dev > base_spread {
        std_dev
    } else {
        base_spread
    };

    // Determine lower and upper bounds for winning votes
    let lower = median.saturating_sub(reward_spread);
    let upper = median.saturating_add(reward_spread);

    // Mark winners
    for vote in ballot.iter() {
        if vote.exchange_rate >= lower && vote.exchange_rate <= upper {
            // Find this voter in claims and increment win_count
            for (addr, claim) in claims.iter_mut() {
                if *addr == vote.voter {
                    claim.win_count += 1;
                    break;
                }
            }
        }
        // Mark did_vote for all participants
        for (addr, claim) in claims.iter_mut() {
            if *addr == vote.voter {
                claim.did_vote = true;
                break;
            }
        }
    }

    median
}

/// Converts a ballot to cross-rates using a reference pair's votes.
///
/// For each voter, the cross-rate is: `reference_rate / vote_rate`.
/// Voters without a reference vote or with zero vote get zero rate and zero power.
pub fn to_cross_rate(
    ballot: &[VoteForTally],
    reference_votes: &[(Address, U256)],
) -> Vec<VoteForTally> {
    ballot
        .iter()
        .map(|vote| {
            let ref_rate = reference_votes
                .iter()
                .find(|(addr, _)| *addr == vote.voter)
                .map(|(_, rate)| *rate);

            match ref_rate {
                Some(r) if !r.is_zero() && !vote.exchange_rate.is_zero() => {
                    // cross_rate = ref_rate * 1e18 / vote_rate
                    // Both are at 1e18 scale, so ref * 1e18 / vote = cross at 1e18
                    let cross = r
                        .checked_mul(SCALE_1E18)
                        .unwrap_or(U256::ZERO)
                        .checked_div(vote.exchange_rate)
                        .unwrap_or(U256::ZERO);
                    VoteForTally {
                        exchange_rate: cross,
                        volume: vote.volume,
                        voter: vote.voter,
                        power: vote.power,
                    }
                }
                _ => VoteForTally {
                    exchange_rate: U256::ZERO,
                    volume: U256::ZERO,
                    voter: vote.voter,
                    power: 0, // zero power = abstain
                },
            }
        })
        .collect()
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
        // No votes this period — all validators get abstain
        for v in &all_validators {
            oracle.increment_abstain(&v.validator_address)?;
        }
        oracle.clear_votes()?;
        return Ok(());
    }

    // Collect vote targets (active pairs)
    let pair_count = oracle.pair_count.read()?;
    let mut active_pairs: Vec<(u32, B256)> = Vec::new();
    for pid in 1..=pair_count {
        let hash = oracle.pair_id_to_hash.read(&pid)?;
        let is_target = oracle.vote_target.read(&hash)?;
        if is_target {
            active_pairs.push((pid, hash));
        }
    }
    let total_targets = active_pairs.len() as u32;

    if total_targets == 0 {
        oracle.clear_votes()?;
        return Ok(());
    }

    // Organize votes into per-pair ballots
    // ballot_map: pair_id => Vec<VoteForTally>
    let mut ballot_map: Vec<(u32, B256, Vec<VoteForTally>)> = active_pairs
        .iter()
        .map(|(pid, hash)| (*pid, *hash, Vec::new()))
        .collect();

    for vi in 0..voter_count {
        let voter = oracle.voter_list.get(vi)?.unwrap_or(Address::ZERO);
        let tuple_count = oracle.vote_tuple_count.read(&voter)?;

        let pair_id_map = oracle.vote_pair_id.get_nested(&voter);
        let rate_map = oracle.vote_rate.get_nested(&voter);
        let volume_map = oracle.vote_volume.get_nested(&voter);

        // Look up validator power from the active set
        let power = all_validators
            .iter()
            .find(|v| v.validator_address == voter)
            .map(|v| {
                // Use stake as power. Convert U256 to u64 by dividing by 1e18.
                // This gives units in whole tokens as consensus power.
                (v.stake / SCALE_1E18).saturating_to::<u64>()
            })
            .unwrap_or(0);

        for ti in 0..tuple_count {
            let pair_id = pair_id_map.read(&ti)?;
            let rate = rate_map.read(&ti)?;
            let volume = volume_map.read(&ti)?;

            // Find the ballot for this pair_id
            if let Some((_, _, ballot)) = ballot_map.iter_mut().find(|(pid, _, _)| *pid == pair_id)
            {
                ballot.push(VoteForTally {
                    exchange_rate: rate,
                    volume,
                    voter,
                    power,
                });
            }
        }
    }

    // Pick reference pair: highest total voting power
    let ref_pair_idx = ballot_map
        .iter()
        .enumerate()
        .max_by_key(|(_, (_, _, ballot))| ballot.iter().map(|v| v.power).sum::<u64>())
        .map(|(idx, _)| idx)
        .unwrap_or(0);

    // Tally reference pair directly
    let ref_pair_id = ballot_map[ref_pair_idx].0;
    let ref_pair_hash = ballot_map[ref_pair_idx].1;
    let ref_median = {
        let ballot = &mut ballot_map[ref_pair_idx].2;
        tally_pair(ballot, reward_band, &mut claims)
    };

    // Collect reference votes for cross-rate (voter => reference rate)
    let reference_votes: Vec<(Address, U256)> = ballot_map[ref_pair_idx]
        .2
        .iter()
        .map(|v| (v.voter, v.exchange_rate))
        .collect();

    // Update reference pair exchange rate
    if !ref_median.is_zero() {
        oracle.update_exchange_rate(ref_pair_hash, ref_median, block_number, timestamp)?;
        let event = IOracle::ExchangeRateUpdated {
            pairId: ref_pair_id,
            rate: ref_median,
            blockNumber: block_number,
        };
        let _ = oracle
            .storage
            .emit_event(ORACLE_ADDRESS, event.encode_log_data());
    }

    // Snapshot entries to collect
    let mut snapshot_entries: Vec<(u32, U256, U256)> = Vec::new();
    if !ref_median.is_zero() {
        let total_volume: U256 = ballot_map[ref_pair_idx]
            .2
            .iter()
            .map(|v| v.volume)
            .fold(U256::ZERO, |acc, v| acc.saturating_add(v));
        snapshot_entries.push((ref_pair_id, ref_median, total_volume));
    }

    // Tally other pairs via cross-rate
    for (i, entry) in ballot_map.iter().enumerate() {
        if i == ref_pair_idx {
            continue;
        }

        let pair_id = entry.0;
        let pair_hash = entry.1;
        let ballot = &entry.2;

        if ballot.is_empty() {
            continue;
        }

        // Convert to cross-rates
        let mut cross_ballot = to_cross_rate(ballot, &reference_votes);

        // Tally the cross-rate ballot
        let cross_median = tally_pair(&mut cross_ballot, reward_band, &mut claims);

        // Convert cross-rate median back to actual rate:
        // actual_rate = reference_median * 1e18 / cross_median
        if !cross_median.is_zero() && !ref_median.is_zero() {
            let actual_rate = ref_median
                .checked_mul(SCALE_1E18)
                .unwrap_or(U256::ZERO)
                .checked_div(cross_median)
                .unwrap_or(U256::ZERO);

            if !actual_rate.is_zero() {
                oracle.update_exchange_rate(pair_hash, actual_rate, block_number, timestamp)?;
                let event = IOracle::ExchangeRateUpdated {
                    pairId: pair_id,
                    rate: actual_rate,
                    blockNumber: block_number,
                };
                let _ = oracle
                    .storage
                    .emit_event(ORACLE_ADDRESS, event.encode_log_data());

                let total_volume: U256 = ballot
                    .iter()
                    .map(|v| v.volume)
                    .fold(U256::ZERO, |acc, v| acc.saturating_add(v));
                snapshot_entries.push((pair_id, actual_rate, total_volume));
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
    let all_validators = vs.get_all_validators()?;
    if all_validators.len() > MAX_ORACLE_SLASH_WINDOW_VALIDATORS {
        return Err(PrecompileError::Revert(format!(
            "Oracle slash-window validator set size {} exceeds cap {}",
            all_validators.len(),
            MAX_ORACLE_SLASH_WINDOW_VALIDATORS
        )));
    }

    for v in &all_validators {
        let addr = v.validator_address;

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
        let valid_rate = U256::in_units(success) / U256::from(total);

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
            exchange_rate: U256::in_units(100u64),
            volume: SCALE_1E18,
            voter: Address::new([1u8; 20]),
            power: 10,
        }];
        assert_eq!(weighted_median(&ballot), U256::in_units(100u64));
    }

    #[test]
    fn test_weighted_median_odd_power() {
        // Three voters: powers 10, 20, 30. Total=60, half=30.
        // Sorted by rate: 100, 200, 300
        // Cumsum: 10 (<30), 30 (>=30) → median = 200
        let ballot = vec![
            VoteForTally {
                exchange_rate: U256::in_units(100u64),
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
                power: 10,
            },
            VoteForTally {
                exchange_rate: U256::in_units(200u64),
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
                power: 20,
            },
            VoteForTally {
                exchange_rate: U256::in_units(300u64),
                volume: SCALE_1E18,
                voter: Address::new([3u8; 20]),
                power: 30,
            },
        ];
        assert_eq!(weighted_median(&ballot), U256::in_units(200u64));
    }

    #[test]
    fn test_weighted_median_equal_power() {
        // Equal power: 10, 10, 10. Total=30, half=15.
        // Cumsum: 10 (<15), 20 (>=15) → median = 200
        let ballot = vec![
            VoteForTally {
                exchange_rate: U256::in_units(100u64),
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
                power: 10,
            },
            VoteForTally {
                exchange_rate: U256::in_units(200u64),
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
                power: 10,
            },
            VoteForTally {
                exchange_rate: U256::in_units(300u64),
                volume: SCALE_1E18,
                voter: Address::new([3u8; 20]),
                power: 10,
            },
        ];
        assert_eq!(weighted_median(&ballot), U256::in_units(200u64));
    }

    #[test]
    fn test_weighted_median_empty() {
        let ballot: Vec<VoteForTally> = vec![];
        assert_eq!(weighted_median(&ballot), U256::ZERO);
    }

    #[test]
    fn test_standard_deviation_identical() {
        // All same rate → std dev = 0
        let ballot = vec![
            VoteForTally {
                exchange_rate: U256::in_units(100u64),
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
                power: 10,
            },
            VoteForTally {
                exchange_rate: U256::in_units(100u64),
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
                power: 10,
            },
        ];
        let median = U256::in_units(100u64);
        assert_eq!(standard_deviation(&ballot, median), U256::ZERO);
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
        // total=20, half=10. cumsum after first: 10 >= 10 → median = 100.
        // Deviations: |100-100|=0, |200-100|=100e18.
        // Squared: 0, (100e18)^2 = 1e40.
        // Variance = 1e40/2 = 5e39. sqrt(5e39) = sqrt(5)*1e19.5... hmm this gets complicated.
        // Let me use simpler values.

        // Rates: 8e18, 12e18. Median = 10e18 (assume given).
        // Deviations: 2e18, 2e18. Squared: 4e36, 4e36.
        // Variance = 8e36/2 = 4e36. StdDev = sqrt(4e36) = 2e18.
        let rate_a = U256::in_units(8u64);
        let rate_b = U256::in_units(12u64);
        let median = U256::in_units(10u64);
        let ballot = vec![
            VoteForTally {
                exchange_rate: rate_a,
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
                power: 10,
            },
            VoteForTally {
                exchange_rate: rate_b,
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
                power: 10,
            },
        ];
        let std_dev = standard_deviation(&ballot, median);
        assert_eq!(std_dev, U256::in_units(2u64));
    }

    #[test]
    fn test_standard_deviation_overflow_returns_zero() {
        // Very large deviation that would overflow U256 when squared.
        // U256::MAX / 2 squared overflows U256.
        let large = U256::MAX / U256::from(2u64);
        let ballot = vec![
            VoteForTally {
                exchange_rate: U256::ZERO,
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
                power: 10,
            },
            VoteForTally {
                exchange_rate: large,
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
                power: 10,
            },
        ];
        let median = large / U256::from(2u64);
        // Deviation from median is ~U256::MAX/4, squared overflows.
        // Should return ZERO deterministically, not U256::MAX.
        let std_dev = standard_deviation(&ballot, median);
        assert_eq!(std_dev, U256::ZERO);
    }

    #[test]
    fn test_tally_pair_winners() {
        // 3 validators voting on one pair.
        // Rates: 100, 101, 200 (1e18 scaled).
        // Powers: 10, 20, 10. Total=40, half=20.
        // Sorted: 100(10), 101(20), 200(10).
        // Cumsum: 10(<20), 30(>=20) → median = 101.
        // StdDev: deviations from 101 = |100-101|=1, |101-101|=0, |200-101|=99
        // Squared: 1, 0, 9801. Sum=9802. Variance=9802/3=3267.33. StdDev=sqrt(3267.33)≈57.16
        // Reward band = 0.02 * 1e18. base_spread = 101 * 0.02 / 2 = 1.01.
        // Since stddev(57.16) > base_spread(1.01), reward_spread = 57.16.
        // Range: [101-57.16, 101+57.16] = [43.84, 158.16]
        // Vote 100 is in range → win. Vote 101 is in range → win. Vote 200 is NOT in range → miss.

        let addr1 = Address::new([1u8; 20]);
        let addr2 = Address::new([2u8; 20]);
        let addr3 = Address::new([3u8; 20]);

        let mut ballot = vec![
            VoteForTally {
                exchange_rate: U256::in_units(100u64),
                volume: SCALE_1E18,
                voter: addr1,
                power: 10,
            },
            VoteForTally {
                exchange_rate: U256::in_units(101u64),
                volume: SCALE_1E18,
                voter: addr2,
                power: 20,
            },
            VoteForTally {
                exchange_rate: U256::in_units(200u64),
                volume: SCALE_1E18,
                voter: addr3,
                power: 10,
            },
        ];

        let reward_band = U256::from(20_000_000_000_000_000u128); // 0.02 * 1e18
        let mut claims = vec![
            (addr1, Claim::default()),
            (addr2, Claim::default()),
            (addr3, Claim::default()),
        ];

        let median = tally_pair(&mut ballot, reward_band, &mut claims);
        assert_eq!(median, U256::in_units(101u64));

        // Voters 1 and 2 should have won, voter 3 should have missed
        assert_eq!(claims[0].1.win_count, 1); // addr1: rate 100, in range
        assert_eq!(claims[1].1.win_count, 1); // addr2: rate 101, in range
        assert_eq!(claims[2].1.win_count, 0); // addr3: rate 200, out of range
        assert!(claims[0].1.did_vote);
        assert!(claims[1].1.did_vote);
        assert!(claims[2].1.did_vote);
    }

    #[test]
    fn test_cross_rate() {
        let addr1 = Address::new([1u8; 20]);
        let addr2 = Address::new([2u8; 20]);

        // Reference pair votes (e.g., ETH/USD): voter1=2000, voter2=2010
        let reference_votes = vec![
            (addr1, U256::in_units(2000u64)),
            (addr2, U256::in_units(2010u64)),
        ];

        // Current pair votes (e.g., BTC/USD): voter1=40000, voter2=40200
        let ballot = vec![
            VoteForTally {
                exchange_rate: U256::in_units(40000u64),
                volume: SCALE_1E18,
                voter: addr1,
                power: 10,
            },
            VoteForTally {
                exchange_rate: U256::in_units(40200u64),
                volume: SCALE_1E18,
                voter: addr2,
                power: 10,
            },
        ];

        let cross = to_cross_rate(&ballot, &reference_votes);

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
}
