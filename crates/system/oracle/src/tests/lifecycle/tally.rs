use super::*;

#[test]
fn run_tally_accepts_a_single_validator_as_the_validator_median() {
    with_coen_usdt_voter(|_storage, oracle, validator| {
        let (_, _, rate, _) = submit_sample_vote(oracle, validator);

        // Run tally
        crate::tally::run_tally(oracle, 2, 24).unwrap();

        // Exchange rate should be updated to the voted rate
        assert_eq!(
            oracle.get_exchange_rate_data(COEN, USDT).unwrap(),
            (rate, 2, 24)
        );

        // Validator should get success (voted within band for all pairs)
        assert_penalty_counters(
            oracle,
            &[
                (validator, Penalty::Success, 1),
                (validator, Penalty::Miss, 0),
                (validator, Penalty::Abstain, 0),
            ],
        );

        // Votes should be cleared
        assert_eq!(oracle.voter_list.len().unwrap(), 0);

        // Snapshot should exist
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 1);
    });
}

#[test]
fn run_tally_rewards_every_voter_inside_the_reward_band() {
    with_coen_usdt_oracle(|storage, oracle| {
        let [v1, v2, v3] = register_staked_voters(&storage, [100, 200, 100]);

        // All vote very close: 1000, 1001, 1002 (spread < 0.2% of median)
        // With 2% reward band, all should be within band.
        let base = fixed18(1000);
        oracle
            .submit_vote(v1, &[(COEN, USDT, base, SCALE_1E18)])
            .unwrap();
        oracle
            .submit_vote(v2, &[(COEN, USDT, base + SCALE_1E18, SCALE_1E18)])
            .unwrap();
        oracle
            .submit_vote(v3, &[(COEN, USDT, base + fixed18(2), SCALE_1E18)])
            .unwrap();

        crate::tally::run_tally(oracle, 2, 24).unwrap();

        // One validator contributes one observation, so the central rate is 1001.
        let rate = oracle.get_exchange_rate(COEN, USDT).unwrap();
        assert_eq!(rate, fixed18(1001));

        // Reward spread = max(std_dev, 1001 * 0.02 / 2) = max(~0.816, ~10.01) = ~10.01
        // All votes within [990.99, 1011.01] -> all win
        assert_penalty_counters(
            oracle,
            &[
                (v1, Penalty::Success, 1),
                (v2, Penalty::Success, 1),
                (v3, Penalty::Success, 1),
            ],
        );
    });
}

#[test]
fn run_tally_gives_every_validator_one_median_observation_regardless_of_stake() {
    with_coen840_oracle(|storage, oracle, pair| {
        let [v1, v2, v3] = register_staked_voters(&storage, [1_000_000, 1, 1]);

        for (voter, rate) in [(v1, 100), (v2, 200), (v3, 300)] {
            oracle
                .submit_vote(voter, &[pair_vote(pair, coen_iso(rate), COEN_ISO_SCALE)])
                .unwrap();
        }

        crate::tally::run_tally(oracle, 2, 24).unwrap();

        assert_eq!(pair_rate(oracle, pair), coen_iso(200));
    });
}

#[test]
fn run_tally_penalizes_a_voter_outside_the_reward_band() {
    with_coen_usdt_oracle(|storage, oracle| {
        let [v1, v2, v3] = register_staked_voters(&storage, [100, 200, 100]);

        // v1 and v2 vote 50, v3 votes 500 (extreme outlier)
        oracle
            .submit_vote(v1, &[(COEN, USDT, fixed18(50), SCALE_1E18)])
            .unwrap();
        oracle
            .submit_vote(v2, &[(COEN, USDT, fixed18(50), SCALE_1E18)])
            .unwrap();
        oracle
            .submit_vote(v3, &[(COEN, USDT, fixed18(500), SCALE_1E18)])
            .unwrap();

        crate::tally::run_tally(oracle, 2, 24).unwrap();

        // The validator median is 50.
        let rate = oracle.get_exchange_rate(COEN, USDT).unwrap();
        assert_eq!(rate, fixed18(50));

        // v1 and v2 should be winners, v3 (outlier at 500) should miss
        assert_penalty_counters(
            oracle,
            &[
                (v1, Penalty::Success, 1),
                (v2, Penalty::Success, 1),
                (v3, Penalty::Miss, 1),
            ],
        );
    });
}

