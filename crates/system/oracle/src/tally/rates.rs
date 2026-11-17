//! Policy rate tally: each currency in `policy_rate_currencies` closes one
//! rate with the quorum and the median of a price.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::error::Result;
use outbe_system_common::addresses::ORACLE_ADDRESS;

use super::ballot::{pair_quorum, sorted_median};
use super::Claim;
use crate::precompile::IOracle;
use crate::schema::OracleContract;

/// Closes the policy rate of each rate vote target from the votes of the
/// active validators:
/// - With quorum, the rate is the median of the voted rates, and its
///   publication time is the median of the voted publication times.
/// - Without quorum, the currency keeps its last rate and publication time.
/// - A publication time after the block time or before the stored time writes
///   nothing.
///
/// A rate vote gives no win and no miss: the rate sources are outside the
/// control of a validator. The Oracle emits `PolicyRateSet` only when the
/// stored rate or its publication time changes.
pub(super) fn tally_rates(
    oracle: &mut OracleContract,
    claims: &[(Address, Claim)],
    timestamp: u64,
) -> Result<()> {
    let currencies = oracle.policy_rate_currencies.read_all()?;
    if currencies.is_empty() {
        return Ok(());
    }
    let voters: Vec<Address> = oracle
        .voter_list
        .read_all()?
        .into_iter()
        .filter(|voter| claims.iter().any(|(validator, _)| validator == voter))
        .collect();
    let quorum = pair_quorum(claims.len());
    for currency in currencies {
        let (mut rates, mut times) = rate_ballot(oracle, &voters, currency)?;
        if rates.len() < quorum {
            continue;
        }
        rates.sort_unstable();
        times.sort_unstable();
        let rate = sorted_median(rates.len(), |index| rates[index]);
        // The lower of the two central times of an even ballot: a time that a
        // validator voted, and never the newer one.
        let published_at = times[(times.len() - 1) / 2];
        close_rate(oracle, currency, rate, published_at, timestamp)?;
    }
    Ok(())
}

/// The voted rates of `currency` and their publication times, one of each
/// for each voter with a rate vote.
fn rate_ballot(
    oracle: &OracleContract,
    voters: &[Address],
    currency: u16,
) -> Result<(Vec<U256>, Vec<u64>)> {
    let mut rates = Vec::with_capacity(voters.len());
    let mut times = Vec::with_capacity(voters.len());
    for voter in voters {
        let published_at = oracle
            .vote_policy_rate_published_at
            .get_nested(voter)
            .read(&currency)?;
        // A publication time of zero means no rate vote.
        if published_at == 0 {
            continue;
        }
        rates.push(oracle.vote_policy_rate.get_nested(voter).read(&currency)?);
        times.push(published_at);
    }
    Ok((rates, times))
}

/// Stores `rate` and `published_at` as the closed rate of `currency` and
/// emits `PolicyRateSet`. Writes nothing for a time after the block time, a
/// time before the stored time, or an unchanged rate and time.
fn close_rate(
    oracle: &mut OracleContract,
    currency: u16,
    rate: U256,
    published_at: u64,
    timestamp: u64,
) -> Result<()> {
    let stored_at = oracle.policy_rate_published_at.read(&currency)?;
    if published_at > timestamp || published_at < stored_at {
        return Ok(());
    }
    if published_at == stored_at && rate == oracle.policy_rate.read(&currency)? {
        return Ok(());
    }
    oracle.policy_rate.write(&currency, rate)?;
    oracle
        .policy_rate_published_at
        .write(&currency, published_at)?;
    let event = IOracle::PolicyRateSet {
        currency,
        rate,
        publishedAt: published_at,
    };
    oracle
        .storage
        .emit_event(ORACLE_ADDRESS, event.encode_log_data())
}
