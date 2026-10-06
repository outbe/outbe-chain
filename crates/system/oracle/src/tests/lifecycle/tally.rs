use super::*;

#[test]
fn run_tally_accepts_a_single_validator_as_the_validator_median() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();

        let validator = Address::new([0x11; 20]);
        register_validator(storage.clone(), validator, native_coen(100));
        let rate = fixed18(50);
        let volume = fixed18(1000);

        oracle
            .submit_vote(validator, &[(COEN, USDT, rate, volume)])
            .unwrap();

        // Run tally
        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        // Exchange rate should be updated to the voted rate
        let (stored_rate, block, ts) = oracle.get_exchange_rate_data(COEN, USDT).unwrap();
        assert_eq!(stored_rate, rate);
        assert_eq!(block, 2);
        assert_eq!(ts, 24);

        // Validator should get success (voted within band for all pairs)
        assert_eq!(oracle.penalty_success_count.read(&validator).unwrap(), 1);
        assert_eq!(oracle.penalty_miss_count.read(&validator).unwrap(), 0);
        assert_eq!(oracle.penalty_abstain_count.read(&validator).unwrap(), 0);

        // Votes should be cleared
        assert_eq!(oracle.voter_list.len().unwrap(), 0);

        // Snapshot should exist
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 1);
    });
}

#[test]
fn run_tally_rewards_every_voter_inside_the_reward_band() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();

        let v1 = Address::new([0x11; 20]);
        let v2 = Address::new([0x22; 20]);
        let v3 = Address::new([0x33; 20]);

        register_validator(storage.clone(), v1, native_coen(100));
        register_validator(storage.clone(), v2, native_coen(200));
        register_validator(storage.clone(), v3, native_coen(100));

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

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        // One validator contributes one observation, so the central rate is 1001.
        let rate = oracle.get_exchange_rate(COEN, USDT).unwrap();
        assert_eq!(rate, fixed18(1001));

        // Reward spread = max(std_dev, 1001 * 0.02 / 2) = max(~0.816, ~10.01) = ~10.01
        // All votes within [990.99, 1011.01] -> all win
        assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 1);
        assert_eq!(oracle.penalty_success_count.read(&v2).unwrap(), 1);
        assert_eq!(oracle.penalty_success_count.read(&v3).unwrap(), 1);
    });
}

#[test]
fn run_tally_gives_every_validator_one_median_observation_regardless_of_stake() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();

        let v1 = Address::new([0x11; 20]);
        let v2 = Address::new([0x22; 20]);
        let v3 = Address::new([0x33; 20]);
        register_validator(storage.clone(), v1, native_coen(1_000_000));
        register_validator(storage.clone(), v2, native_coen(1));
        register_validator(storage, v3, native_coen(1));

        for (voter, rate) in [(v1, 100), (v2, 200), (v3, 300)] {
            oracle
                .submit_vote(voter, &[(COEN, usd(), coen_iso(rate), COEN_ISO_SCALE)])
                .unwrap();
        }

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        assert_eq!(
            oracle.get_exchange_rate(COEN, usd()).unwrap(),
            coen_iso(200)
        );
    });
}

#[test]
fn run_tally_penalizes_a_voter_outside_the_reward_band() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();

        let v1 = Address::new([0x11; 20]);
        let v2 = Address::new([0x22; 20]);
        let v3 = Address::new([0x33; 20]);

        register_validator(storage.clone(), v1, native_coen(100));
        register_validator(storage.clone(), v2, native_coen(200));
        register_validator(storage.clone(), v3, native_coen(100));

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

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        // The validator median is 50.
        let rate = oracle.get_exchange_rate(COEN, USDT).unwrap();
        assert_eq!(rate, fixed18(50));

        // v1 and v2 should be winners, v3 (outlier at 500) should miss
        assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 1);
        assert_eq!(oracle.penalty_success_count.read(&v2).unwrap(), 1);
        assert_eq!(oracle.penalty_miss_count.read(&v3).unwrap(), 1);
    });
}

