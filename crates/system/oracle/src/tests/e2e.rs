//! End-to-end tests: genesis round-trips, precompile dispatch, VWAP flows.

use alloy_primitives::{Address, U256};
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::Units;

use crate::schema::{OracleContract, SCALE_1E18};

use super::common::*;

// -----------------------------------------------------------------------
// Genesis config tests
// -----------------------------------------------------------------------

#[test]
fn init_from_genesis_default_config_matches_the_hardcoded_state() {
    with_storage(|storage| {
        // Reference: manually-written init (mirrors the old executor code).
        let mut expected = OracleContract::new(storage.clone());
        expected.config_vote_period.write(2).unwrap();
        expected
            .config_reward_band
            .write(U256::from(20_000_000_000_000_000u128))
            .unwrap();
        expected.config_slash_window.write(96).unwrap();
        expected
            .config_min_valid_per_window
            .write(U256::from(50_000_000_000_000_000u128))
            .unwrap();
        expected.config_slash_fraction.write(U256::ZERO).unwrap();
        expected.config_lookback_duration.write(86400).unwrap();
        expected.config_enabled.write(true).unwrap();
        expected.config_is_initialized.write(true).unwrap();
        expected
            .register_pair(AddressPair::new_coen_to(840))
            .unwrap();

        // Snapshot expected state.
        let exp_vote_period = expected.config_vote_period.read().unwrap();
        let exp_reward_band = expected.config_reward_band.read().unwrap();
        let exp_slash_window = expected.config_slash_window.read().unwrap();
        let exp_min_valid = expected.config_min_valid_per_window.read().unwrap();
        let exp_slash_fraction = expected.config_slash_fraction.read().unwrap();
        let exp_lookback = expected.config_lookback_duration.read().unwrap();
        let exp_enabled = expected.config_enabled.read().unwrap();
        let exp_initialized = expected.config_is_initialized.read().unwrap();
        let exp_pair_count = expected.pair_count.read().unwrap();
        let exp_pair_id = expected.pair_index_of(pair_key(COEN, usd())).unwrap();
        let exp_vote_target = expected.is_vote_target(COEN, usd()).unwrap();

        // Now init through the genesis config path (uses same storage).
        // Since pairs are already registered we need a fresh storage context.
        // We re-enter with a new provider to start clean.

        let mut storage2 = HashMapStorageProvider::new(2);
        StorageHandle::enter(&mut storage2, |storage| {
            let mut oracle = OracleContract::new(storage.clone());
            crate::genesis::init_from_genesis(
                &mut oracle,
                &crate::genesis::OracleGenesisConfig::default_config(),
            )
            .unwrap();

            assert_eq!(oracle.config_vote_period.read().unwrap(), exp_vote_period);
            assert_eq!(oracle.config_reward_band.read().unwrap(), exp_reward_band);
            assert_eq!(oracle.config_slash_window.read().unwrap(), exp_slash_window);
            assert_eq!(
                oracle.config_min_valid_per_window.read().unwrap(),
                exp_min_valid
            );
            assert_eq!(
                oracle.config_slash_fraction.read().unwrap(),
                exp_slash_fraction
            );
            assert_eq!(
                oracle.config_lookback_duration.read().unwrap(),
                exp_lookback
            );
            assert_eq!(oracle.config_enabled.read().unwrap(), exp_enabled);
            assert_eq!(
                oracle.config_is_initialized.read().unwrap(),
                exp_initialized
            );
            assert_eq!(oracle.pair_count.read().unwrap(), exp_pair_count);
            assert_eq!(
                oracle.pair_index_of(pair_key(COEN, usd())).unwrap(),
                exp_pair_id
            );
            assert_eq!(oracle.is_vote_target(COEN, usd()).unwrap(), exp_vote_target);
        });
    });
}

#[test]
fn init_from_genesis_imports_every_custom_config_collection() {
    with_storage(|storage| {
        register_validator(
            storage.clone(),
            Address::new([0x11; 20]),
            U256::in_units(100u64),
        );
        register_validator(
            storage.clone(),
            Address::new([0x22; 20]),
            U256::in_units(100u64),
        );
        let config = crate::genesis::OracleGenesisConfig {
            vote_period: 5,
            reward_band: U256::from(10_000_000_000_000_000u128), // 0.01
            slash_window: 200,
            min_valid_per_window: U256::from(100_000_000_000_000_000u128), // 0.10
            slash_fraction: U256::from(1_000_000_000_000_000u128),         // 0.001
            lookback_duration: 172_800,                                    // 2 days
            pairs: vec![(COEN, usd()), (usd(), ETH), (BTC, USDT)],
            initial_rates: vec![(COEN, usd(), coen840(1)), (usd(), ETH, fixed18(2000))],
            feeder_delegations: vec![
                (Address::new([0x11; 20]), Address::new([0xAAu8; 20])),
                (Address::new([0x22; 20]), Address::new([0xBBu8; 20])),
            ],
            reference_currencies: vec![ref_cur(840)],
            penalty_counters: vec![],
            aggregate_votes: vec![],
            snapshots: vec![],
            scurve_entries: vec![],
            protected_validators: vec![],
        };

        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();

        // Verify config slots.
        assert_eq!(oracle.config_vote_period.read().unwrap(), 5);
        assert_eq!(
            oracle.config_reward_band.read().unwrap(),
            U256::from(10_000_000_000_000_000u128)
        );
        assert_eq!(oracle.config_slash_window.read().unwrap(), 200);
        assert_eq!(
            oracle.config_min_valid_per_window.read().unwrap(),
            U256::from(100_000_000_000_000_000u128)
        );
        assert_eq!(
            oracle.config_slash_fraction.read().unwrap(),
            U256::from(1_000_000_000_000_000u128)
        );
        assert_eq!(oracle.config_lookback_duration.read().unwrap(), 172_800);
        assert!(oracle.config_enabled.read().unwrap());
        assert!(oracle.config_is_initialized.read().unwrap());

        // Verify all three pairs registered.
        assert_eq!(oracle.pair_count.read().unwrap(), 3);
        assert_eq!(oracle.pair_index_of(pair_key(COEN, usd())).unwrap(), 1);
        assert_eq!(oracle.pair_index_of(pair_key(usd(), ETH)).unwrap(), 2);
        assert_eq!(oracle.pair_index_of(pair_key(BTC, USDT)).unwrap(), 3);
        assert!(oracle.is_vote_target(COEN, usd()).unwrap());
        assert!(oracle.is_vote_target(usd(), ETH).unwrap());
        assert!(oracle.is_vote_target(BTC, USDT).unwrap());

        // Verify initial rates (only first two pairs have rates).
        let (rate_coen, blk, ts) = oracle.get_exchange_rate_data(COEN, usd()).unwrap();
        assert_eq!(rate_coen, coen840(1));
        assert_eq!(blk, 0);
        assert_eq!(ts, 0);

        let rate_eth = oracle.get_exchange_rate(usd(), ETH).unwrap();
        assert_eq!(rate_eth, fixed18(2000));

        // BTC/USDT has no initial rate set → zero.
        let rate_btc = oracle.get_exchange_rate(BTC, USDT).unwrap();
        assert_eq!(rate_btc, U256::ZERO);

        // Verify feeder delegations.
        let v1 = Address::new([0x11; 20]);
        let v2 = Address::new([0x22; 20]);
        assert_eq!(oracle.get_feeder(&v1).unwrap(), Address::new([0xAAu8; 20]));
        assert_eq!(oracle.get_feeder(&v2).unwrap(), Address::new([0xBBu8; 20]));

        // An ISO resolves through the pair registry alone.
        assert_eq!(
            crate::api::require_coen_pair(storage.clone(), 840)
                .unwrap()
                .0,
            pair_key(COEN, usd())
        );
        assert!(crate::api::require_coen_pair(storage.clone(), 978).is_err());
    });
}