#[test]
fn run_tally_counts_a_zero_rate_submission_as_a_miss_without_poisoning_price() {
    with_coen_usdt_oracle(|storage, oracle| {
        let [valid, valid2, invalid] = register_staked_voters(&storage, [100; 3]);
        oracle
            .submit_vote(valid, &[(COEN, USDT, fixed18(50), SCALE_1E18)])
            .unwrap();
        oracle
            .submit_vote(valid2, &[(COEN, USDT, fixed18(50), SCALE_1E18)])
            .unwrap();
        oracle
            .submit_vote(invalid, &[(COEN, USDT, U256::ZERO, SCALE_1E18)])
            .unwrap();

        crate::tally::run_tally(oracle, 2, 24).unwrap();

        assert_eq!(oracle.get_exchange_rate(COEN, USDT).unwrap(), fixed18(50));
        assert_penalty_counters(
            oracle,
            &[
                (valid, Penalty::Success, 1),
                (valid2, Penalty::Success, 1),
                (invalid, Penalty::Miss, 1),
                (invalid, Penalty::Abstain, 0),
            ],
        );
    });
}

#[test]
fn run_tally_breaks_equal_reference_observation_ties_by_registry_order() {
    with_oracle(|storage, oracle| {
        let (lower, higher) = register_coen_usdt_and_usdt_eth(oracle);

        let [voter1, voter2] = register_staked_voters(&storage, [100; 2]);
        oracle
            .submit_vote(
                voter1,
                &[
                    pair_vote(lower, fixed18(50), SCALE_1E18),
                    pair_vote(higher, fixed18(2_000), SCALE_1E18),
                ],
            )
            .unwrap();
        oracle
            .submit_vote(
                voter2,
                &[
                    pair_vote(lower, fixed18(50), SCALE_1E18),
                    pair_vote(higher, fixed18(2_000), SCALE_1E18),
                ],
            )
            .unwrap();

        crate::tally::run_tally(oracle, 2, 24).unwrap();

        assert_eq!(pair_rate(oracle, lower), fixed18(50));
        assert_eq!(pair_rate(oracle, higher), fixed18(2_000));
        assert_reference_pair(oracle, lower);
    });
}

#[test]
fn later_pair_with_more_observations_becomes_the_reference_pair() {
    with_synthetic_market(
        |_, _| {},
        |oracle, (first, later), voters| {
            for (index, voter) in voters.into_iter().enumerate() {
                let mut votes = vec![pair_vote(later, fixed18(1), SCALE_1E18)];
                if index < 3 {
                    let rate = [fixed18(2), fixed18(3), fixed18(7)][index];
                    votes.push(pair_vote(first, rate, SCALE_1E18));
                }
                oracle.submit_vote(voter, &votes).unwrap();
            }

            crate::tally::run_tally(oracle, 2, 24).unwrap();

            assert_eq!(pair_rate(oracle, later), fixed18(1));
            assert_eq!(pair_rate(oracle, first), fixed18(3) + U256::from(3u64));
            assert_reference_pair(oracle, later);
        },
    );
}

#[test]
fn pair_below_quorum_is_not_selected_as_reference() {
    with_synthetic_market(
        |oracle, (first, _)| publish_prior_rate(oracle, first, fixed18(40)),
        |oracle, (first, later), voters| {
            oracle
                .submit_vote(voters[0], &[pair_vote(first, fixed18(50), SCALE_1E18)])
                .unwrap();
            oracle
                .submit_vote(
                    voters[1],
                    &[
                        pair_vote(first, fixed18(50), SCALE_1E18),
                        pair_vote(later, fixed18(2), SCALE_1E18),
                    ],
                )
                .unwrap();
            for voter in &voters[2..] {
                oracle
                    .submit_vote(*voter, &[pair_vote(later, fixed18(2), SCALE_1E18)])
                    .unwrap();
            }

            crate::tally::run_tally(oracle, 2, 24).unwrap();

            assert_prior_rate_kept(oracle, first, fixed18(40));
            assert_eq!(pair_rate(oracle, later), fixed18(2));
            assert_reference_pair(oracle, later);
        },
    );
}