#[test]
fn run_tally_counts_a_zero_rate_submission_as_a_miss_without_poisoning_price() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();

        let valid = Address::new([0x11; 20]);
        let valid2 = Address::new([0x22; 20]);
        let invalid = Address::new([0x33; 20]);
        register_validator(storage.clone(), valid, native_coen(100));
        register_validator(storage.clone(), valid2, native_coen(100));
        register_validator(storage.clone(), invalid, native_coen(100));
        oracle
            .submit_vote(valid, &[(COEN, USDT, fixed18(50), SCALE_1E18)])
            .unwrap();
        oracle
            .submit_vote(valid2, &[(COEN, USDT, fixed18(50), SCALE_1E18)])
            .unwrap();
        oracle
            .submit_vote(invalid, &[(COEN, USDT, U256::ZERO, SCALE_1E18)])
            .unwrap();

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        assert_eq!(oracle.get_exchange_rate(COEN, USDT).unwrap(), fixed18(50));
        assert_eq!(oracle.penalty_success_count.read(&valid).unwrap(), 1);
        assert_eq!(oracle.penalty_success_count.read(&valid2).unwrap(), 1);
        assert_eq!(oracle.penalty_miss_count.read(&invalid).unwrap(), 1);
        assert_eq!(oracle.penalty_abstain_count.read(&invalid).unwrap(), 0);
    });
}

#[test]
fn run_tally_breaks_equal_reference_observation_ties_by_registry_order() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let lower = AddressPair::from_addresses(COEN, USDT).to_canonical();
        let higher = AddressPair::from_addresses(USDT, ETH).to_canonical();
        oracle.register_pair(lower).unwrap();
        oracle.register_pair(higher).unwrap();

        let voter1 = Address::new([0x11; 20]);
        let voter2 = Address::new([0x22; 20]);
        register_validator(storage.clone(), voter1, native_coen(100));
        register_validator(storage.clone(), voter2, native_coen(100));
        oracle
            .submit_vote(
                voter1,
                &[
                    (lower.address1(), lower.address2(), fixed18(50), SCALE_1E18),
                    (
                        higher.address1(),
                        higher.address2(),
                        fixed18(2_000),
                        SCALE_1E18,
                    ),
                ],
            )
            .unwrap();
        oracle
            .submit_vote(
                voter2,
                &[
                    (lower.address1(), lower.address2(), fixed18(50), SCALE_1E18),
                    (
                        higher.address1(),
                        higher.address2(),
                        fixed18(2_000),
                        SCALE_1E18,
                    ),
                ],
            )
            .unwrap();

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        assert_eq!(
            oracle
                .get_exchange_rate(lower.address1(), lower.address2())
                .unwrap(),
            fixed18(50)
        );
        assert_eq!(
            oracle
                .get_exchange_rate(higher.address1(), higher.address2())
                .unwrap(),
            fixed18(2_000)
        );
        let (_, _, bases, quotes, _, _) = oracle.get_all_price_snapshot_history(1).unwrap();
        assert_eq!((bases[0], quotes[0]), (lower.address1(), lower.address2()));
    });
}