#[test]
fn init_from_genesis_is_idempotent_on_replay() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let config = crate::genesis::OracleGenesisConfig::default_config();

        // First init succeeds
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();
        assert!(oracle.config_is_initialized.read().unwrap());
        assert_eq!(oracle.pair_count.read().unwrap(), 1);

        // Second init is a no-op (idempotent — no error)
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();
        assert_eq!(oracle.pair_count.read().unwrap(), 1); // still 1, not 2
    });
}
#[test]
fn precompile_dispatch_returns_the_configured_params() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);

        // Encode getParams() call
        use crate::precompile::IOracle;
        use alloy_sol_types::SolCall;
        let calldata = IOracle::getParamsCall {}.abi_encode();

        // Dispatch through precompile
        let result =
            crate::precompile::dispatch(storage.clone(), &calldata, Address::ZERO, U256::ZERO)
                .unwrap();

        // Decode result
        let decoded = IOracle::getParamsCall::abi_decode_returns(&result).unwrap();
        assert_eq!(decoded.votePeriod, 2);
        assert!(decoded.enabled);
    });
}

#[test]
fn precompile_dispatch_round_trips_an_exchange_rate() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        let expected_rate = coen840(123);
        oracle
            .set_exchange_rate(
                Address::ZERO,
                AddressPair::from_addresses(COEN, usd()),
                expected_rate,
                42,
                86_400,
            )
            .unwrap();

        use crate::precompile::IOracle;
        use alloy_sol_types::SolCall;

        let calldata = IOracle::getExchangeRateCall {
            base: COEN,
            quote: usd(),
        }
        .abi_encode();
        let result =
            crate::precompile::dispatch(storage.clone(), &calldata, Address::ZERO, U256::ZERO)
                .unwrap();
        let rate = IOracle::getExchangeRateCall::abi_decode_returns(&result).unwrap();
        assert_eq!(rate, expected_rate);

        let calldata = IOracle::getExchangeRateDataCall {
            base: COEN,
            quote: usd(),
        }
        .abi_encode();
        let result =
            crate::precompile::dispatch(storage.clone(), &calldata, Address::ZERO, U256::ZERO)
                .unwrap();
        let decoded = IOracle::getExchangeRateDataCall::abi_decode_returns(&result).unwrap();

        assert_eq!(decoded.rate, expected_rate);
        assert_eq!(decoded.lastBlock, 42);
        assert_eq!(decoded.lastTimestamp, 86_400);

        // The same market quoted backwards prices at the reciprocal rather than
        // reverting, and keeps the stored observation's block and timestamp.
        let calldata = IOracle::getExchangeRateDataCall {
            base: usd(),
            quote: COEN,
        }
        .abi_encode();
        let result =
            crate::precompile::dispatch(storage.clone(), &calldata, Address::ZERO, U256::ZERO)
                .unwrap();
        let flipped = IOracle::getExchangeRateDataCall::abi_decode_returns(&result).unwrap();

        assert_eq!(flipped.rate, COEN840_SCALE * COEN840_SCALE / expected_rate);
        assert_eq!(flipped.lastBlock, 42);
        assert_eq!(flipped.lastTimestamp, 86_400);

        // COEN sorts first, so the ISO-code shorthand is always the canonical
        // direction and agrees with the explicit two-address form.
        let calldata = IOracle::getCoenExchangeRateForCall { isoCode: 840 }.abi_encode();
        let result =
            crate::precompile::dispatch(storage.clone(), &calldata, Address::ZERO, U256::ZERO)
                .unwrap();
        assert_eq!(
            IOracle::getCoenExchangeRateForCall::abi_decode_returns(&result).unwrap(),
            expected_rate
        );
    });
}
#[test]
fn precompile_dispatch_round_trips_the_whole_query_surface() {
    with_storage_at(3_000, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);

        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        oracle
            .register_pair(AddressPair::from_addresses(usd(), ETH))
            .unwrap();
        oracle
            .write_snapshot(
                1_000,
                &[
                    (pair_key(COEN, usd()), coen840(100), coen840(1)),
                    (pair_key(usd(), ETH), fixed18(2_000), SCALE_1E18),
                ],
            )
            .unwrap();
        oracle
            .write_snapshot(
                2_000,
                &[
                    (pair_key(COEN, usd()), coen840(120), coen840(1)),
                    (pair_key(usd(), ETH), fixed18(2_200), SCALE_1E18),
                ],
            )
            .unwrap();
        oracle
            .write_snapshot(
                3_000,
                &[
                    (pair_key(COEN, usd()), coen840(140), coen840(1)),
                    (pair_key(usd(), ETH), fixed18(2_400), SCALE_1E18),
                ],
            )
            .unwrap();

        crate::scurve::store_scurve_entry(&mut oracle, pair_key(COEN, usd()), 0, coen840(160))
            .unwrap();

        use crate::precompile::IOracle;
        use alloy_sol_types::SolCall;

        let history = IOracle::getAllPriceSnapshotHistoryCall { count: 2 }.abi_encode();
        let decoded = IOracle::getAllPriceSnapshotHistoryCall::abi_decode_returns(
            &crate::precompile::dispatch(storage.clone(), &history, Address::ZERO, U256::ZERO)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded.snapshotIds, vec![2, 2, 1, 1]);
        assert_eq!(decoded.bases, vec![COEN, usd(), COEN, usd()]);
        assert_eq!(decoded.quotes, vec![usd(), ETH, usd(), ETH]);

        let twaps = IOracle::getTwapsCall { lookback: 2_500 }.abi_encode();
        let decoded = IOracle::getTwapsCall::abi_decode_returns(
            &crate::precompile::dispatch(storage.clone(), &twaps, Address::ZERO, U256::ZERO)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded.bases, vec![COEN, usd()]);
        assert_eq!(decoded.quotes, vec![usd(), ETH]);
        assert_eq!(decoded.lookbackSeconds, vec![2_500, 2_500]);
        assert_eq!(decoded.twaps.len(), 2);

        let wwd = IOracle::getWorldwideDayVwapCall {
            startTime: 1_000,
            endTime: 3_000,
        }
        .abi_encode();
        let decoded = IOracle::getWorldwideDayVwapCall::abi_decode_returns(
            &crate::precompile::dispatch(storage.clone(), &wwd, Address::ZERO, U256::ZERO).unwrap(),
        )
        .unwrap();
        assert_eq!(decoded.bases, vec![COEN, usd()]);
        assert_eq!(decoded.quotes, vec![usd(), ETH]);
        assert_eq!(decoded.lookbackSeconds, vec![2_000, 2_000]);

        let scurve_values = IOracle::getScurveValuesCall {
            base: COEN,
            quote: usd(),
            timestamp: 3_000,
        }
        .abi_encode();
        let decoded = IOracle::getScurveValuesCall::abi_decode_returns(
            &crate::precompile::dispatch(
                storage.clone(),
                &scurve_values,
                Address::ZERO,
                U256::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded.targetDay, 0);
        assert_eq!(decoded.peakDays, vec![0]);
        assert_eq!(decoded.values, vec![coen840(160)]);

        let scurve_data = IOracle::getAllScurveDataForPairCall {
            base: COEN,
            quote: usd(),
        }
        .abi_encode();
        let decoded = IOracle::getAllScurveDataForPairCall::abi_decode_returns(
            &crate::precompile::dispatch(storage.clone(), &scurve_data, Address::ZERO, U256::ZERO)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded.peakDays, vec![0]);
        assert_eq!(decoded.peakPrices, vec![coen840(160)]);

        let nominal_components = IOracle::getNominalPriceComponentsCall {
            base: COEN,
            quote: usd(),
            timestamp: 3_000,
        }
        .abi_encode();
        let decoded = IOracle::getNominalPriceComponentsCall::abi_decode_returns(
            &crate::precompile::dispatch(
                storage.clone(),
                &nominal_components,
                Address::ZERO,
                U256::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded.nominalPrice, coen840(160));
        assert_eq!(decoded.maxScurve, coen840(160));
        assert_eq!(decoded.source, "scurve");

        let nominal = IOracle::getNominalPriceCall {
            base: COEN,
            quote: usd(),
            timestamp: 3_000,
        }
        .abi_encode();
        let decoded = IOracle::getNominalPriceCall::abi_decode_returns(
            &crate::precompile::dispatch(storage.clone(), &nominal, Address::ZERO, U256::ZERO)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded, coen840(160));
    });
}

