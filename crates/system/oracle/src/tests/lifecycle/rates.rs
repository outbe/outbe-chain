use outbe_primitives::storage::StorageHandle;
use outbe_system_common::address_pair::AddressPair;

use super::*;

const EUR: u16 = 978;
/// The block time of the votes and the tally.
const NOW: u64 = 1_800_000_000;
/// The publication time of the first rate: one day before [`NOW`].
const DAY_1: u64 = NOW - 86_400;
/// The publication time of the next rate.
const DAY_2: u64 = NOW - 3_600;

/// An Oracle at block time [`NOW`] with EUR as the only rate vote target.
fn with_eur_rate_target(
    f: impl FnOnce(StorageHandle<'_>, &mut OracleContract<'_>, AddressPair, [Address; 4]),
) {
    with_coen840_oracle(|storage, oracle, pair| {
        storage.set_block_timestamp(U256::from(NOW)).unwrap();
        oracle.policy_rate_currencies.clear().unwrap();
        oracle.policy_rate_currencies.push(EUR).unwrap();
        let voters = register_four_voters(&storage);
        f(storage, oracle, pair, voters);
    });
}

/// Each voter votes the same pair price and its `(rate, publication time)`
/// for EUR.
fn vote_rates(
    oracle: &mut OracleContract,
    pair: AddressPair,
    voters: &[Address],
    rates: &[(u64, u64)],
) {
    for (voter, (rate, published_at)) in voters.iter().zip(rates) {
        oracle
            .submit_vote(
                *voter,
                &[pair_vote(pair, coen_iso(2), COEN_ISO_SCALE)],
                &[],
                &[(EUR, U256::from(*rate), *published_at)],
            )
            .unwrap();
    }
}

fn tally(oracle: &mut OracleContract) {
    crate::tally::run_tally(oracle, 2, NOW).unwrap();
}

#[test]
fn policy_rate_comes_from_the_rate_votes() {
    with_eur_rate_target(|_storage, oracle, pair, voters| {
        // Three of four voters are the quorum. The fourth sends no rate.
        vote_rates(
            oracle,
            pair,
            &voters[..3],
            &[(24_300, DAY_1), (24_400, DAY_1), (24_500, DAY_1)],
        );
        oracle
            .submit_vote(
                voters[3],
                &[pair_vote(pair, coen_iso(2), COEN_ISO_SCALE)],
                &[],
                &[],
            )
            .unwrap();

        tally(oracle);

        assert_eq!(
            oracle.get_policy_rate(EUR).unwrap(),
            (U256::from(24_400), DAY_1)
        );
    });
}

#[test]
fn a_rate_vote_gives_no_win_and_no_miss() {
    with_eur_rate_target(|_storage, oracle, pair, voters| {
        // Voter 3 sends no rate and voter 2 sends a rate far from the median.
        vote_rates(
            oracle,
            pair,
            &voters[..3],
            &[(24_400, DAY_1), (24_400, DAY_1), (900_000, DAY_1)],
        );
        oracle
            .submit_vote(
                voters[3],
                &[pair_vote(pair, coen_iso(2), COEN_ISO_SCALE)],
                &[],
                &[],
            )
            .unwrap();

        tally(oracle);

        assert_penalty_counters(
            oracle,
            &[
                (voters[0], Penalty::Success, 1),
                (voters[2], Penalty::Success, 1),
                (voters[3], Penalty::Success, 1),
            ],
        );
    });
}

#[test]
fn rate_keeps_its_publication_time_without_a_new_publication() {
    with_eur_rate_target(|_storage, oracle, pair, voters| {
        oracle
            .set_policy_rate_for_test(EUR, U256::from(24_400), DAY_1)
            .unwrap();

        // No quorum: two of four voters send a new rate.
        vote_rates(oracle, pair, &voters[..2], &[(30_000, DAY_2); 2]);
        tally(oracle);
        assert_eq!(
            oracle.get_policy_rate(EUR).unwrap(),
            (U256::from(24_400), DAY_1)
        );

        // No new publication: the quorum sends the same rate again.
        vote_rates(oracle, pair, &voters, &[(24_400, DAY_1); 4]);
        tally(oracle);
        assert_eq!(
            oracle.get_policy_rate(EUR).unwrap(),
            (U256::from(24_400), DAY_1)
        );

        // A new publication moves the rate and the time.
        vote_rates(oracle, pair, &voters, &[(24_500, DAY_2); 4]);
        tally(oracle);
        assert_eq!(
            oracle.get_policy_rate(EUR).unwrap(),
            (U256::from(24_500), DAY_2)
        );
    });
}

