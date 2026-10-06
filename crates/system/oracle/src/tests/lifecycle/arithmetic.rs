use super::*;

/// Restore a pre-admission-bounds ballot for lifecycle arithmetic regression
/// tests. Current submitVote rejects these extreme values. Admission tests cover
/// that boundary separately. Keep the validated pair/voter metadata, then seed
/// only the historical rate/volume payload that begin_block/run_tally consume.
fn seed_legacy_vote(
    oracle: &mut OracleContract,
    voter: Address,
    tuples: &[(Address, Address, U256, U256)],
) {
    let bounded: Vec<_> = tuples
        .iter()
        .map(|(base, quote, rate, volume)| {
            let pair = AddressPair::from_addresses(*base, *quote);
            let scale = crate::constants::reciprocal_scale(pair);
            (
                *base,
                *quote,
                (*rate).min(U256::from(crate::constants::MAX_VOTE_PRICE_WHOLE) * scale),
                (*volume).min(U256::from(crate::constants::MAX_VOTE_VOLUME_WHOLE) * scale),
            )
        })
        .collect();
    oracle.submit_vote(voter, &bounded).unwrap();
    for (index, (_, _, rate, volume)) in tuples.iter().enumerate() {
        let index = index as u32;
        oracle
            .vote_rate
            .get_nested(&voter)
            .write(&index, *rate)
            .unwrap();
        oracle
            .vote_volume
            .get_nested(&voter)
            .write(&index, *volume)
            .unwrap();
    }
}

#[test]
fn begin_block_skips_only_the_unrepresentable_legacy_cross_row() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let lower = AddressPair::from_addresses(COEN, USDT).to_canonical();
        let higher = AddressPair::from_addresses(USDT, ETH).to_canonical();
        // Registry-order tie-break makes the first pair the reference. Its
        // extreme historical row has an unrepresentable cross conversion.
        oracle.register_pair(higher).unwrap();
        oracle.register_pair(lower).unwrap();

        let voters = [
            Address::new([0x11; 20]),
            Address::new([0x22; 20]),
            Address::new([0x33; 20]),
            Address::new([0x44; 20]),
        ];
        for voter in voters {
            register_validator(storage.clone(), voter, native_coen(100));
        }
        seed_legacy_vote(
            &mut oracle,
            voters[0],
            &[
                (higher.address1(), higher.address2(), U256::MAX, SCALE_1E18),
                (lower.address1(), lower.address2(), U256::ONE, SCALE_1E18),
            ],
        );
        for voter in &voters[1..] {
            oracle
                .submit_vote(
                    *voter,
                    &[
                        (
                            higher.address1(),
                            higher.address2(),
                            fixed18(100),
                            SCALE_1E18,
                        ),
                        (lower.address1(), lower.address2(), fixed18(50), SCALE_1E18),
                    ],
                )
                .unwrap();
        }

        let runtime_ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(2, 24, 1), storage.clone());
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();

        assert_eq!(
            oracle
                .get_exchange_rate(higher.address1(), higher.address2())
                .unwrap(),
            fixed18(100)
        );
        assert_eq!(
            oracle
                .get_exchange_rate(lower.address1(), lower.address2())
                .unwrap(),
            fixed18(50)
        );
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 1);
        assert_eq!(oracle.penalty_miss_count.read(&voters[0]).unwrap(), 1);
        for voter in &voters[1..] {
            assert_eq!(oracle.penalty_success_count.read(voter).unwrap(), 1);
        }
        assert!(voters
            .iter()
            .all(|voter| !oracle.vote_exists.read(voter).unwrap()));
    });
}