#[test]
fn ioracle_selectors_are_unique() {
    use crate::precompile::IOracle;
    use alloy_sol_types::SolInterface;
    use std::collections::HashSet;

    const EXPECTED_IORACLE_FUNCTIONS: usize = 36;

    let selectors: Vec<[u8; 4]> = IOracle::IOracleCalls::selectors().collect();
    assert_eq!(
        selectors.len(),
        IOracle::IOracleCalls::COUNT,
        "selector iterator must cover every generated IOracle call variant"
    );
    assert_eq!(
        IOracle::IOracleCalls::COUNT,
        EXPECTED_IORACLE_FUNCTIONS,
        "IOracle function count changed; update selector collision coverage"
    );

    let unique: HashSet<[u8; 4]> = selectors.iter().copied().collect();
    assert_eq!(
        unique.len(),
        selectors.len(),
        "selector collision detected among {} IOracle functions",
        selectors.len()
    );
}
// -----------------------------------------------------------------------
// Genesis Import: Snapshots, Penalties, S-curves
// -----------------------------------------------------------------------

#[test]
fn genesis_imports_penalty_counters() {
    with_storage(|storage| {
        let v1 = Address::new([0x11; 20]);
        let v2 = Address::new([0x22; 20]);
        let config = crate::genesis::OracleGenesisConfig {
            penalty_counters: vec![(v1, 10, 2, 3), (v2, 5, 0, 1)],
            ..crate::genesis::OracleGenesisConfig::default_config()
        };

        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();

        assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 10);
        assert_eq!(oracle.penalty_abstain_count.read(&v1).unwrap(), 2);
        assert_eq!(oracle.penalty_miss_count.read(&v1).unwrap(), 3);
        assert_eq!(oracle.penalty_success_count.read(&v2).unwrap(), 5);
        assert_eq!(oracle.penalty_miss_count.read(&v2).unwrap(), 1);
    });
}