#[test]
fn later_pair_with_more_observations_becomes_the_reference_pair() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let first = AddressPair::from_addresses(Address::new([0x71; 20]), Address::new([0x72; 20]));
        let later = AddressPair::from_addresses(Address::new([0x81; 20]), Address::new([0x82; 20]));
        oracle.register_pair(first).unwrap();
        oracle.register_pair(later).unwrap();

        let voters = [
            Address::new([0x11; 20]),
            Address::new([0x22; 20]),
            Address::new([0x33; 20]),
            Address::new([0x44; 20]),
        ];
        for voter in voters {
            register_validator(storage.clone(), voter, native_coen(100));
        }

        for (index, voter) in voters.into_iter().enumerate() {
            let mut votes = vec![(later.address1(), later.address2(), fixed18(1), SCALE_1E18)];
            if index < 3 {
                let rate = [fixed18(2), fixed18(3), fixed18(7)][index];
                votes.push((first.address1(), first.address2(), rate, SCALE_1E18));
            }
            oracle.submit_vote(voter, &votes).unwrap();
        }

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        assert_eq!(
            oracle
                .get_exchange_rate(later.address1(), later.address2())
                .unwrap(),
            fixed18(1)
        );
        assert_eq!(
            oracle
                .get_exchange_rate(first.address1(), first.address2())
                .unwrap(),
            fixed18(3) + U256::from(3u64)
        );
        let (_, _, bases, quotes, _, _) = oracle.get_all_price_snapshot_history(1).unwrap();
        assert_eq!((bases[0], quotes[0]), (later.address1(), later.address2()));
    });
}

#[test]
fn pair_below_quorum_is_not_selected_as_reference() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let first = AddressPair::from_addresses(Address::new([0x71; 20]), Address::new([0x72; 20]));
        let later = AddressPair::from_addresses(Address::new([0x81; 20]), Address::new([0x82; 20]));
        oracle.register_pair(first).unwrap();
        oracle.register_pair(later).unwrap();
        oracle
            .set_exchange_rate(Address::ZERO, first, fixed18(40), 1, 12)
            .unwrap();

        let voters = [
            Address::new([0x11; 20]),
            Address::new([0x22; 20]),
            Address::new([0x33; 20]),
            Address::new([0x44; 20]),
        ];
        for voter in voters {
            register_validator(storage.clone(), voter, native_coen(100));
        }

        oracle
            .submit_vote(
                voters[0],
                &[(first.address1(), first.address2(), fixed18(50), SCALE_1E18)],
            )
            .unwrap();
        oracle
            .submit_vote(
                voters[1],
                &[
                    (first.address1(), first.address2(), fixed18(50), SCALE_1E18),
                    (later.address1(), later.address2(), fixed18(2), SCALE_1E18),
                ],
            )
            .unwrap();
        for voter in &voters[2..] {
            oracle
                .submit_vote(
                    *voter,
                    &[(later.address1(), later.address2(), fixed18(2), SCALE_1E18)],
                )
                .unwrap();
        }

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        assert_eq!(
            oracle
                .get_exchange_rate_data(first.address1(), first.address2())
                .unwrap(),
            (fixed18(40), 1, 12)
        );
        assert_eq!(
            oracle
                .get_exchange_rate(later.address1(), later.address2())
                .unwrap(),
            fixed18(2)
        );
        let (_, _, bases, quotes, _, _) = oracle.get_all_price_snapshot_history(1).unwrap();
        assert_eq!((bases[0], quotes[0]), (later.address1(), later.address2()));
    });
}

#[test]
fn two_votes_cannot_replace_the_third_vote_required_for_four_validator_quorum() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let pair = AddressPair::from_addresses(COEN, USDT);
        oracle.register_pair(pair).unwrap();
        oracle
            .set_exchange_rate(Address::ZERO, pair, fixed18(40), 1, 12)
            .unwrap();

        let v1 = Address::new([0x11; 20]);
        let v2 = Address::new([0x22; 20]);
        let v3 = Address::new([0x33; 20]);
        let v4 = Address::new([0x44; 20]);
        register_validator(storage.clone(), v1, native_coen(100));
        register_validator(storage.clone(), v2, native_coen(100));
        register_validator(storage.clone(), v3, native_coen(1));
        register_validator(storage.clone(), v4, native_coen(1));

        for voter in [v1, v2] {
            oracle
                .submit_vote(voter, &[(COEN, USDT, fixed18(50), SCALE_1E18)])
                .unwrap();
        }

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        assert_eq!(
            oracle.get_exchange_rate_data(COEN, USDT).unwrap(),
            (fixed18(40), 1, 12)
        );
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 0);
        assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 1);
        assert_eq!(oracle.penalty_success_count.read(&v2).unwrap(), 1);
        assert_eq!(oracle.penalty_abstain_count.read(&v3).unwrap(), 1);
        assert_eq!(oracle.penalty_abstain_count.read(&v4).unwrap(), 1);
    });
}

