use super::*;

#[test]
fn synthetic_measurement_day_precedes_cycle_utc_day_at_date_boundaries() {
    for (utc_day, seconds, forming, expected) in [
        (20260101, 0, 180_000, 20251230),
        (20261017, 35_999, 180_000, 20261015),
        (20261017, 36_000, 180_000, 20261015),
        (20261017, 43_199, 180_000, 20261015),
        (20261017, 43_200, 180_000, 20261016),
        (20261017, 86_399, 180_000, 20261016),
        (20260301, 0, 180_000, 20260227),
        (20240301, 36_000, 180_000, 20240228),
        (20240301, 43_200, 180_000, 20240229),
        (20260101, 0, 60, 20251231),
        (20240301, 36_000, 60, 20240229),
    ] {
        let execution_day = WorldwideDay::new(utc_day);
        let timestamp = execution_day.to_timestamp_utc().unwrap() + seconds;
        let day = MeasurementDay::new(timestamp, forming, 600).unwrap();
        assert_eq!(day.worldwide_day.value(), expected);
        assert!(day.worldwide_day < execution_day);
        assert_eq!(
            day.forming_start,
            day.worldwide_day.start_timestamp().unwrap()
        );
        assert_eq!(day.forming_end - day.forming_start, forming);
        assert!(day.forming_end <= timestamp);
        assert_eq!(day.genesis_timestamp, timestamp);
        assert_eq!(day.offering_end, timestamp + 600);
    }
    let parameters = outbe_chain_constants::GenesisProtocolParametersV1::from_genesis(
        &serde_json::json!({"config": {"outbeProtocol": {
            "schemaVersion": 1,
            "metadosis": {"lookbackDelaySeconds": 79_200}
        }}}),
    )
    .unwrap();
    let timestamp = WorldwideDay::new(20260101).to_timestamp_utc().unwrap();
    let day =
        MeasurementDay::new(timestamp, parameters.metadosis_forming_period_seconds, 600).unwrap();
    assert_eq!(day.worldwide_day.value(), 20251230);
    assert_eq!(day.forming_end - day.forming_start, 180_000);
}

const NATIVE_CHAIN_ID: u64 = 54_322_345;

#[derive(Clone, Copy)]
struct NativeDateCase {
    host: u64,
    timestamp: u64,
    wwd: u32,
    forming_start: u64,
    offering_start: u64,
    offering_end: u64,
    price_day: u32,
    emission_day: u32,
}

const NATIVE_DATE_CASES: [NativeDateCase; 3] = [
    NativeDateCase {
        host: 1_767_225_599,
        timestamp: 1_767_225_900,
        wwd: 20251228,
        forming_start: 1_766_829_600,
        offering_start: 1_767_088_800,
        offering_end: 1_767_268_800,
        price_day: 20251231,
        emission_day: 20260101,
    },
    NativeDateCase {
        host: 1_709_208_000,
        timestamp: 1_709_251_500,
        wwd: 20240226,
        forming_start: 1_708_855_200,
        offering_start: 1_709_114_400,
        offering_end: 1_709_294_400,
        price_day: 20240229,
        emission_day: 20240301,
    },
    NativeDateCase {
        host: 1_772_236_800,
        timestamp: 1_772_323_500,
        wwd: 20260225,
        forming_start: 1_771_927_200,
        offering_start: 1_772_186_400,
        offering_end: 1_772_366_400,
        price_day: 20260228,
        emission_day: 20260301,
    },
];

impl NativeDateCase {
    fn processing_time(self) -> u64 {
        match self.emission_day {
            20260101 => 1_767_312_001,
            20240301 => 1_709_337_601,
            20260301 => 1_772_409_601,
            day => panic!("unexpected independent fixture day: {day}"),
        }
    }
}

struct PreparedNativeFixture {
    _localnet: Localnet,
    _environment: crate::env::Environment,
    _directory: tempfile::TempDir,
    genesis: serde_json::Value,
    expected: outbe_metadosis::EmissionDayProjection,
}