#[test]
fn the_tally_does_not_move_a_rate_back() {
    with_eur_rate_target(|_storage, oracle, pair, voters| {
        oracle
            .set_policy_rate_for_test(EUR, U256::from(24_500), DAY_2)
            .unwrap();

        vote_rates(oracle, pair, &voters, &[(24_400, DAY_1); 4]);
        tally(oracle);

        assert_eq!(
            oracle.get_policy_rate(EUR).unwrap(),
            (U256::from(24_500), DAY_2)
        );
    });
}

#[test]
fn zero_rate_is_a_rate() {
    with_eur_rate_target(|_storage, oracle, pair, voters| {
        let error = oracle.get_policy_rate(EUR).unwrap_err();
        assert_typed_revert(
            &error,
            crate::errors::OracleError::NoPolicyRate { iso_code: EUR },
        );

        vote_rates(oracle, pair, &voters, &[(0, DAY_1); 4]);
        tally(oracle);

        assert_eq!(oracle.get_policy_rate(EUR).unwrap(), (U256::ZERO, DAY_1));
    });
}

#[test]
fn rate_source_is_optional_for_another_priced_currency() {
    const CAD: u16 = 124;
    with_eur_rate_target(|_storage, oracle, pair, voters| {
        // CAD is no rate vote target: a vote for it fails, and it has no rate.
        let result = oracle.submit_vote(
            voters[0],
            &[pair_vote(pair, coen_iso(2), COEN_ISO_SCALE)],
            &[],
            &[(CAD, U256::from(30_000), DAY_1)],
        );
        assert_typed_revert(
            &result.unwrap_err(),
            crate::errors::OracleError::RateCurrencyNotVoteTarget { currency: CAD },
        );
        assert!(oracle.get_policy_rate(CAD).is_err());

        // A rate entry makes CAD a rate vote target. No reference list limits it.
        oracle.policy_rate_currencies.push(CAD).unwrap();
        for voter in voters {
            oracle
                .submit_vote(
                    voter,
                    &[pair_vote(pair, coen_iso(2), COEN_ISO_SCALE)],
                    &[],
                    &[(CAD, U256::from(30_000), DAY_1)],
                )
                .unwrap();
        }
        tally(oracle);

        assert_eq!(
            oracle.get_policy_rate(CAD).unwrap(),
            (U256::from(30_000), DAY_1)
        );
    });
}

#[test]
fn a_bad_rate_vote_fails_and_stores_no_vote() {
    use crate::errors::OracleError;

    with_eur_rate_target(|_storage, oracle, pair, voters| {
        let max = U256::from(crate::constants::MAX_POLICY_RATE_VOTE);
        for (rates, expected) in [
            (
                vec![(EUR, U256::from(1), DAY_1), (EUR, U256::from(1), DAY_1)],
                OracleError::DuplicateRateCurrencyInVote { currency: EUR },
            ),
            (
                vec![(EUR, max + U256::from(1), DAY_1)],
                OracleError::VoteRateExceedsCap {
                    currency: EUR,
                    rate: max + U256::from(1),
                    max,
                },
            ),
            (
                vec![(EUR, U256::from(1), 0)],
                OracleError::RatePublicationTimeInvalid {
                    currency: EUR,
                    published_at: 0,
                },
            ),
            (
                vec![(EUR, U256::from(1), NOW + 1)],
                OracleError::RatePublicationTimeInvalid {
                    currency: EUR,
                    published_at: NOW + 1,
                },
            ),
        ] {
            let result = oracle.submit_vote(
                voters[0],
                &[pair_vote(pair, coen_iso(2), COEN_ISO_SCALE)],
                &[],
                &rates,
            );
            assert_typed_revert(&result.unwrap_err(), expected);
            assert!(!oracle.vote_exists.read(&voters[0]).unwrap());
        }
    });
}