#[test]
fn genesis_imports_price_snapshots() {
    with_storage(|storage| {
        let config = crate::genesis::OracleGenesisConfig {
            snapshots: vec![
                crate::genesis::GenesisSnapshot {
                    timestamp: 1000,
                    entries: vec![(COEN, usd(), coen840(100), coen840(1))],
                },
                crate::genesis::GenesisSnapshot {
                    timestamp: 2000,
                    entries: vec![(COEN, usd(), coen840(200), coen840(1))],
                },
            ],
            ..crate::genesis::OracleGenesisConfig::default_config()
        };

        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();

        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 2);
        assert_eq!(oracle.snapshot_timestamp.read(&0u64).unwrap(), 1000);
        assert_eq!(oracle.snapshot_timestamp.read(&1u64).unwrap(), 2000);
    });
}

#[test]
fn genesis_imports_scurve_entries() {
    with_storage(|storage| {
        let config = crate::genesis::OracleGenesisConfig {
            scurve_entries: vec![
                crate::genesis::GenesisScurveEntry {
                    base: COEN,
                    quote: usd(),
                    peak_day: 86400,
                    peak_price: coen840(500),
                },
                crate::genesis::GenesisScurveEntry {
                    base: COEN,
                    quote: usd(),
                    peak_day: 86400 * 10,
                    peak_price: coen840(600),
                },
            ],
            ..crate::genesis::OracleGenesisConfig::default_config()
        };

        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();

        assert_eq!(oracle.scurve_count.read().unwrap(), 2);
        assert_eq!(oracle.pair_at(1).unwrap(), pair_key(COEN, usd()));
        assert_eq!(oracle.scurve_peak_day.read(&0u32).unwrap(), 86400);
        assert_eq!(oracle.scurve_peak_price.read(&0u32).unwrap(), coen840(500));
    });
}

#[test]
fn genesis_imports_protected_validators() {
    with_storage(|storage| {
        let v1 = Address::new([0x11; 20]);
        let v2 = Address::new([0x22; 20]);
        let config = crate::genesis::OracleGenesisConfig {
            protected_validators: vec![v1, v2],
            ..crate::genesis::OracleGenesisConfig::default_config()
        };

        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();

        assert!(oracle.config_allow_protected.read().unwrap());
        assert!(oracle.protected_validator.read(&v1).unwrap());
        assert!(oracle.protected_validator.read(&v2).unwrap());
        assert!(!oracle
            .protected_validator
            .read(&Address::new([0x33; 20]))
            .unwrap());
    });
}

#[test]
fn genesis_imports_pending_aggregate_votes() {
    with_storage(|storage| {
        let validator = Address::new([0x11; 20]);
        let rate1 = coen840(42);
        let rate2 = fixed18(2100);
        let volume1 = coen840(100);
        let volume2 = fixed18(200);
        let config = crate::genesis::OracleGenesisConfig {
            pairs: vec![(COEN, usd()), (usd(), ETH)],
            aggregate_votes: vec![crate::genesis::GenesisAggregateVote {
                validator,
                entries: vec![(COEN, usd(), rate1, volume1), (usd(), ETH, rate2, volume2)],
            }],
            ..crate::genesis::OracleGenesisConfig::default_config()
        };

        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();

        assert!(oracle.vote_exists.read(&validator).unwrap());
        assert_eq!(oracle.vote_tuple_count.read(&validator).unwrap(), 2);
        assert_eq!(oracle.voter_list.len().unwrap(), 1);
        assert_eq!(oracle.voter_list.get(0).unwrap(), Some(validator));

        let (exists, bases, quotes, rates, volumes) =
            oracle.get_aggregate_vote(&validator).unwrap();
        assert!(exists);
        assert_eq!(bases, vec![COEN, usd()]);
        assert_eq!(quotes, vec![usd(), ETH]);
        assert_eq!(rates, vec![rate1, rate2]);
        assert_eq!(volumes, vec![volume1, volume2]);
    });
}

#[test]
fn genesis_rejects_a_duplicate_aggregate_vote_pair() {
    with_storage(|storage| {
        let validator = Address::new([0x11; 20]);
        let config = crate::genesis::OracleGenesisConfig {
            aggregate_votes: vec![crate::genesis::GenesisAggregateVote {
                validator,
                entries: vec![
                    (COEN, usd(), coen840(42), coen840(1)),
                    (COEN, usd(), coen840(43), coen840(1)),
                ],
            }],
            ..crate::genesis::OracleGenesisConfig::default_config()
        };

        let mut oracle = OracleContract::new(storage.clone());
        assert!(crate::genesis::init_from_genesis(&mut oracle, &config).is_err());
        assert!(!oracle.config_is_initialized.read().unwrap());
        assert!(!oracle.config_enabled.read().unwrap());
        assert!(!oracle.vote_exists.read(&validator).unwrap());
        assert_eq!(oracle.voter_list.len().unwrap(), 0);
    });
}