fn prepare_native_fixture(case: &NativeDateCase, is_nod_lifecycle: bool) -> PreparedNativeFixture {
    let NativeDateCase {
        timestamp,
        wwd,
        emission_day,
        ..
    } = *case;
    let directory = tempfile::tempdir().unwrap();
    let env = crate::env::Environment::default();
    env.ports.start_scenario(env.validators).unwrap();
    let mut cfg = crate::internal::config::Config::for_scenario(&env, 1);
    cfg.dir = directory.path().to_owned();
    let localnet = Localnet::new(cfg);
    let mut genesis = serde_json::json!({
        "config": {"chainId": NATIVE_CHAIN_ID, "outbeProtocol": {
            "schemaVersion": 1,
            "metadosis": {"lookbackDelaySeconds": 79_200}
        }},
        "timestamp": format!("0x{timestamp:x}"),
        "alloc": {}
    });
    for address in [
        EMISSION_ADDRESS,
        METADOSIS_ADDRESS,
        ORACLE_ADDRESS,
        TRIBUTE_ADDRESS,
        TRIBUTE_FACTORY_ADDRESS,
    ] {
        genesis["alloc"][format!("{address:#x}")] =
            serde_json::json!({"storage": {}, "balance": "0x0"});
    }
    let mut provider = HashMapStorageProvider::new(NATIVE_CHAIN_ID);
    let keys = load_measurement_accounts(&genesis, &mut provider).unwrap();
    StorageHandle::enter(&mut provider, |storage| -> PrecompileResult<()> {
        for iso in [840, 978] {
            outbe_oracle::api::register_pair(
                storage.clone(),
                outbe_oracle::types::AddressPair::new_coen_to(iso),
            )?;
        }
        outbe_oracle::api::set_exchange_rate(
            storage,
            Address::ZERO,
            outbe_oracle::api::DAY_TYPE_PAIR,
            outbe_oracle::api::RateObservation {
                rate: U256::from(1_000_000),
                block_number: 0,
                timestamp,
            },
        )
    })
    .unwrap();
    write_measurement_accounts(&mut genesis, &provider, keys).unwrap();
    let path = localnet.cfg.dir.join("genesis.json");
    replace_json_atomically(&path, &genesis).unwrap();
    let (actual_wwd, opts) = if is_nod_lifecycle {
        localnet.prepare_nod_lifecycle_fixture(800_000, 6).unwrap()
    } else {
        localnet.prepare_native_lysis_fixture(6).unwrap()
    };
    assert_eq!(actual_wwd, wwd);
    assert_eq!(opts.voting_window, Some(6));
    assert!(opts.genesis_timestamp_pre_shifted);
    let genesis: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(read_genesis_timestamp(&genesis).unwrap(), timestamp);
    let parameters =
        outbe_chain_constants::GenesisProtocolParametersV1::from_genesis(&genesis).unwrap();
    assert_eq!(parameters.metadosis_forming_period_seconds, 180_000);
    assert_eq!(parameters.metadosis_lookback_delay_seconds, 79_200);
    assert_eq!(parameters.metadosis_offering_period_seconds, 180_000);

    let processing_time = case.processing_time();
    let expected = outbe_metadosis::EmissionDayProjection {
        day: emission_day,
        emission_limit_minor: U256::from(225_485_783_040_000_u64),
        wwd: WorldwideDay::new(wwd),
        metadosis_start: processing_time,
        state: outbe_metadosis::emission_day_state::NONE,
    };

    PreparedNativeFixture {
        _localnet: localnet,
        _environment: env,
        _directory: directory,
        genesis,
        expected,
    }
}

fn native_live_prices(is_nod_lifecycle: bool) -> [(u16, u64); 2] {
    if is_nod_lifecycle {
        [(840, 800_000), (458, 4_512_345)]
    } else {
        [(840, 1_000_000), (978, 250_000)]
    }
}

