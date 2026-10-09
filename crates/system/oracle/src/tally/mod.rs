//! Oracle tally algorithm: validator median, standard deviation, reward band.
//!
//! Ported from Cosmos SDK `x/oracle/tally.go` and `x/oracle/types/ballot.go`.
//! Rates and volumes remain in each pair's registered scale. Dimensionless
//! reward/validity ratios and the unchanged generic cross-rate use FP18.

mod ballot;
mod cross_rate;
mod round;
mod slash_window;

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

use crate::schema::OracleContract;

pub use ballot::{isqrt, median, standard_deviation, tally_pair};
pub use cross_rate::to_cross_rate;
pub use slash_window::slash_and_reset_counters;

/// Maximum validator records processed by the receipt-visible Oracle slash-window
/// system transaction. The configured genesis maximum is 128. An explicit cap
/// makes the gas bound of the mandatory phase protocol-visible.
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
}

/// Per-validator claim tracking across all pairs during a tally round.
#[derive(Clone, Debug, Default)]
pub struct Claim {
    /// Number of pairs where this validator's vote was within reward band.
    pub win_count: u32,
    /// Whether the validator submitted any vote.
    pub did_vote: bool,
}

/// Orchestrates the full tally for all pairs in a vote period.
///
/// 1. Reads all votes from storage
/// 2. Organizes into per-pair ballots
/// 3. Picks reference pair (most validator observations)
/// 4. Tallies reference pair directly, others via cross-rate
/// 5. Updates exchange rates and snapshots
/// 6. Counts miss/success/abstain per validator
/// 7. Clears votes
pub fn run_tally(oracle: &mut OracleContract, block_number: u64, timestamp: u64) -> Result<()> {
    let storage = oracle.storage.clone();
    storage.with_checkpoint(|| round::run_tally_inner(oracle, block_number, timestamp))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, U256};
    use outbe_primitives::address_pair::AddressPair;

    use super::ballot::{evaluate_pair, pair_quorum};
    use super::cross_rate::from_cross_rate;
    use super::*;
    use crate::schema::SCALE_1E18;

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

    fn vote(voter: u8, rate: u64, volume: U256) -> VoteForTally {
        VoteForTally {
            exchange_rate: fixed18(rate),
            volume,
            voter: Address::new([voter; 20]),
        }
    }

    fn outcome_volume(ballot: &[VoteForTally]) -> U256 {
        let reward_band = U256::from(20_000_000_000_000_000u128);
        evaluate_pair(ballot, reward_band).unwrap().volume
    }

    #[test]
    fn volume_is_the_winners_median_and_ignores_a_rate_outlier() {
        let ballot = [
            vote(1, 100, U256::from(10u64)),
            vote(2, 101, U256::from(21u64)),
            vote(3, 200, U256::MAX),
        ];
        assert_eq!(outcome_volume(&ballot), U256::from(15u64));
    }

    #[test]
    fn volume_median_of_an_odd_winner_set_is_the_central_volume() {
        let ballot = [
            vote(1, 100, U256::from(5u64)),
            vote(2, 100, U256::MAX),
            vote(3, 100, U256::ONE),
        ];
        assert_eq!(outcome_volume(&ballot), U256::from(5u64));
    }

    #[test]
    fn zero_volume_counts_as_an_observation() {
        let ballot = [
            vote(1, 100, U256::ZERO),
            vote(2, 100, U256::ZERO),
            vote(3, 100, U256::from(7u64)),
        ];
        assert_eq!(outcome_volume(&ballot), U256::ZERO);
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
    fn median_returns_the_only_observation() {
        let ballot = vec![VoteForTally {
            exchange_rate: fixed18(100u64),
            volume: SCALE_1E18,
            voter: Address::new([1u8; 20]),
        }];
        assert_eq!(median(&ballot), fixed18(100u64));
    }

    #[test]
    fn median_returns_the_central_observation_for_an_odd_ballot() {
        let ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(100u64),
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
            },
            VoteForTally {
                exchange_rate: fixed18(200u64),
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
            },
            VoteForTally {
                exchange_rate: fixed18(300u64),
                volume: SCALE_1E18,
                voter: Address::new([3u8; 20]),
            },
        ];
        assert_eq!(median(&ballot), fixed18(200u64));
    }

    #[test]
    fn median_averages_the_two_central_observations_for_an_even_ballot() {
        let ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(100u64),
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
            },
            VoteForTally {
                exchange_rate: fixed18(200u64),
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
            },
        ];
        assert_eq!(median(&ballot), fixed18(150));
    }

    #[test]
    fn median_midpoint_floors_in_the_rate_minor_unit() {
        let ballot = vec![
            VoteForTally {
                exchange_rate: U256::from(100u64),
                volume: U256::ONE,
                voter: Address::new([1u8; 20]),
            },
            VoteForTally {
                exchange_rate: U256::from(201u64),
                volume: U256::ONE,
                voter: Address::new([2u8; 20]),
            },
        ];

        assert_eq!(median(&ballot), U256::from(150u64));
    }

    #[test]
    fn median_is_invariant_under_input_permutation_after_sorting() {
        let mut ballot = vec![
            VoteForTally {
                exchange_rate: U256::from(300u64),
                volume: U256::ONE,
                voter: Address::new([1u8; 20]),
            },
            VoteForTally {
                exchange_rate: U256::from(100u64),
                volume: U256::ONE,
                voter: Address::new([2u8; 20]),
            },
            VoteForTally {
                exchange_rate: U256::from(200u64),
                volume: U256::ONE,
                voter: Address::new([3u8; 20]),
            },
        ];
        ballot.sort_by_key(|vote| vote.exchange_rate);
        assert_eq!(median(&ballot), U256::from(200u64));
    }

    #[test]
    fn median_of_an_empty_ballot_is_zero() {
        let ballot: Vec<VoteForTally> = vec![];
        assert_eq!(median(&ballot), U256::ZERO);
    }

    #[test]
    fn test_standard_deviation_identical() {
        // All same rate -> std dev = 0
        let ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(100u64),
                volume: SCALE_1E18,
                voter: Address::new([1u8; 20]),
            },
            VoteForTally {
                exchange_rate: fixed18(100u64),
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
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
            },
            VoteForTally {
                exchange_rate: rate_b,
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
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
            },
            VoteForTally {
                exchange_rate: U256::MAX,
                volume: SCALE_1E18,
                voter: Address::new([2u8; 20]),
            },
        ];
        let std_dev = standard_deviation(&ballot, U256::ZERO).unwrap();
        assert_eq!(std_dev, U256::MAX);
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
            },
            VoteForTally {
                exchange_rate: fixed18(100),
                volume: U256::ONE,
                voter: voters[1],
            },
            VoteForTally {
                exchange_rate: fixed18(101),
                volume: U256::ONE,
                voter: voters[2],
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
    fn reward_band_excludes_one_rate_unit_beyond_the_lower_boundary() {
        let voters = [
            Address::new([1u8; 20]),
            Address::new([2u8; 20]),
            Address::new([3u8; 20]),
        ];
        let mut ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(99) - U256::ONE,
                volume: U256::ONE,
                voter: voters[0],
            },
            VoteForTally {
                exchange_rate: fixed18(100),
                volume: U256::ONE,
                voter: voters[1],
            },
            VoteForTally {
                exchange_rate: fixed18(101),
                volume: U256::ONE,
                voter: voters[2],
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
        assert_eq!(claims[0].1.win_count, 0);
        assert_eq!(claims[1].1.win_count, 1);
        assert_eq!(claims[2].1.win_count, 1);
    }

    #[test]
    fn test_tally_pair_winners() {
        // 3 validators voting on one pair.
        // Rates: 100, 101, 200 (1e18 scaled).
        // With one validator per observation, the central rate is 101.
        // StdDev: deviations from 101 = |100-101|=1, |101-101|=0, |200-101|=99
        // Squared: 1, 0, 9801. Sum=9802. Variance=9802/3=3267.33. StdDev=sqrt(3267.33)~=57.16
        // Reward band = 0.02 * 1e18. base_spread = 101 * 0.02 / 2 = 1.01.
        // Since stddev(57.16) > base_spread(1.01), reward_spread = 57.16.
        // Range: [101-57.16, 101+57.16] = [43.84, 158.16]
        // Vote 100 is in range -> win. Vote 101 is in range -> win.
        // Vote 200 is NOT in range -> miss.

        let addr1 = Address::new([1u8; 20]);
        let addr2 = Address::new([2u8; 20]);
        let addr3 = Address::new([3u8; 20]);

        let mut ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(100u64),
                volume: SCALE_1E18,
                voter: addr1,
            },
            VoteForTally {
                exchange_rate: fixed18(101u64),
                volume: SCALE_1E18,
                voter: addr2,
            },
            VoteForTally {
                exchange_rate: fixed18(200u64),
                volume: SCALE_1E18,
                voter: addr3,
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

        // Voters 1 and 2 should have won. Voter 3 should have missed.
        assert_eq!(claims[0].1.win_count, 1); // addr1: rate 100, in range
        assert_eq!(claims[1].1.win_count, 1); // addr2: rate 101, in range
        assert_eq!(claims[2].1.win_count, 0); // addr3: rate 200, out of range
        assert!(claims[0].1.did_vote);
        assert!(claims[1].1.did_vote);
        assert!(claims[2].1.did_vote);
    }

    #[test]
    fn tally_pair_excludes_zero_rate_from_price_and_rewards() {
        let valid = Address::new([1u8; 20]);
        let zero_rate = Address::new([2u8; 20]);
        let mut ballot = vec![
            VoteForTally {
                exchange_rate: fixed18(100),
                volume: SCALE_1E18,
                voter: valid,
            },
            VoteForTally {
                exchange_rate: U256::ZERO,
                volume: SCALE_1E18,
                voter: zero_rate,
            },
        ];
        let mut claims = vec![(valid, Claim::default()), (zero_rate, Claim::default())];

        let median = tally_pair(&mut ballot, U256::ZERO, &mut claims).unwrap();

        assert_eq!(median, fixed18(100));
        assert_eq!(claims[0].1.win_count, 1);
        assert_eq!(claims[1].1.win_count, 0);
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
            },
            VoteForTally {
                exchange_rate: fixed18(40200u64),
                volume: SCALE_1E18,
                voter: addr2,
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
            },
            VoteForTally {
                exchange_rate: U256::from(5_000_000u64),
                volume: U256::from(1_000_000u64),
                voter: missing,
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
    fn mixed_scale_cross_rate_round_trip_documents_flooring_loss() {
        let voter = Address::new([1u8; 20]);
        let reference_pair = AddressPair::new_coen_to(840);
        let target_pair =
            AddressPair::from_addresses(Address::new([2u8; 20]), Address::new([3u8; 20]));
        let reference_rate = U256::from(1_000_000u64);
        let target_rate = fixed18(3);
        let ballot = vec![VoteForTally {
            exchange_rate: target_rate,
            volume: U256::ONE,
            voter,
        }];

        let cross = to_cross_rate(
            &ballot,
            &[(voter, reference_rate)],
            reference_pair,
            target_pair,
        )
        .unwrap();

        assert_eq!(
            cross[0].exchange_rate,
            U256::from(333_333_333_333_333_333u64)
        );
        assert_eq!(
            from_cross_rate(
                reference_rate,
                cross[0].exchange_rate,
                reference_pair,
                target_pair,
            )
            .unwrap(),
            target_rate + U256::from(3u64)
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