#[test]
fn run_tally_skips_only_a_legacy_target_with_unrepresentable_final_cross_rate() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let reference =
            AddressPair::from_addresses(Address::new([0x71; 20]), Address::new([0x72; 20]));
        let target =
            AddressPair::from_addresses(Address::new([0x81; 20]), Address::new([0x82; 20]));
        oracle.register_pair(reference).unwrap();
        oracle.register_pair(target).unwrap();
        oracle
            .set_exchange_rate(Address::ZERO, target, fixed18(7), 1, 12)
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

        for (index, voter) in voters.into_iter().enumerate() {
            let reference_rate = if index < 2 { U256::ONE } else { U256::MAX };
            let mut votes = vec![(
                reference.address1(),
                reference.address2(),
                reference_rate,
                U256::ZERO,
            )];
            if index < 3 {
                votes.push((target.address1(), target.address2(), SCALE_1E18, U256::ZERO));
            }
            seed_legacy_vote(&mut oracle, voter, &votes);
        }

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        assert_ne!(
            oracle
                .get_exchange_rate(reference.address1(), reference.address2())
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(
            oracle
                .get_exchange_rate_data(target.address1(), target.address2())
                .unwrap(),
            (fixed18(7), 1, 12)
        );
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 0);
        assert!(voters
            .iter()
            .all(|voter| !oracle.vote_exists.read(voter).unwrap()));
    });
}

#[test]
fn unrepresentable_legacy_volume_keeps_the_vote_and_stays_out_of_the_median() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();

        let voters = [
            Address::new([0x11; 20]),
            Address::new([0x22; 20]),
            Address::new([0x33; 20]),
            Address::new([0x44; 20]),
        ];
        for voter in voters {
            register_validator(storage.clone(), voter, native_coen(100));
        }
        seed_legacy_vote(
            &mut oracle,
            voters[0],
            &[(COEN, usd(), coen_iso(2), U256::MAX)],
        );
        for voter in &voters[1..] {
            oracle
                .submit_vote(*voter, &[(COEN, usd(), coen_iso(2), coen_iso(1))])
                .unwrap();
        }

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        assert_eq!(oracle.get_exchange_rate(COEN, usd()).unwrap(), coen_iso(2));
        let (_, _, _, _, _, volumes) = oracle.get_all_price_snapshot_history(1).unwrap();
        assert_eq!(volumes, vec![coen_iso(1)]);
        for voter in &voters {
            assert_eq!(oracle.penalty_success_count.read(voter).unwrap(), 1);
        }
    });
}

#[test]
fn unrepresentable_legacy_median_volume_omits_snapshot_without_penalizing_any_validator() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();

        let voters = [
            Address::new([0x11; 20]),
            Address::new([0x22; 20]),
            Address::new([0x33; 20]),
            Address::new([0x44; 20]),
        ];
        for voter in voters {
            register_validator(storage.clone(), voter, native_coen(100));
        }
        for voter in &voters[..3] {
            seed_legacy_vote(
                &mut oracle,
                *voter,
                &[(COEN, usd(), coen_iso(2), U256::MAX)],
            );
        }
        oracle
            .submit_vote(voters[3], &[(COEN, usd(), coen_iso(2), coen_iso(1))])
            .unwrap();

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        assert_eq!(oracle.get_exchange_rate(COEN, usd()).unwrap(), coen_iso(2));
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 0);
        for voter in &voters {
            assert_eq!(oracle.penalty_success_count.read(voter).unwrap(), 1);
        }
    });
}

#[test]
fn exhausted_existing_aggregate_omits_snapshot_without_penalizing_valid_votes() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        let timestamp = 1_780_012_800u64 + 60 * 60;
        let day = timestamp - timestamp % 86_400;
        oracle
            .wwd_prefix_pv_sum
            .get_nested(&pair)
            .write(&day, U256::MAX)
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
        for voter in &voters[..3] {
            oracle
                .submit_vote(*voter, &[(COEN, usd(), coen_iso(2), coen_iso(1))])
                .unwrap();
        }

        crate::tally::run_tally(&mut oracle, 2, timestamp).unwrap();

        assert_eq!(oracle.get_exchange_rate(COEN, usd()).unwrap(), coen_iso(2));
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 0);
        for voter in &voters[..3] {
            assert_eq!(oracle.penalty_success_count.read(voter).unwrap(), 1);
        }
        assert_eq!(oracle.penalty_abstain_count.read(&voters[3]).unwrap(), 1);
        assert!(voters
            .iter()
            .all(|voter| !oracle.vote_exists.read(voter).unwrap()));
    });
}