#[test]
fn export_genesis_round_trips_the_full_oracle_state() {
    let v1 = Address::new([0x11; 20]);
    let v2 = Address::new([0x22; 20]);
    let config = crate::genesis::OracleGenesisConfig {
        pairs: vec![(COEN, usd()), (usd(), ETH), (BTC, USDT)],
        initial_rates: vec![(COEN, usd(), coen840(1)), (usd(), ETH, fixed18(2000))],
        feeder_delegations: vec![(v1, Address::new([0xAAu8; 20]))],
        aggregate_votes: vec![
            crate::genesis::GenesisAggregateVote {
                validator: v1,
                entries: vec![
                    (COEN, usd(), coen840(42), coen840(1)),
                    (usd(), ETH, fixed18(2100), SCALE_1E18),
                ],
            },
            crate::genesis::GenesisAggregateVote {
                validator: v2,
                entries: vec![(COEN, usd(), coen840(41), coen840(1))],
            },
        ],
        reference_currencies: vec![ref_cur(840), ref_cur(978)],
        penalty_counters: vec![(v1, 7, 2, 1), (v2, 3, 0, 4)],
        snapshots: vec![crate::genesis::GenesisSnapshot {
            timestamp: 5000,
            entries: vec![
                (COEN, usd(), coen840(42), coen840(1)),
                (usd(), ETH, fixed18(2100), SCALE_1E18),
            ],
        }],
        scurve_entries: vec![crate::genesis::GenesisScurveEntry {
            base: COEN,
            quote: usd(),
            peak_day: 86400,
            peak_price: coen840(100),
        }],
        protected_validators: vec![v1],
        vote_period: 2,
        reward_band: U256::from(20_000_000_000_000_000u128),
        slash_window: 96,
        min_valid_per_window: U256::from(50_000_000_000_000_000u128),
        slash_fraction: U256::ZERO,
        lookback_duration: 86400,
    };

    let exported = {
        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |storage| {
            register_validator(storage.clone(), v1, U256::in_units(100u64));
            let mut oracle = OracleContract::new(storage.clone());
            crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();
            crate::genesis::export_genesis(&oracle, &[v1, v2]).unwrap()
        })
    };

    assert_eq!(exported.vote_period, 2);
    assert_eq!(exported.slash_window, 96);
    assert_eq!(exported.pairs, config.pairs);
    assert_eq!(exported.initial_rates, config.initial_rates);
    assert_eq!(exported.feeder_delegations, config.feeder_delegations);
    assert_eq!(exported.aggregate_votes.len(), 2);
    assert_eq!(exported.aggregate_votes[0].validator, v1);
    assert_eq!(
        exported.aggregate_votes[0].entries,
        config.aggregate_votes[0].entries
    );
    assert_eq!(exported.aggregate_votes[1].validator, v2);
    assert_eq!(
        exported.aggregate_votes[1].entries,
        config.aggregate_votes[1].entries
    );
    assert_eq!(exported.penalty_counters, config.penalty_counters);
    assert_eq!(exported.snapshots.len(), 1);
    assert_eq!(exported.snapshots[0].timestamp, 5000);
    assert_eq!(exported.snapshots[0].entries.len(), 2);
    assert_eq!(exported.scurve_entries.len(), 1);
    assert_eq!(exported.scurve_entries[0].base, COEN);
    assert_eq!(exported.scurve_entries[0].quote, usd());
    assert_eq!(exported.scurve_entries[0].peak_day, 86400);
    assert_eq!(exported.protected_validators, vec![v1]);

    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        register_validator(storage.clone(), v1, U256::in_units(100u64));
        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(&mut oracle, &exported).unwrap();

        assert_eq!(oracle.pair_count.read().unwrap(), 3);
        assert_eq!(oracle.pair_index_of(pair_key(COEN, usd())).unwrap(), 1);
        assert_eq!(oracle.pair_index_of(pair_key(usd(), ETH)).unwrap(), 2);
        assert_eq!(oracle.pair_index_of(pair_key(BTC, USDT)).unwrap(), 3);
        assert_eq!(oracle.get_exchange_rate(COEN, usd()).unwrap(), coen840(1));
        assert_eq!(oracle.get_exchange_rate(usd(), ETH).unwrap(), fixed18(2000));
        assert_eq!(oracle.get_feeder(&v1).unwrap(), Address::new([0xAAu8; 20]));
        assert_eq!(oracle.get_aggregate_vote(&v1).unwrap().1, vec![COEN, usd()]);
        assert_eq!(oracle.get_aggregate_vote(&v2).unwrap().1, vec![COEN]);
        assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 7);
        assert_eq!(oracle.penalty_miss_count.read(&v2).unwrap(), 4);
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 1);
        assert_eq!(oracle.scurve_count.read().unwrap(), 1);
        assert!(oracle.protected_validator.read(&v1).unwrap());
    });
}

#[test]
fn export_genesis_fails_without_ordinal_reverse_lookup_columns() {
    with_storage(|storage| {
        // `pair_count` claims an ordinal whose reverse-lookup columns were
        // never written, so the rebuilt key does not round-trip back to it.
        let oracle = OracleContract::new(storage.clone());
        oracle.pair_count.write(1).unwrap();
        oracle
            .pair_to_index
            .write(&pair_key(COEN, usd()), 1)
            .unwrap();

        assert!(crate::genesis::export_genesis(&oracle, &[]).is_err());
    });
}

