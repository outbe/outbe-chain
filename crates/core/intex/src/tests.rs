use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api;
use crate::precompile::{dispatch, IIntex};
use crate::schema::{CreateSeriesParams, IntexCallTrigger, IntexState, SeriesId};
use outbe_primitives::storage::types::{Storable, StorageKey};

const CHAIN_ID: u64 = 1;
const ISSUED_AT: u32 = 1_700_000_000;
const PROMIS_LOAD_MINOR: u128 = 1_000_000_000_000_000_000; // 1e18
const CALL_NOTICE_PERIOD: u32 = 21 * 24 * 60 * 60;

fn with_registry<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    StorageHandle::enter(&mut storage, f)
}

#[test]
fn certified_contributor_generation_is_absent_before_activation() {
    with_registry(|storage| {
        assert_eq!(
            api::certified_contributor_generation(&storage, 20_260_725).unwrap(),
            None
        );
    });
}

#[test]
fn certified_contributor_generation_reads_fail_closed_on_residual_or_malformed_state() {
    for corrupt in 0..6 {
        with_registry(|storage| {
            let series_id = 20_260_726 + corrupt;
            let intex = crate::IntexContract::new(storage.clone());
            match corrupt {
                0 => intex
                    .ocomp_contributor_root
                    .write(&series_id, alloy_primitives::B256::repeat_byte(1))
                    .unwrap(),
                1 => intex
                    .ocomp_contributor_metadata
                    .write(&series_id, U256::from(1))
                    .unwrap(),
                2 => {
                    intex
                        .ocomp_contributor_root
                        .write(&series_id, alloy_primitives::B256::repeat_byte(2))
                        .unwrap();
                    intex
                        .ocomp_contributor_metadata
                        .write(&series_id, U256::from(1) << 96)
                        .unwrap();
                }
                3 => {
                    intex
                        .ocomp_contributor_root
                        .write(&series_id, alloy_primitives::B256::repeat_byte(3))
                        .unwrap();
                    intex
                        .ocomp_contributor_metadata
                        .write(&series_id, U256::from(1))
                        .unwrap();
                    intex
                        .ocomp_eligible_nominal_total
                        .write(&series_id, U256::from(1))
                        .unwrap();
                }
                4 => {
                    intex
                        .ocomp_contributor_root
                        .write(&series_id, alloy_primitives::B256::repeat_byte(4))
                        .unwrap();
                    intex
                        .ocomp_contributor_metadata
                        .write(&series_id, U256::from(2))
                        .unwrap();
                }
                5 => {
                    api::create_series(&storage, sample_params(series_id)).unwrap();
                    intex
                        .ocomp_contributor_root
                        .write(&series_id, alloy_primitives::B256::repeat_byte(5))
                        .unwrap();
                    intex
                        .ocomp_contributor_metadata
                        .write(&series_id, U256::from(3))
                        .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(api::certified_contributor_generation(&storage, series_id).is_err());
            assert!(api::ocomp_contributor_target_projection(&storage, series_id).is_err());
        });
    }
}

#[test]
fn certified_contributor_installation_has_no_public_write_selector() {
    let selector = alloy_primitives::keccak256(b"installCertifiedContributorRoot(bytes)");
    let calldata = selector[..4].to_vec();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);

    let result = StorageHandle::enter(&mut provider, |storage| {
        dispatch(storage, &calldata, Address::ZERO, U256::ZERO)
    });
    assert!(result.is_err());
    assert!(provider.storage.is_empty());
    assert!(provider.get_ordered_events().is_empty());
}

/// Test ids carry a fixed USD/U pair; only the day varies.
fn sid(worldwide_day: u32) -> SeriesId {
    SeriesId::pack(worldwide_day, *b"USD", b'U').unwrap()
}

fn sample_params(worldwide_day: u32) -> CreateSeriesParams {
    CreateSeriesParams {
        series_id: sid(worldwide_day),
        worldwide_day: worldwide_day.into(),
        issued_intex_count: 100,
        promis_load_minor: PROMIS_LOAD_MINOR,
        entry_price_minor: U256::from(2_000u64),
        floor_price_minor: U256::from(1_500u64),
        call_price_minor: U256::from(900u64),
        call_trigger: IntexCallTrigger {
            call_window: 30 * 24 * 60 * 60,
            call_threshold: 5 * 24 * 60 * 60,
            call_notice_period: CALL_NOTICE_PERIOD,
        },
        issued_at: ISSUED_AT,
        issuance_currency: 840,
        reference_currency: 840,
    }
}

// ---------------------------------------------------------------------
// create + read round-trip
// ---------------------------------------------------------------------

#[test]
fn create_then_read_round_trip() {
    with_registry(|s| {
        let mut p = sample_params(7);
        p.worldwide_day = 20260101.into();
        api::create_series(&s, p).unwrap();

        let r = api::read_series(&s, sid(7)).unwrap();
        assert_eq!(r.series_id, sid(7));
        // u128 -> U256 widening preserved.
        assert_eq!(r.promis_load_minor, U256::from(PROMIS_LOAD_MINOR));
        assert_eq!(r.entry_price_minor, U256::from(2_000u64));
        assert_eq!(r.floor_price_minor, U256::from(1_500u64));
        assert_eq!(r.issued_intex_count, 100);
        assert_eq!(
            r.call_trigger(),
            IntexCallTrigger {
                call_window: 30 * 24 * 60 * 60,
                call_threshold: 5 * 24 * 60 * 60,
                call_notice_period: CALL_NOTICE_PERIOD,
            }
        );
        assert_eq!(r.lifecycle_state().unwrap(), IntexState::Issued);
        assert_eq!(r.issued_at, ISSUED_AT);
        assert_eq!(r.called_at, 0);
        assert_eq!(r.worldwide_day, 20260101.into());
        // The ledger stores the call period verbatim; defaulting is the
        // caller's job.
        assert_eq!(r.call_notice_period, CALL_NOTICE_PERIOD);
        assert_eq!(r.issuance_currency, 840);
        assert_eq!(r.reference_currency, 840);
    });
}

// ---------------------------------------------------------------------
// create validation (ledger guards only the existence sentinel)
// ---------------------------------------------------------------------

#[test]
fn create_rejects_duplicate_series_id() {
    with_registry(|s| {
        api::create_series(&s, sample_params(7)).unwrap();
        let err = api::create_series(&s, sample_params(7)).unwrap_err();
        // record-level create rejects the existing slot.
        assert!(err.to_string().to_lowercase().contains("exist"));
    });
}

#[test]
fn create_rejects_zero_issued_at() {
    with_registry(|s| {
        let mut p = sample_params(1);
        p.issued_at = 0;
        assert!(api::create_series(&s, p).is_err());
    });
}

// ---------------------------------------------------------------------
// reads on a missing series
// ---------------------------------------------------------------------

#[test]
fn reads_on_missing_series() {
    with_registry(|s| {
        assert!(api::read_series(&s, sid(42)).is_err());
        assert_eq!(api::get_series(&s, sid(42)).unwrap(), None);
        assert!(!api::series_exists(&s, sid(42)).unwrap());
    });
}

// ---------------------------------------------------------------------
// state machine: mark_qualified
// ---------------------------------------------------------------------

#[test]
fn mark_qualified_from_issued() {
    with_registry(|s| {
        api::create_series(&s, sample_params(1)).unwrap();
        api::mark_qualified(&s, sid(1)).unwrap();
        assert_eq!(
            api::read_series(&s, sid(1))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            IntexState::Qualified
        );
    });
}

#[test]
fn mark_qualified_rejected_when_not_issued() {
    with_registry(|s| {
        api::create_series(&s, sample_params(1)).unwrap();
        api::mark_qualified(&s, sid(1)).unwrap();
        // Already Qualified -> rejected.
        let err = api::mark_qualified(&s, sid(1)).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("state"));
    });
}

