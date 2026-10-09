//! Cross-rate conversion between a pair ballot and the reference pair.

use alloy_primitives::{Address, U256, U512};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;

use super::ballot::vote_has_price;
use super::VoteForTally;
use crate::constants::reciprocal_scale;
use crate::schema::SCALE_1E18;

/// Converts a ballot to cross-rates using a reference pair's votes.
///
/// For each voter, the cross-rate is: `reference_rate / vote_rate`.
/// The function excludes each row independently when one of these is true:
/// - The row has no eligible reference leg.
/// - The cross-rate of the row is unrepresentable.
/// - The positive input of the row floors to zero.
///
/// One bad validator row must not abort the mandatory round tally.
pub fn to_cross_rate(
    ballot: &[VoteForTally],
    reference_votes: &[(Address, U256)],
    reference_pair: AddressPair,
    target_pair: AddressPair,
) -> Result<Vec<VoteForTally>> {
    let reference_scale = reciprocal_scale(reference_pair);
    let target_scale = reciprocal_scale(target_pair);
    let mut cross_ballot = Vec::with_capacity(ballot.len());
    for vote in ballot.iter().filter(|vote| vote_has_price(vote)) {
        let Some(reference_rate) = reference_votes
            .iter()
            .find(|(address, rate)| *address == vote.voter && !rate.is_zero())
            .map(|(_, rate)| *rate)
        else {
            continue;
        };
        let Some(numerator) = U512::from(reference_rate)
            .checked_mul(U512::from(target_scale))
            .and_then(|value| value.checked_mul(U512::from(SCALE_1E18)))
        else {
            continue;
        };
        let Some(denominator) =
            U512::from(reference_scale).checked_mul(U512::from(vote.exchange_rate))
        else {
            continue;
        };
        let Some(cross) = numerator
            .checked_div(denominator)
            .and_then(narrow_cross_rate)
        else {
            continue;
        };
        if cross.is_zero() {
            continue;
        }
        cross_ballot.push(VoteForTally {
            exchange_rate: cross,
            volume: vote.volume,
            voter: vote.voter,
        });
    }
    Ok(cross_ballot)
}

pub(super) fn from_cross_rate(
    reference_rate: U256,
    cross_rate: U256,
    reference_pair: AddressPair,
    target_pair: AddressPair,
) -> Option<U256> {
    let numerator = U512::from(reference_rate)
        .checked_mul(U512::from(reciprocal_scale(target_pair)))
        .and_then(|value| value.checked_mul(U512::from(SCALE_1E18)))?;
    let denominator =
        U512::from(reciprocal_scale(reference_pair)).checked_mul(U512::from(cross_rate))?;
    let rate = numerator
        .checked_div(denominator)
        .and_then(narrow_cross_rate)?;
    (!rate.is_zero()).then_some(rate)
}

fn narrow_cross_rate(value: U512) -> Option<U256> {
    if value > U512::from(U256::MAX) {
        return None;
    }
    Some(value.wrapping_to::<U256>())
}
