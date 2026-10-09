//! Ballot statistics for one pair: median, standard deviation, reward band
//! winners and participation claims.

use alloy_primitives::{aliases::U1024, Address, U256, U512};
use outbe_primitives::error::Result;

use super::{Claim, VoteForTally};
use crate::errors::OracleError;
use crate::schema::SCALE_1E18;

#[derive(Clone, Debug)]
pub(super) struct PairTallyOutcome {
    pub(super) median: U256,
    /// Median volume of the reward-band winners.
    pub(super) volume: U256,
    pub(super) winning_validators: Vec<Address>,
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

pub(super) fn isqrt_u1024(n: U1024) -> U1024 {
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

/// Computes the median of one-price-per-validator observations.
///
/// The ballot must be sorted by exchange rate. Every validator has equal
/// weight. An even ballot uses the floored midpoint of the two central rates.
pub fn median(ballot: &[VoteForTally]) -> U256 {
    sorted_median(ballot.len(), |index| ballot[index].exchange_rate)
}
pub(super) fn sorted_median(len: usize, value_at: impl Fn(usize) -> U256) -> U256 {
    if len == 0 {
        return U256::ZERO;
    }

    let upper_index = len / 2;
    if len % 2 == 1 {
        return value_at(upper_index);
    }

    let lower = value_at(upper_index - 1);
    let upper = value_at(upper_index);
    lower + (upper - lower) / U256::from(2u64)
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
/// Computes validator median, standard deviation, reward spread, and marks
/// winners in the claim map.
///
/// Only positive-rate rows participate in price, deviation and winner
/// calculations. Every submitted row still marks participation, so a zero-rate
/// row is a miss rather than an abstention at the round level.
///
/// Returns the validator median exchange rate.
pub fn tally_pair(
    ballot: &mut [VoteForTally],
    reward_band: U256,
    claims: &mut [(Address, Claim)],
) -> Result<U256> {
    for vote in ballot.iter() {
        mark_participation(claims, vote.voter);
    }

    let outcome = evaluate_pair(ballot, reward_band)?;
    apply_winners(claims, &outcome.winning_validators);
    Ok(outcome.median)
}
pub(super) fn evaluate_pair(
    ballot: &[VoteForTally],
    reward_band: U256,
) -> Result<PairTallyOutcome> {
    if ballot.is_empty() {
        return Ok(PairTallyOutcome {
            median: U256::ZERO,
            volume: U256::ZERO,
            winning_validators: Vec::new(),
        });
    }

    let mut eligible: Vec<VoteForTally> = ballot
        .iter()
        .filter(|vote| vote_has_price(vote))
        .cloned()
        .collect();
    if eligible.is_empty() {
        return Ok(PairTallyOutcome {
            median: U256::ZERO,
            volume: U256::ZERO,
            winning_validators: Vec::new(),
        });
    }

    eligible.sort_by_key(|vote| vote.exchange_rate);

    let median = median(&eligible);
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

    let winners: Vec<&VoteForTally> = eligible
        .iter()
        .filter(|vote| vote.exchange_rate >= lower && vote.exchange_rate <= upper)
        .collect();
    let mut volumes: Vec<U256> = winners.iter().map(|vote| vote.volume).collect();
    volumes.sort_unstable();

    Ok(PairTallyOutcome {
        median,
        volume: sorted_median(volumes.len(), |index| volumes[index]),
        winning_validators: winners.iter().map(|vote| vote.voter).collect(),
    })
}

pub(super) fn apply_winners(claims: &mut [(Address, Claim)], winning_validators: &[Address]) {
    for voter in winning_validators {
        credit_win(claims, *voter);
    }
}

/// Adds one win to the claim of `voter`. A voter without a claim gets nothing.
pub(super) fn credit_win(claims: &mut [(Address, Claim)], voter: Address) {
    if let Some((_, claim)) = claims.iter_mut().find(|(address, _)| *address == voter) {
        claim.win_count += 1;
    }
}

pub(super) fn vote_has_price(vote: &VoteForTally) -> bool {
    !vote.exchange_rate.is_zero()
}

pub(super) fn observation_count(ballot: &[VoteForTally]) -> usize {
    ballot.iter().filter(|vote| vote_has_price(vote)).count()
}

/// Number of independent validator observations required for one raw pair.
/// Every active validator contributes at most one vote, regardless of stake.
pub(super) fn pair_quorum(active_validator_count: usize) -> usize {
    active_validator_count - active_validator_count / 3
}

pub(super) fn mark_participation(claims: &mut [(Address, Claim)], voter: Address) {
    if let Some((_, claim)) = claims.iter_mut().find(|(address, _)| *address == voter) {
        claim.did_vote = true;
    }
}