#[test]
fn mark_qualified_rejected_on_missing() {
    with_registry(|s| {
        assert!(api::mark_qualified(&s, sid(1)).is_err());
    });
}

// ---------------------------------------------------------------------
// state machine: mark_called
// ---------------------------------------------------------------------

#[test]
fn mark_called_from_issued_sets_called_at() {
    with_registry(|s| {
        api::create_series(&s, sample_params(1)).unwrap();
        api::mark_called(&s, sid(1), ISSUED_AT + 10).unwrap();
        let r = api::read_series(&s, sid(1)).unwrap();
        assert_eq!(r.lifecycle_state().unwrap(), IntexState::Called);
        assert_eq!(r.called_at, ISSUED_AT + 10);
    });
}

#[test]
fn mark_called_from_qualified() {
    with_registry(|s| {
        api::create_series(&s, sample_params(1)).unwrap();
        api::mark_qualified(&s, sid(1)).unwrap();
        api::mark_called(&s, sid(1), ISSUED_AT + 10).unwrap();
        assert_eq!(
            api::read_series(&s, sid(1))
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            IntexState::Called
        );
    });
}

#[test]
fn mark_called_rejected_when_already_called() {
    with_registry(|s| {
        api::create_series(&s, sample_params(1)).unwrap();
        api::mark_called(&s, sid(1), ISSUED_AT + 10).unwrap();
        assert!(api::mark_called(&s, sid(1), ISSUED_AT + 20).is_err());
    });
}