#[test]
fn export_genesis_omits_a_zero_initial_rate() {
    with_storage(|storage| {
        let config = crate::genesis::OracleGenesisConfig {
            pairs: vec![(COEN, usd()), (BTC, USDT)],
            ..crate::genesis::OracleGenesisConfig::default_config()
        };
        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();

        let exported = crate::genesis::export_genesis(&oracle, &[]).unwrap();
        assert_eq!(exported.pairs, config.pairs);
        assert!(exported.initial_rates.is_empty());
    });
}
#[test]
fn store_worldwide_day_vwap_snapshot_round_trips_every_pair() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        oracle
            .register_pair(AddressPair::from_addresses(usd(), ETH))
            .unwrap();

        oracle
            .write_snapshot(
                1_500,
                &[
                    (pair_key(COEN, usd()), coen840(110), coen840(1)),
                    (pair_key(usd(), ETH), fixed18(2_200), SCALE_1E18),
                ],
            )
            .unwrap();

        oracle
            .store_worldwide_day_vwap_snapshot(20260302u32.into(), 1_000, 3_000)
            .unwrap();

        let (start_time, end_time, bases, quotes, vwaps, lookbacks) = oracle
            .get_worldwide_day_vwap_snapshot(20260302u32.into())
            .unwrap();
        assert_eq!(start_time, 1_000);
        assert_eq!(end_time, 3_000);
        assert_eq!(bases, vec![COEN, usd()]);
        assert_eq!(quotes, vec![usd(), ETH]);
        assert_eq!(vwaps, vec![coen840(110), fixed18(2_200)]);
        assert_eq!(lookbacks, vec![2_000, 2_000]);
        assert_eq!(
            oracle
                .get_worldwide_day_vwap_for_pair(
                    20260302u32.into(),
                    oracle.pair_index_of(pair_key(COEN, usd())).unwrap()
                )
                .unwrap(),
            Some(coen840(110))
        );
        // A registered pair with no data that day reads as absent, not as some
        // neighbouring entry's value.
        oracle.register_pair(pair_key(BTC, USDT)).unwrap();
        assert_eq!(
            oracle
                .get_worldwide_day_vwap_for_pair(
                    20260302u32.into(),
                    oracle.pair_index_of(pair_key(BTC, USDT)).unwrap()
                )
                .unwrap(),
            None
        );

        use crate::precompile::IOracle;
        use alloy_sol_types::SolCall;

        let call = IOracle::getWorldwideDayVwapSnapshotCall {
            worldwideDay: 20260302,
        }
        .abi_encode();
        let decoded = IOracle::getWorldwideDayVwapSnapshotCall::abi_decode_returns(
            &crate::precompile::dispatch(storage.clone(), &call, Address::ZERO, U256::ZERO)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded.startTime, 1_000);
        assert_eq!(decoded.endTime, 3_000);
        assert_eq!(decoded.bases, vec![COEN, usd()]);
        assert_eq!(decoded.quotes, vec![usd(), ETH]);
        assert_eq!(decoded.vwaps, vec![coen840(110), fixed18(2_200)]);
    });
}

#[test]
fn day_type_pair_vwap_reports_missing_data_without_reverting() {
    with_storage(|storage| {
        let wwd = outbe_common::WorldwideDay::new(20260302u32);

        // Pair not registered yet → typed None, not an error.
        assert_eq!(
            crate::api::day_type_pair_vwap(storage.clone(), wwd).unwrap(),
            None
        );

        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        oracle
            .write_snapshot(1_500, &[(pair_key(COEN, usd()), coen840(110), coen840(1))])
            .unwrap();

        // No window data → store is a deterministic no-op returning false,
        // not a "no VWAP data" revert leaking to the caller.
        assert!(
            !crate::api::store_worldwide_day_vwap_snapshot(storage.clone(), wwd, 100, 200).unwrap()
        );
        assert_eq!(
            crate::api::day_type_pair_vwap(storage.clone(), wwd).unwrap(),
            None,
            "no snapshot written → None"
        );

        // Window with data → store writes (true) and the COEN VWAP resolves.
        assert!(
            crate::api::store_worldwide_day_vwap_snapshot(storage.clone(), wwd, 1_000, 3_000)
                .unwrap()
        );
        assert_eq!(
            crate::api::day_type_pair_vwap(storage.clone(), wwd).unwrap(),
            Some(coen840(110))
        );
    });
}

#[test]
fn finalize_utc_day_vwap_persists_every_vote_target_pair() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        oracle
            .register_pair(AddressPair::from_addresses(usd(), ETH))
            .unwrap();

        let utc_day = 20260624u32;
        let day_start = outbe_primitives::time::date_key_to_utc_timestamp(utc_day);

        // Two COEN samples within the day → volume-weighted:
        // (100*2 + 200*1) / (2 + 1) = 133.333333 at the six-decimal boundary.
        oracle
            .write_snapshot(
                day_start + 100,
                &[(pair_key(COEN, usd()), coen840(100), coen840(2))],
            )
            .unwrap();
        oracle
            .write_snapshot(
                day_start + 200,
                &[(pair_key(COEN, usd()), coen840(200), coen840(1))],
            )
            .unwrap();
        // ETH single sample → VWAP == rate.
        oracle
            .write_snapshot(
                day_start + 300,
                &[(pair_key(usd(), ETH), fixed18(2_200), SCALE_1E18)],
            )
            .unwrap();

        oracle.finalize_utc_day_vwap(utc_day).unwrap();

        let index_of = |pair| oracle.pair_index_of(pair).unwrap();
        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair(utc_day, index_of(pair_key(COEN, usd())))
                .unwrap(),
            Some(U256::from(133_333_333u64))
        );
        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair(utc_day, index_of(pair_key(usd(), ETH)))
                .unwrap(),
            Some(fixed18(2_200))
        );

        let (bases, quotes, vwaps) = oracle.get_utc_day_vwap_snapshot(utc_day).unwrap();
        assert_eq!(bases, vec![COEN, usd()]);
        assert_eq!(quotes, vec![usd(), ETH]);
        assert_eq!(vwaps, vec![U256::from(133_333_333u64), fixed18(2_200)]);

        // Unregistered pair (index 0) on a finalized day, and an unfinalized
        // day, both read None.
        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair(utc_day, index_of(pair_key(BTC, USDT)))
                .unwrap(),
            None
        );
        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair(20260101, index_of(pair_key(COEN, usd())))
                .unwrap(),
            None
        );
    });
}

#[test]
fn finalize_utc_day_vwap_writes_nothing_for_a_day_without_data() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        let utc_day = 20260624u32;

        // No snapshots for the day → finalize is a no-op, nothing written.
        oracle.finalize_utc_day_vwap(utc_day).unwrap();

        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair(
                    utc_day,
                    oracle.pair_index_of(pair_key(COEN, usd())).unwrap()
                )
                .unwrap(),
            None
        );
        let (bases, quotes, vwaps) = oracle.get_utc_day_vwap_snapshot(utc_day).unwrap();
        assert!(bases.is_empty() && quotes.is_empty() && vwaps.is_empty());
    });
}

