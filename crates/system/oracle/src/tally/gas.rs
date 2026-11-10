//! Gas price tally: each chain in `gas_chains` is one more vote target.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

use super::ballot::{
    apply_winners, credit_win, evaluate_pair, mark_participation, observation_count, pair_quorum,
    vote_has_price,
};
use super::{Claim, VoteForTally};
use crate::schema::OracleContract;

/// Tallies each chain in `gas_chains` as a pair is tallied, without a cross
/// rate and with `config_gas_reward_band`:
/// - With quorum, the Oracle writes the median, and a vote inside the band wins.
/// - Without quorum, the chain keeps its last price, and each vote with a
///   price wins.
/// - A zero price or no price is no win.
///
/// Returns the number of gas vote targets.
pub(super) fn tally_gas(
    oracle: &mut OracleContract,
    claims: &mut [(Address, Claim)],
    timestamp: u64,
) -> Result<u32> {
    let chains = oracle.gas_chains.read_all()?;
    let voters: Vec<Address> = oracle
        .voter_list
        .read_all()?
        .into_iter()
        .filter(|voter| claims.iter().any(|(validator, _)| validator == voter))
        .collect();
    let quorum = pair_quorum(claims.len());
    let reward_band = oracle.config_gas_reward_band.read()?;
    for &chain_id in &chains {
        let mut ballot = Vec::with_capacity(voters.len());
        for &voter in &voters {
            let price = oracle.vote_gas_price.get_nested(&voter).read(&chain_id)?;
            if !price.is_zero() {
                mark_participation(claims, voter);
            }
            ballot.push(VoteForTally {
                exchange_rate: price,
                volume: U256::ZERO,
                voter,
            });
        }
        if observation_count(&ballot) < quorum {
            for vote in ballot.iter().filter(|vote| vote_has_price(vote)) {
                credit_win(claims, vote.voter);
            }
            continue;
        }
        let outcome = evaluate_pair(&ballot, reward_band)?;
        apply_winners(claims, &outcome.winning_validators);
        oracle.gas_price.write(&chain_id, outcome.median)?;
        oracle.gas_price_timestamp.write(&chain_id, timestamp)?;
    }
    Ok(chains.len() as u32)
}