// ---------------------------------------------------------------------
// enumeration
// ---------------------------------------------------------------------

#[test]
fn dense_enumeration_tracks_created_series() {
    with_registry(|s| {
        assert_eq!(api::total_series(&s).unwrap(), 0);
        api::create_series(&s, sample_params(11)).unwrap();
        api::create_series(&s, sample_params(22)).unwrap();
        api::create_series(&s, sample_params(33)).unwrap();

        assert_eq!(api::total_series(&s).unwrap(), 3);
        assert_eq!(api::series_id_at(&s, 0).unwrap(), sid(11));
        assert_eq!(api::series_id_at(&s, 1).unwrap(), sid(22));
        assert_eq!(api::series_id_at(&s, 2).unwrap(), sid(33));
    });
}

// ---------------------------------------------------------------------
// state encoding parity
// ---------------------------------------------------------------------

#[test]
fn intex_state_encoding_matches_solidity() {
    assert_eq!(IntexState::Issued as u8, 0);
    assert_eq!(IntexState::Qualified as u8, 1);
    assert_eq!(IntexState::Called as u8, 2);
    assert_eq!(IntexState::from_u8(0).unwrap(), IntexState::Issued);
    assert_eq!(IntexState::from_u8(1).unwrap(), IntexState::Qualified);
    assert_eq!(IntexState::from_u8(2).unwrap(), IntexState::Called);
    assert!(IntexState::from_u8(3).is_err());
}

// ---------------------------------------------------------------------
// read-only precompile
// ---------------------------------------------------------------------

#[test]
fn precompile_series_data_round_trip() {
    with_registry(|s| {
        api::create_series(&s, sample_params(7)).unwrap();
        api::mark_qualified(&s, sid(7)).unwrap();

        let call = IIntex::seriesDataCall {
            seriesId: sid(7).into(),
        }
        .abi_encode();
        let out = dispatch(s.clone(), &call, Address::ZERO, U256::ZERO).unwrap();
        let data = IIntex::seriesDataCall::abi_decode_returns(&out).unwrap();

        assert_eq!(data.seriesId, alloy_primitives::FixedBytes::from(sid(7)));
        assert_eq!(data.promisLoadMinor, U256::from(PROMIS_LOAD_MINOR));
        assert_eq!(data.entryPriceMinor, U256::from(2_000u64));
        assert_eq!(data.floorPriceMinor, U256::from(1_500u64));
        assert_eq!(data.issuedIntexCount, 100);
        assert_eq!(data.callWindow, 30 * 24 * 60 * 60);
        assert_eq!(data.callThreshold, 5 * 24 * 60 * 60);
        assert_eq!(data.callPriceMinor, U256::from(900u64));
        assert_eq!(data.state, IntexState::Qualified as u8);
        assert_eq!(data.issuedAt, ISSUED_AT);
        assert_eq!(data.callNoticePeriod, CALL_NOTICE_PERIOD);
    });
}