fn assert_native_fixture_execution(
    fixture: &PreparedNativeFixture,
    case: &NativeDateCase,
    is_nod_lifecycle: bool,
    execution_time: u64,
) {
    let timestamp = case.timestamp;
    let emission_day = case.emission_day;
    let genesis = &fixture.genesis;
    let live_prices = native_live_prices(is_nod_lifecycle);
    let mut provider = HashMapStorageProvider::new(NATIVE_CHAIN_ID);
    load_measurement_accounts(genesis, &mut provider).unwrap();
    provider.set_genesis_timestamp(timestamp);
    StorageHandle::enter(&mut provider, |storage| {
        for (iso, _) in live_prices {
            assert_eq!(
                outbe_oracle::api::get_utc_day_vwap_for_iso(storage.clone(), emission_day, iso,)
                    .unwrap(),
                None,
                "future emission-day price must not exist in genesis"
            );
        }
    });

    record_native_observations(&mut provider, timestamp, execution_time, live_prices);
    provider.set_block_number(26);
    provider.set_timestamp(U256::from(execution_time));
    provider.enable_metadosis_mutation_frames(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
        26,
    );
    StorageHandle::enter(&mut provider, |storage| {
        assert_native_schedule(storage.clone(), case, fixture.expected);
        assert_native_prices(storage.clone(), case, is_nod_lifecycle, execution_time);
        assert_native_processing(storage, case, fixture.expected, execution_time);
    });
}

fn record_native_observations(
    provider: &mut HashMapStorageProvider,
    timestamp: u64,
    execution_time: u64,
    live_prices: [(u16, u64); 2],
) {
    let entries = live_prices.map(|(iso, rate)| {
        (
            outbe_oracle::types::AddressPair::new_coen_to(iso),
            U256::from(rate),
            U256::from(1_000_000_u64),
        )
    });
    for hour in 0..24_u64 {
        let observed_at = timestamp + hour * 3_600;
        if observed_at > execution_time {
            break;
        }
        provider.set_block_number(hour + 2);
        provider.set_timestamp(U256::from(observed_at));
        StorageHandle::enter(provider, |storage| -> PrecompileResult<()> {
            let mut oracle = outbe_oracle::schema::OracleContract::new(storage);
            oracle.config_vote_period.write(1)?;
            oracle.record_hour_block(observed_at, hour + 2)?;
            oracle.write_snapshot(observed_at, &entries)
        })
        .unwrap();
    }
}

fn assert_native_schedule(
    storage: StorageHandle<'_>,
    case: &NativeDateCase,
    expected: outbe_metadosis::EmissionDayProjection,
) {
    let NativeDateCase {
        timestamp,
        wwd,
        forming_start,
        offering_start,
        offering_end,
        price_day,
        emission_day,
        ..
    } = *case;
    let projection = outbe_tributefactory::worldwideday::api::worldwide_day(
        storage.clone(),
        WorldwideDay::new(wwd),
    )
    .unwrap()
    .unwrap();
    assert_eq!(projection.forming_start, forming_start);
    assert_eq!(projection.forming_end, forming_start + 180_000);
    assert_eq!(projection.offerings_start, offering_start);
    assert_eq!(projection.offering_end, offering_end);
    assert_eq!(offering_start - projection.forming_end, 79_200);
    assert_eq!(offering_end - offering_start, 180_000);
    assert!(projection.is_offering_at(timestamp));
    assert_eq!(offering_end - timestamp, 42_900);
    assert_eq!(
        outbe_nod::openings::entry_price_day(offering_end).unwrap(),
        price_day
    );
    assert_eq!(
        outbe_nod::openings::entry_price_day(timestamp).unwrap(),
        price_day
    );
    assert_eq!(
        outbe_metadosis::api::emission_day(storage.clone(), emission_day).unwrap(),
        Some(expected)
    );
}