#[test]
fn a_validator_deactivated_after_submission_does_not_count_toward_tally_quorum() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        oracle
            .set_exchange_rate(Address::ZERO, pair, coen_iso(40), 1, 12)
            .unwrap();

        let voters = [
            Address::new([0x11; 20]),
            Address::new([0x22; 20]),
            Address::new([0x33; 20]),
            Address::new([0x44; 20]),
        ];
        for voter in voters {
            register_validator(storage.clone(), voter, native_coen(100));
        }

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
        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        assert_eq!(
            oracle.get_exchange_rate_data(COEN, usd()).unwrap(),
            (coen_iso(40), 1, 12)
        );
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 0);
        assert_eq!(oracle.penalty_success_count.read(&voters[0]).unwrap(), 1);
        assert_eq!(oracle.penalty_success_count.read(&voters[1]).unwrap(), 0);
        assert_eq!(oracle.penalty_abstain_count.read(&voters[2]).unwrap(), 1);
        assert_eq!(oracle.penalty_abstain_count.read(&voters[3]).unwrap(), 1);
    });
}

#[test]
fn partial_pair_votes_use_independent_quorum_and_no_cross_intersection_quorum() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let reference = AddressPair::from_addresses(COEN, USDT);
        let target = AddressPair::from_addresses(USDT, ETH).to_canonical();
        oracle.register_pair(reference).unwrap();
        oracle.register_pair(target).unwrap();

        let v1 = Address::new([0x11; 20]);
        let v2 = Address::new([0x22; 20]);
        let v3 = Address::new([0x33; 20]);
        let v4 = Address::new([0x44; 20]);
        for voter in [v1, v2, v3, v4] {
            register_validator(storage.clone(), voter, native_coen(100));
        }

        oracle
            .submit_vote(v1, &[(COEN, USDT, fixed18(50), fixed18(10))])
            .unwrap();
        oracle
            .submit_vote(
                v2,
                &[
                    (COEN, USDT, fixed18(50), fixed18(10)),
                    (
                        target.address1(),
                        target.address2(),
                        fixed18(2_000),
                        fixed18(20),
                    ),
                ],
            )
            .unwrap();
        oracle
            .submit_vote(
                v3,
                &[
                    (COEN, USDT, fixed18(50), fixed18(10)),
                    (
                        target.address1(),
                        target.address2(),
                        fixed18(2_000),
                        fixed18(30),
                    ),
                ],
            )
            .unwrap();
        oracle
            .submit_vote(
                v4,
                &[(
                    target.address1(),
                    target.address2(),
                    fixed18(2_000),
                    fixed18(40),
                )],
            )
            .unwrap();

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        assert_eq!(oracle.get_exchange_rate(COEN, USDT).unwrap(), fixed18(50));
        assert_eq!(
            oracle
                .get_exchange_rate(target.address1(), target.address2())
                .unwrap(),
            fixed18(2_000)
        );
        let (_, _, bases, quotes, _, volumes) = oracle.get_all_price_snapshot_history(1).unwrap();
        let target_row = bases
            .iter()
            .zip(&quotes)
            .position(|(base, quote)| (*base, *quote) == (target.address1(), target.address2()))
            .unwrap();
        assert_eq!(volumes[target_row], fixed18(25));

        assert_eq!(oracle.penalty_miss_count.read(&v1).unwrap(), 1);
        assert_eq!(oracle.penalty_success_count.read(&v2).unwrap(), 1);
        assert_eq!(oracle.penalty_success_count.read(&v3).unwrap(), 1);
        assert_eq!(oracle.penalty_miss_count.read(&v4).unwrap(), 1);
    });
}