#[test]
fn precompile_series_data_missing_reverts() {
    with_registry(|s| {
        let call = IIntex::seriesDataCall {
            seriesId: sid(99).into(),
        }
        .abi_encode();
        assert!(dispatch(s.clone(), &call, Address::ZERO, U256::ZERO).is_err());
    });
}

#[test]
fn precompile_series_exists() {
    with_registry(|s| {
        api::create_series(&s, sample_params(7)).unwrap();

        let yes = IIntex::seriesExistsCall {
            seriesId: sid(7).into(),
        }
        .abi_encode();
        let out = dispatch(s.clone(), &yes, Address::ZERO, U256::ZERO).unwrap();
        assert!(IIntex::seriesExistsCall::abi_decode_returns(&out).unwrap());

        let no = IIntex::seriesExistsCall {
            seriesId: sid(8).into(),
        }
        .abi_encode();
        let out = dispatch(s.clone(), &no, Address::ZERO, U256::ZERO).unwrap();
        assert!(!IIntex::seriesExistsCall::abi_decode_returns(&out).unwrap());
    });
}

#[test]
fn precompile_total_and_at() {
    with_registry(|s| {
        api::create_series(&s, sample_params(11)).unwrap();
        api::create_series(&s, sample_params(22)).unwrap();

        let total = IIntex::totalSeriesCall {}.abi_encode();
        let out = dispatch(s.clone(), &total, Address::ZERO, U256::ZERO).unwrap();
        assert_eq!(
            IIntex::totalSeriesCall::abi_decode_returns(&out).unwrap(),
            2
        );

        let at1 = IIntex::seriesAtCall { index: 1 }.abi_encode();
        let out = dispatch(s.clone(), &at1, Address::ZERO, U256::ZERO).unwrap();
        assert_eq!(
            IIntex::seriesAtCall::abi_decode_returns(&out).unwrap(),
            alloy_primitives::FixedBytes::from(sid(22))
        );
    });
}

#[test]
fn precompile_rejects_value() {
    with_registry(|s| {
        let call = IIntex::totalSeriesCall {}.abi_encode();
        assert!(dispatch(s.clone(), &call, Address::ZERO, U256::from(1)).is_err());
    });
}

// ---------------------------------------------------------------------
// Creator-reward: contributors
// ---------------------------------------------------------------------

fn addr(n: u8) -> Address {
    Address::from([n; 20])
}

#[test]
fn record_and_read_contributors() {
    with_registry(|s| {
        let contributors = vec![
            (addr(1), U256::from(90u64)),
            (addr(2), U256::from(110u64)),
            (addr(3), U256::from(300u64)),
        ];
        api::record_contributors(&s, 20_260_401, &contributors).unwrap();

        assert_eq!(api::contributor_count(&s, 20_260_401).unwrap(), 3);
        assert_eq!(
            api::contributor_total(&s, 20_260_401).unwrap(),
            U256::from(500u64)
        );
        assert_eq!(
            api::contributor_at(&s, 20_260_401, 0).unwrap(),
            (addr(1), U256::from(90u64))
        );
        assert_eq!(
            api::contributor_at(&s, 20_260_401, 2).unwrap(),
            (addr(3), U256::from(300u64))
        );
        assert_eq!(
            api::read_contributors(&s, 20_260_401).unwrap(),
            contributors
        );
    });
}

#[test]
fn contributors_empty_series_is_zero() {
    with_registry(|s| {
        assert_eq!(api::contributor_count(&s, 1).unwrap(), 0);
        assert_eq!(api::contributor_total(&s, 1).unwrap(), U256::ZERO);
        assert!(api::read_contributors(&s, 1).unwrap().is_empty());
    });
}