#[test]
fn limited_existing_headroom_omits_snapshot_without_penalizing_any_validator() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        let timestamp = 1_780_012_800u64 + 60 * 60;
        let day = timestamp - timestamp % 86_400;
        let rate = coen_iso(2);
        let unit_volume = coen_iso(1);
        oracle
            .wwd_prefix_pv_sum
            .get_nested(&pair)
            .write(&day, U256::MAX - rate * unit_volume + U256::ONE)
            .unwrap();

        let voters = [
            Address::new([0x11; 20]),
            Address::new([0x22; 20]),
            Address::new([0x33; 20]),
            Address::new([0x44; 20]),
        ];
        for voter in voters {
            register_validator(storage.clone(), voter, native_coen(100));
            oracle
                .submit_vote(voter, &[(COEN, usd(), rate, unit_volume)])
                .unwrap();
        }

        crate::tally::run_tally(&mut oracle, 2, timestamp).unwrap();

        assert_eq!(oracle.get_exchange_rate(COEN, usd()).unwrap(), rate);
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 0);
        for voter in &voters {
            assert_eq!(oracle.penalty_success_count.read(voter).unwrap(), 1);
        }
    });
}

#[test]
fn cross_target_volume_is_the_median_of_the_cross_winners() {
    let high_rate = fixed18(100);
    // Cover both admitted market volumes and historical overflow ballots.
    for large_volume in [
        fixed18(crate::constants::MAX_VOTE_VOLUME_WHOLE),
        U256::MAX / high_rate + U256::ONE,
    ] {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);
            let reference =
                AddressPair::from_addresses(Address::new([0x71; 20]), Address::new([0x72; 20]));
            let target =
                AddressPair::from_addresses(Address::new([0x81; 20]), Address::new([0x82; 20]));
            oracle.register_pair(reference).unwrap();
            oracle.register_pair(target).unwrap();

            let voters = [
                Address::new([0x11; 20]),
                Address::new([0x22; 20]),
                Address::new([0x33; 20]),
                Address::new([0x44; 20]),
            ];
            for voter in voters {
                register_validator(storage.clone(), voter, native_coen(100));
            }

            let reference_rates = [fixed18(1), high_rate, fixed18(1), fixed18(1)];
            let target_rates = [fixed18(1), high_rate, high_rate];
            let target_volumes = [U256::ZERO, large_volume, U256::ZERO];

            for (index, voter) in voters.into_iter().enumerate() {
                let mut votes = vec![(
                    reference.address1(),
                    reference.address2(),
                    reference_rates[index],
                    U256::ZERO,
                )];
                if index < 3 {
                    votes.push((
                        target.address1(),
                        target.address2(),
                        target_rates[index],
                        target_volumes[index],
                    ));
                }
                seed_legacy_vote(&mut oracle, voter, &votes);
            }

            crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

            assert_eq!(
                oracle
                    .get_exchange_rate(target.address1(), target.address2())
                    .unwrap(),
                fixed18(1)
            );
            let (_, _, bases, quotes, _, volumes) =
                oracle.get_all_price_snapshot_history(1).unwrap();
            let target_row = bases
                .iter()
                .zip(&quotes)
                .position(|(base, quote)| (*base, *quote) == (target.address1(), target.address2()))
                .unwrap();
            assert_eq!(volumes[target_row], large_volume / U256::from(2u64));
            assert_eq!(oracle.penalty_success_count.read(&voters[0]).unwrap(), 1);
            assert_eq!(oracle.penalty_miss_count.read(&voters[1]).unwrap(), 1);
            assert_eq!(oracle.penalty_miss_count.read(&voters[2]).unwrap(), 1);
            assert_eq!(oracle.penalty_miss_count.read(&voters[3]).unwrap(), 1);
        });
    }
}