#[test]
fn get_utc_day_vwap_precompile_returns_the_finalized_value() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        let utc_day = 20260624u32;
        let day_start = outbe_primitives::time::date_key_to_utc_timestamp(utc_day);
        oracle
            .write_snapshot(
                day_start + 100,
                &[(pair_key(COEN, usd()), coen840(150), coen840(1))],
            )
            .unwrap();
        oracle.finalize_utc_day_vwap(utc_day).unwrap();

        use crate::precompile::IOracle;
        use alloy_sol_types::SolCall;

        // Finalized day → returns the stored VWAP.
        let call = IOracle::getUtcDayVwapCall {
            base: COEN,
            quote: usd(),
            utcDay: utc_day,
        }
        .abi_encode();
        let decoded = IOracle::getUtcDayVwapCall::abi_decode_returns(
            &crate::precompile::dispatch(storage.clone(), &call, Address::ZERO, U256::ZERO)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded, coen840(150));

        // Unfinalized day → revert.
        let unfinalized = IOracle::getUtcDayVwapCall {
            base: COEN,
            quote: usd(),
            utcDay: 20260625u32,
        }
        .abi_encode();
        assert!(crate::precompile::dispatch(
            storage.clone(),
            &unfinalized,
            Address::ZERO,
            U256::ZERO
        )
        .is_err());
    });
}
#[test]
fn gas_cost_vwap_50h_window_with_varying_snapshot_counts() {
    // Measures gas cost of calculate_vwap for a 50-hour window
    // with increasing snapshot counts (simulating real testnet load).
    // Each snapshot has 1 pair. vote_period=2 blocks, ~1s blocks.
    // 50h = 180,000s → ~90,000 snapshots at 1 per 2s.
    // We test smaller counts to show the linear growth curve.
    let counts = [100, 500, 1_000, 5_000, 10_000];
    let window_seconds: u64 = 50 * 3600; // 50 hours

    for &n in &counts {
        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |handle| {
            let mut oracle = OracleContract::new(handle.clone());
            oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();

            let start_ts: u64 = 1_000_000;
            let interval = window_seconds / n;
            for i in 0..n {
                let ts = start_ts + i * interval;
                let price = U256::from(100 + (i % 10)) * COEN840_SCALE;
                let volume = coen840(1000);
                oracle
                    .write_snapshot(ts, &[(pair_key(COEN, usd()), price, volume)])
                    .unwrap();
            }

            let _vwap = oracle
                .calculate_vwap(pair_key(COEN, usd()), start_ts, start_ts + window_seconds)
                .unwrap();

            // Gas estimation: each snapshot in range costs ~5 sloads
            // (timestamp + pair_count + pair_id + rate + volume).
            // calculate_vwaps calls this for each vote_target pair.
            // With 7 pairs: 7 × n × 5 sloads × 100 gas = 3500 × n gas.
            let estimated_gas_per_pair = n * 5 * 100; // 1 pair
            let estimated_gas_7_pairs = estimated_gas_per_pair * 7;

            eprintln!(
                "VWAP cost: snapshots={n}, window=50h, estimated_gas_1pair={estimated_gas_per_pair}, estimated_gas_7pairs={estimated_gas_7_pairs}, fits_100M={}",
                estimated_gas_7_pairs < 100_000_000
            );

            assert!(
                estimated_gas_7_pairs < 100_000_000,
                "VWAP with {n} snapshots × 7 pairs costs ~{estimated_gas_7_pairs} gas — exceeds 100M"
            );
        });
    }
}

// === Reference currencies tests ===

#[test]
fn genesis_seeds_reference_currencies_with_usd() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(
            &mut oracle,
            &crate::genesis::OracleGenesisConfig::default_config(),
        )
        .unwrap();

        assert_eq!(oracle.reference_currencies.len().unwrap(), 1);
        assert_eq!(oracle.reference_currencies.get(0).unwrap(), Some(840));
    });
}

#[test]
fn genesis_seeds_custom_reference_currencies() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let config = crate::genesis::OracleGenesisConfig {
            reference_currencies: vec![ref_cur(840), ref_cur(978), ref_cur(392)],
            ..crate::genesis::OracleGenesisConfig::default_config()
        };
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();

        assert_eq!(oracle.reference_currencies.len().unwrap(), 3);
        assert_eq!(oracle.reference_currencies.get(0).unwrap(), Some(840));
        assert_eq!(oracle.reference_currencies.get(1).unwrap(), Some(978));
        assert_eq!(oracle.reference_currencies.get(2).unwrap(), Some(392));
    });
}

#[test]
fn init_from_genesis_rejects_a_zero_reference_iso_code() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let config = crate::genesis::OracleGenesisConfig {
            reference_currencies: vec![ref_cur(0)],
            ..crate::genesis::OracleGenesisConfig::default_config()
        };
        let err = crate::genesis::init_from_genesis(&mut oracle, &config).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("reference iso_code must be non-zero"),
            "unexpected error: {msg}"
        );
    });
}

#[test]
fn init_from_genesis_rejects_a_duplicate_reference_iso_code() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let config = crate::genesis::OracleGenesisConfig {
            reference_currencies: vec![ref_cur(840), ref_cur(840)],
            ..crate::genesis::OracleGenesisConfig::default_config()
        };
        let err = crate::genesis::init_from_genesis(&mut oracle, &config).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("duplicate reference iso_code"),
            "unexpected error: {msg}"
        );
    });
}

#[test]
fn export_genesis_round_trips_reference_currencies() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let config = crate::genesis::OracleGenesisConfig {
            reference_currencies: vec![ref_cur(840), ref_cur(978)],
            ..crate::genesis::OracleGenesisConfig::default_config()
        };
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();

        let exported = crate::genesis::export_genesis(&oracle, &[]).unwrap();
        assert_eq!(
            exported.reference_currencies,
            vec![ref_cur(840), ref_cur(978)]
        );
    });
}