// ---------------------------------------------------------------------
// Creator-reward: paginated distribution progress + active set
// ---------------------------------------------------------------------

#[test]
fn start_distribution_rejects_duplicate() {
    with_registry(|s| {
        api::record_contributors(&s, 7, &[(addr(1), U256::from(40u64))]).unwrap();
        api::start_distribution(&s, 7, U256::from(1000u64), U256::from(40u64)).unwrap();
        // A second open for the same series must not overwrite in-flight progress.
        assert!(api::start_distribution(&s, 7, U256::from(500u64), U256::from(40u64)).is_err());
    });
}

#[test]
fn distribution_progress_lifecycle() {
    with_registry(|s| {
        api::record_contributors(
            &s,
            7,
            &[(addr(1), U256::from(40u64)), (addr(2), U256::from(60u64))],
        )
        .unwrap();
        api::start_distribution(&s, 7, U256::from(1000u64), U256::from(100u64)).unwrap();

        let p = api::get_progress(&s, 7).unwrap().expect("progress exists");
        assert_eq!(p.worldwide_day, 7);
        assert_eq!(p.amount, U256::from(1000u64));
        assert_eq!(p.total_nominal, U256::from(100u64));
        assert_eq!(p.cursor, 0);
        assert_eq!(p.paid_so_far, U256::ZERO);
        assert_eq!(p.active, 1);

        // advance one chunk
        let mut p2 = p.clone();
        p2.cursor = 1;
        p2.paid_so_far = U256::from(400u64);
        api::save_progress(&s, &p2).unwrap();
        let p3 = api::get_progress(&s, 7).unwrap().unwrap();
        assert_eq!(p3.cursor, 1);
        assert_eq!(p3.paid_so_far, U256::from(400u64));

        // enrolled in the active set
        assert_eq!(api::active_dist_count(&s).unwrap(), 1);
        assert_eq!(api::active_dist_at(&s, 0).unwrap(), 7);

        // finish: progress, contributors and active entry all gone
        api::clear_distribution(&s, 7).unwrap();
        assert_eq!(api::get_progress(&s, 7).unwrap(), None);
        assert_eq!(api::contributor_count(&s, 7).unwrap(), 0);
        assert_eq!(api::contributor_total(&s, 7).unwrap(), U256::ZERO);
        assert_eq!(api::active_dist_count(&s).unwrap(), 0);
    });
}

#[test]
fn active_dist_set_swap_remove() {
    with_registry(|s| {
        for sid in [11u32, 22, 33] {
            api::start_distribution(&s, sid, U256::from(1u64), U256::from(1u64)).unwrap();
        }
        assert_eq!(api::active_dist_count(&s).unwrap(), 3);

        // remove the middle one; swap-remove moves the last into its slot.
        api::clear_distribution(&s, 22).unwrap();
        assert_eq!(api::active_dist_count(&s).unwrap(), 2);

        let remaining: Vec<u32> = (0..api::active_dist_count(&s).unwrap())
            .map(|i| api::active_dist_at(&s, i).unwrap())
            .collect();
        assert!(remaining.contains(&11));
        assert!(remaining.contains(&33));
        assert!(!remaining.contains(&22));
    });
}

// ---------------------------------------------------------------------
// composite series id
// ---------------------------------------------------------------------

const DAY: u32 = 20_260_212;

#[test]
fn packs_and_unpacks_every_component() {
    let id = SeriesId::pack(DAY, *b"TRY", b'U').unwrap();
    assert_eq!(id.worldwide_day(), DAY);
    assert_eq!(id.as_bytes(), b"20260212-TRY-U");
    assert_eq!(SeriesId::from_bytes(*id.as_bytes()), id);
}

#[test]
fn reads_as_text_in_both_code_forms() {
    assert_eq!(
        SeriesId::pack(DAY, *b"TRY", b'U').unwrap().to_string(),
        "20260212-TRY-U"
    );
    assert_eq!(
        SeriesId::pack(DAY, SeriesId::numeric_code(949).unwrap(), b'U')
            .unwrap()
            .to_string(),
        "20260212-949-U"
    );
}