fn assert_native_prices(
    storage: StorageHandle<'_>,
    case: &NativeDateCase,
    is_nod_lifecycle: bool,
    execution_time: u64,
) {
    let NativeDateCase {
        wwd,
        price_day,
        emission_day,
        ..
    } = *case;
    let processing_time = case.processing_time();
    let live_prices = native_live_prices(is_nod_lifecycle);
    let (issuance, reference, entry_prices, reference_vwap, scurve) = if is_nod_lifecycle {
        (
            458,
            840,
            [(840, 800_000_u64), (458, 10_250_000)],
            800_000_u64,
            800_000_u64,
        )
    } else {
        (
            949,
            978,
            [(840, 1_000_000_u64), (978, 250_000)],
            250_000_u64,
            320_000_u64,
        )
    };
    let inputs = outbe_oracle::api::tribute_pricing_inputs(
        storage.clone(),
        issuance,
        reference,
        WorldwideDay::new(wwd),
    )
    .unwrap()
    .unwrap();
    assert_eq!(inputs.issuance_wwd_vwap_minor, U256::from(10_250_000));
    assert_eq!(inputs.reference_wwd_vwap_minor, U256::from(reference_vwap));
    assert_eq!(inputs.reference_scurve_minor, U256::from(scurve));
    for (iso, value) in entry_prices {
        assert_eq!(
            outbe_oracle::api::get_utc_day_vwap_for_iso(storage.clone(), price_day, iso).unwrap(),
            Some(U256::from(value))
        );
    }
    if execution_time >= processing_time - 1 {
        let mut oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle.record_hour_block(execution_time, 26).unwrap();
        oracle.finalize_utc_day_vwap(emission_day).unwrap();
    }
    for (iso, rate) in live_prices {
        let actual =
            outbe_oracle::api::get_utc_day_vwap_for_iso(storage.clone(), emission_day, iso)
                .unwrap();
        assert_eq!(
            actual,
            (execution_time >= processing_time - 1).then_some(U256::from(rate))
        );
    }
}

fn assert_native_processing(
    storage: StorageHandle<'_>,
    case: &NativeDateCase,
    expected: outbe_metadosis::EmissionDayProjection,
    execution_time: u64,
) {
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    let NativeDateCase {
        timestamp,
        wwd,
        forming_start,
        emission_day,
        ..
    } = *case;
    let processing_time = case.processing_time();
    let context = BlockRuntimeContext::new(
        BlockContext {
            block_number: 26,
            timestamp: execution_time,
            chain_id: NATIVE_CHAIN_ID,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: timestamp,
            proposer: alloy_primitives::Address::ZERO,
            validators: Vec::new(),
        },
        storage.clone(),
    );
    outbe_tributefactory::worldwideday::advance_worldwide_days_command(&context).unwrap();
    let due = outbe_metadosis::commands::find_due_emission_day(&context)
        .unwrap()
        .map(outbe_metadosis::EmissionDayProjection::from);
    assert_eq!(due, (execution_time == processing_time).then_some(expected));
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    assert_eq!(
        oracle
            .worldwide_day_vwap_start
            .read(&WorldwideDay::new(wwd))
            .unwrap(),
        forming_start
    );
    assert_eq!(
        oracle
            .worldwide_day_vwap_end
            .read(&WorldwideDay::new(wwd))
            .unwrap(),
        forming_start + 180_000
    );
    assert_eq!(
        outbe_metadosis::api::emission_day(storage, emission_day).unwrap(),
        Some(expected)
    );
}

fn assert_native_clock_boundaries() {
    assert_eq!(
        Localnet::native_lysis_genesis_timestamp(1_767_225_600).unwrap(),
        1_767_312_300
    );
    assert!(Localnet::native_lysis_genesis_timestamp(u64::MAX).is_err());
    for (host, expected_offset) in [(1_767_225_599, 301), (1_767_226_200, -300)] {
        let opts = native_start_options(1_767_225_900, host, 6).unwrap();
        assert_eq!(opts.unix_time_offset_secs, Some(expected_offset));
        assert_eq!(opts.voting_window, Some(6));
        assert!(opts.genesis_timestamp_pre_shifted);
    }
    assert!(native_start_options(u64::MAX, 0, 6).is_err());
}

#[test]
fn native_lysis_fixture_roundtrip_has_canonical_window_and_linked_allocation() {
    for case in NATIVE_DATE_CASES {
        assert_eq!(
            Localnet::native_lysis_genesis_timestamp(case.host).unwrap(),
            case.timestamp
        );
        for is_nod_lifecycle in [false, true] {
            let fixture = prepare_native_fixture(&case, is_nod_lifecycle);
            for execution_time in [
                case.timestamp,
                case.offering_end,
                case.processing_time() - 1,
                case.processing_time(),
            ] {
                assert_native_fixture_execution(&fixture, &case, is_nod_lifecycle, execution_time);
            }
        }
    }
    assert_native_clock_boundaries();
}