#[test]
fn two_votes_cannot_replace_the_third_vote_required_for_four_validator_quorum() {
    with_oracle(|storage, oracle| {
        let pair = AddressPair::from_addresses(COEN, USDT);
        oracle.register_pair(pair).unwrap();
        publish_prior_rate(oracle, pair, fixed18(40));

        let [v1, v2, v3, v4] = register_staked_voters(&storage, [100, 100, 1, 1]);

        for voter in [v1, v2] {
            oracle
                .submit_vote(voter, &[(COEN, USDT, fixed18(50), SCALE_1E18)])
                .unwrap();
        }

        crate::tally::run_tally(oracle, 2, 24).unwrap();

        assert_prior_rate_kept(oracle, pair, fixed18(40));
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 0);
        assert_penalty_counters(
            oracle,
            &[
                (v1, Penalty::Success, 1),
                (v2, Penalty::Success, 1),
                (v3, Penalty::Abstain, 1),
                (v4, Penalty::Abstain, 1),
            ],
        );
    });
}

#[test]
fn a_validator_deactivated_after_submission_does_not_count_toward_tally_quorum() {
    with_coen840_oracle(|storage, oracle, pair| {
        publish_prior_rate(oracle, pair, coen_iso(40));

        let voters = register_four_voters(&storage);

        for voter in &voters[..2] {
            oracle
                .submit_vote(*voter, &[(COEN, usd(), coen_iso(50), COEN_ISO_SCALE)])
                .unwrap();
        }
        outbe_validatorset::contract::ValidatorSet::new(storage)
            .deactivate_validator(Address::ZERO, voters[1])
            .unwrap();

        // Three validators remain active, so two observations are required.
        // The exiting validator's stored row must not supply the missing vote.
        crate::tally::run_tally(oracle, 2, 24).unwrap();

        assert_prior_rate_kept(oracle, pair, coen_iso(40));
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 0);
        assert_penalty_counters(
            oracle,
            &[
                (voters[0], Penalty::Success, 1),
                (voters[1], Penalty::Success, 0),
                (voters[2], Penalty::Abstain, 1),
                (voters[3], Penalty::Abstain, 1),
            ],
        );
    });
}

#[test]
fn partial_pair_votes_use_independent_quorum_and_no_cross_intersection_quorum() {
    with_oracle(|storage, oracle| {
        let (_, target) = register_coen_usdt_and_usdt_eth(oracle);

        let [v1, v2, v3, v4] = register_four_voters(&storage);

        oracle
            .submit_vote(v1, &[(COEN, USDT, fixed18(50), fixed18(10))])
            .unwrap();
        oracle
            .submit_vote(
                v2,
                &[
                    (COEN, USDT, fixed18(50), fixed18(10)),
                    pair_vote(target, fixed18(2_000), fixed18(20)),
                ],
            )
            .unwrap();
        oracle
            .submit_vote(
                v3,
                &[
                    (COEN, USDT, fixed18(50), fixed18(10)),
                    pair_vote(target, fixed18(2_000), fixed18(30)),
                ],
            )
            .unwrap();
        oracle
            .submit_vote(v4, &[pair_vote(target, fixed18(2_000), fixed18(40))])
            .unwrap();

        crate::tally::run_tally(oracle, 2, 24).unwrap();

        assert_eq!(oracle.get_exchange_rate(COEN, USDT).unwrap(), fixed18(50));
        assert_eq!(pair_rate(oracle, target), fixed18(2_000));
        assert_eq!(latest_snapshot_volume(oracle, target), fixed18(25));

        assert_penalty_counters(
            oracle,
            &[
                (v1, Penalty::Miss, 1),
                (v2, Penalty::Success, 1),
                (v3, Penalty::Success, 1),
                (v4, Penalty::Miss, 1),
            ],
        );
    });
}