#[test]
fn numeric_code_zero_pads_and_refuses_a_wider_code() {
    assert_eq!(SeriesId::numeric_code(840).unwrap(), *b"840");
    assert_eq!(SeriesId::numeric_code(32).unwrap(), *b"032");
    assert_eq!(SeriesId::numeric_code(8).unwrap(), *b"008");
    // ISO numbers are three digits. Folding a wider one would spell two currencies
    // the same way and give a day two series with one id.
    assert!(SeriesId::numeric_code(1949).is_err());
    assert!(SeriesId::for_pair(DAY, 1949, 840).is_err());
}

#[test]
fn rejects_a_zero_day_and_lowercase_or_symbol_codes() {
    assert!(SeriesId::pack(0, *b"USD", b'U').is_err());
    assert!(SeriesId::pack(100_000_000, *b"USD", b'U').is_err());
    assert!(SeriesId::pack(DAY, *b"usd", b'U').is_err());
    assert!(SeriesId::pack(DAY, *b"US-", b'U').is_err());
    assert!(SeriesId::pack(DAY, *b"USD", b'u').is_err());
    assert!(SeriesId::pack(DAY, *b"USD", 0).is_err());
}

#[test]
fn orders_by_day_before_currency() {
    // Dense enumeration and the bin-tree range scans both walk ids in order,
    // so a later day must never sort before an earlier one.
    let early_z = SeriesId::pack(DAY, *b"ZWL", b'Z').unwrap();
    let late_a = SeriesId::pack(DAY + 1, *b"AED", b'A').unwrap();
    assert!(early_z < late_a);

    let same_day_a = SeriesId::pack(DAY, *b"AED", b'U').unwrap();
    assert!(same_day_a < early_z);
}

#[test]
fn round_trips_through_storage_word_and_key() {
    let id = SeriesId::pack(DAY, *b"EUR", b'E').unwrap();
    assert_eq!(SeriesId::from_word(id.to_word()), id);
    assert_eq!(id.key_bytes(), b"20260212-EUR-E".to_vec());
}

// ---------------------------------------------------------------------
// proceeds fan-in across several issuances of one day
// ---------------------------------------------------------------------

const DEADLINE: u64 = ISSUED_AT as u64 + 1000;

#[test]
fn arming_twice_in_one_day_expects_the_union_of_winning_chains() {
    with_registry(|storage| {
        api::arm_proceeds(&storage, DAY, &[10, 20], DEADLINE).unwrap();
        api::arm_proceeds(&storage, DAY, &[20, 30], DEADLINE).unwrap();

        // Chain 20 is armed by both and must count once; chain 30 joins the two
        // already armed, so the fan-in completes only on the third arrival.
        api::credit_proceeds(&storage, DAY, 10, U256::from(1u64)).unwrap();
        api::credit_proceeds(&storage, DAY, 20, U256::from(1u64)).unwrap();
        assert!(!api::proceeds_ready(&storage, DAY).unwrap());

        api::credit_proceeds(&storage, DAY, 30, U256::from(1u64)).unwrap();
        assert!(api::proceeds_ready(&storage, DAY).unwrap());
    });
}

#[test]
fn a_later_arming_does_not_complete_the_fan_in_early() {
    with_registry(|storage| {
        api::arm_proceeds(&storage, DAY, &[10, 20], DEADLINE).unwrap();
        api::credit_proceeds(&storage, DAY, 10, U256::from(1u64)).unwrap();

        // A second issuance arms one further chain while an earlier one is still
        // outstanding: the day must not read ready off the newcomer alone.
        api::arm_proceeds(&storage, DAY, &[30], DEADLINE).unwrap();
        api::credit_proceeds(&storage, DAY, 30, U256::from(1u64)).unwrap();
        assert!(!api::proceeds_ready(&storage, DAY).unwrap());

        api::credit_proceeds(&storage, DAY, 20, U256::from(1u64)).unwrap();
        assert!(api::proceeds_ready(&storage, DAY).unwrap());
    });
}