#[test]
fn check_reference_currency_accepts_a_seeded_code() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(
            &mut oracle,
            &crate::genesis::OracleGenesisConfig::default_config(),
        )
        .unwrap();
        drop(oracle);

        let ctx = BlockRuntimeContext::new(
            BlockContext::new(1, 1, 1, Address::ZERO, Vec::new()),
            storage,
        );
        crate::api::check_reference_currency(&ctx, 840).unwrap();
    });
}

#[test]
fn check_reference_currency_rejects_an_unseeded_code() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(
            &mut oracle,
            &crate::genesis::OracleGenesisConfig::default_config(),
        )
        .unwrap();
        drop(oracle);

        let ctx = BlockRuntimeContext::new(
            BlockContext::new(1, 1, 1, Address::ZERO, Vec::new()),
            storage,
        );
        let err = crate::api::check_reference_currency(&ctx, 978).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("not a registered reference currency"),
            "unexpected error: {msg}"
        );
    });
}

#[test]
fn get_reference_currencies_precompile_returns_the_seeded_list() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let config = crate::genesis::OracleGenesisConfig {
            reference_currencies: vec![ref_cur(840), ref_cur(978)],
            ..crate::genesis::OracleGenesisConfig::default_config()
        };
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();
        drop(oracle);

        use crate::precompile::IOracle;
        use alloy_sol_types::SolCall;

        let call = IOracle::getReferenceCurrenciesCall {}.abi_encode();
        let decoded = IOracle::getReferenceCurrenciesCall::abi_decode_returns(
            &crate::precompile::dispatch(storage, &call, Address::ZERO, U256::ZERO).unwrap(),
        )
        .unwrap();
        assert_eq!(decoded, vec![840u16, 978u16]);
    });
}

// -----------------------------------------------------------------------
// COEN price lookup (`api::coen_pair_price`)
// -----------------------------------------------------------------------

/// `None` distinguishes "this chain cannot price that currency at all" from
/// `Some(0)`, "the pair exists but the day has no data yet". TributeFactory maps
/// the first to `IssuanceCurrencyNotRegistered` and the second to
/// `NominalPriceUnavailable`, so the two must not collapse.
#[test]
fn coen_pair_price_separates_an_unregistered_pair_from_an_unpriced_day() {
    let day = 20260302u32.into();

    with_storage(|storage| {
        assert_eq!(
            crate::api::coen_pair_price(storage.clone(), 840, day).unwrap(),
            None,
            "no COEN/840 pair is registered"
        );

        OracleContract::new(storage.clone())
            .register_pair(AddressPair::new_coen_to(840))
            .unwrap();
        assert_eq!(
            crate::api::coen_pair_price(storage, 840, day).unwrap(),
            Some(U256::ZERO),
            "registered but with neither a day VWAP nor an S-curve entry"
        );
    });
}

/// Each currency is priced from its own `COEN/<iso>` pair, and a pair COEN is not
/// the base of never leaks into the answer.
#[test]
fn coen_pair_price_reads_the_matching_coen_pair() {
    let eur: Address = AssetType::IsoCurrency(978).into();
    let day = 20260302u32;

    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        oracle.register_pair(AddressPair::new_coen_to(978)).unwrap();
        // COEN is not the base — must not be mistaken for a COEN/<iso> pair.
        oracle
            .register_pair(AddressPair::from_addresses(usd(), ETH))
            .unwrap();

        oracle
            .write_snapshot(
                1_500,
                &[
                    (pair_key(COEN, usd()), coen840(110), coen840(1)),
                    (pair_key(COEN, eur), fixed18(90), SCALE_1E18),
                    (pair_key(usd(), ETH), fixed18(2_200), SCALE_1E18),
                ],
            )
            .unwrap();
        oracle
            .store_worldwide_day_vwap_snapshot(day.into(), 1_000, 3_000)
            .unwrap();

        assert_eq!(
            crate::api::coen_pair_price(storage.clone(), 840, day.into()).unwrap(),
            Some(coen840(110))
        );
        assert_eq!(
            crate::api::coen_pair_price(storage.clone(), 978, day.into()).unwrap(),
            Some(fixed18(90))
        );
        // ETH is quoted against USD, not COEN, so it is not a currency price.
        assert_eq!(
            crate::api::coen_pair_price(storage, 826, day.into()).unwrap(),
            None
        );
    });
}

/// The price is `max(day VWAP, active S-curve)` — an S-curve peak above the day's
/// VWAP wins, and only for the currency it belongs to.
#[test]
fn coen_pair_price_takes_the_max_of_vwap_and_scurve() {
    let eur: Address = AssetType::IsoCurrency(978).into();
    let worldwide_day = outbe_common::WorldwideDay::from_timestamp(ATOMIC_DAY_START);

    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        oracle.register_pair(AddressPair::new_coen_to(978)).unwrap();

        oracle
            .write_snapshot(
                ATOMIC_DAY_START + 100,
                &[
                    (pair_key(COEN, usd()), coen840(110), coen840(1)),
                    (pair_key(COEN, eur), fixed18(90), SCALE_1E18),
                ],
            )
            .unwrap();
        oracle
            .store_worldwide_day_vwap_snapshot(
                worldwide_day,
                ATOMIC_DAY_START,
                ATOMIC_DAY_START + outbe_primitives::time::SECONDS_PER_DAY,
            )
            .unwrap();

        // USD gets an S-curve peak above its VWAP; EUR gets none.
        crate::scurve::store_scurve_entry(
            &mut oracle,
            pair_key(COEN, usd()),
            ATOMIC_DAY_START,
            coen840(500),
        )
        .unwrap();

        assert_eq!(
            crate::api::coen_pair_price(storage.clone(), 840, worldwide_day).unwrap(),
            Some(coen840(500)),
            "S-curve peak wins for USD"
        );
        assert_eq!(
            crate::api::coen_pair_price(storage, 978, worldwide_day).unwrap(),
            Some(fixed18(90)),
            "EUR keeps its own VWAP"
        );
    });
}
