//! Property tests for the tally arithmetic: the median, the standard deviation
//! and the integer square root.

use alloy_primitives::{Address, U256};
use proptest::prelude::*;

use crate::tally::{isqrt, median, standard_deviation, VoteForTally};

fn ballot(rates: &[u128]) -> Vec<VoteForTally> {
    let mut votes: Vec<VoteForTally> = rates
        .iter()
        .map(|&rate| VoteForTally {
            exchange_rate: U256::from(rate),
            volume: U256::ZERO,
            voter: Address::ZERO,
        })
        .collect();
    votes.sort_by_key(|vote| vote.exchange_rate);
    votes
}

proptest! {
    /// The median of a sorted ballot lies between its lowest and highest rate.
    #[test]
    fn median_lies_within_the_ballot(rates in prop::collection::vec(1u128.., 1..64)) {
        let votes = ballot(&rates);
        let m = median(&votes);
        prop_assert!(votes[0].exchange_rate <= m && m <= votes[votes.len() - 1].exchange_rate);
    }

    /// The standard deviation is zero for equal rates and never exceeds the rate range.
    #[test]
    fn standard_deviation_is_bounded_by_the_range(rates in prop::collection::vec(1u128.., 1..64)) {
        let votes = ballot(&rates);
        let spread = standard_deviation(&votes, median(&votes)).unwrap();
        let range = votes[votes.len() - 1].exchange_rate - votes[0].exchange_rate;
        prop_assert!(spread <= range);
    }

    /// isqrt(n) is the floor of the square root: isqrt(n)^2 <= n < (isqrt(n) + 1)^2.
    #[test]
    fn isqrt_is_the_floor_square_root(n in any::<u128>()) {
        let n = U256::from(n);
        let root = isqrt(n);
        let next = root + U256::from(1u64);
        prop_assert!(root * root <= n && n < next * next);
    }
}
