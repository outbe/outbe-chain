use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api;
use crate::precompile::{dispatch, IIntexRegistry};
use crate::schema::{CreateSeriesParams, IntexCallTrigger, IntexState};

const CHAIN_ID: u64 = 1;
const ISSUED_AT: u32 = 1_700_000_000;
const PROMIS_LOAD_MINOR: u128 = 1_000_000_000_000_000_000; // 1e18
const CALL_PERIOD: u32 = 21 * 24 * 60 * 60; // 21 days

fn with_registry<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    StorageHandle::enter(&mut storage, f)
}

fn sample_params(series_id: u32) -> CreateSeriesParams {
    CreateSeriesParams {
        series_id,
        issued_intex_count: 100,
        promis_load_minor: PROMIS_LOAD_MINOR,
        cost_amount_minor: 2_000,
        floor_price_minor: U256::from(1_500u64),
        intex_call_period: CALL_PERIOD,
        call_trigger: IntexCallTrigger {
            window_days: 30,
            threshold_days: 5,
            coen_price_call_trigger: U256::from(900u64),
        },
        issued_at: ISSUED_AT,
    }
}

// ---------------------------------------------------------------------
// create + read round-trip
// ---------------------------------------------------------------------

#[test]
fn create_then_read_round_trip() {
    with_registry(|s| {
        api::create_series(&s, sample_params(7)).unwrap();

        let r = api::read_series(&s, 7).unwrap();
        assert_eq!(r.series_id, 7);
        // u128 -> U256 widening preserved.
        assert_eq!(r.promis_load_minor, U256::from(PROMIS_LOAD_MINOR));
        assert_eq!(r.cost_amount_minor, 2_000);
        assert_eq!(r.floor_price_minor, U256::from(1_500u64));
        assert_eq!(r.issued_intex_count, 100);
        assert_eq!(
            r.call_trigger(),
            IntexCallTrigger {
                window_days: 30,
                threshold_days: 5,
                coen_price_call_trigger: U256::from(900u64),
            }
        );
        assert_eq!(r.lifecycle_state().unwrap(), IntexState::Issued);
        assert_eq!(r.issued_at, ISSUED_AT);
        assert_eq!(r.called_at, 0);
        // The ledger stores the call period verbatim; defaulting is the
        // caller's job.
        assert_eq!(r.intex_call_period, CALL_PERIOD);
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
        assert!(api::read_series(&s, 42).is_err());
        assert_eq!(api::get_series(&s, 42).unwrap(), None);
        assert!(!api::series_exists(&s, 42).unwrap());
    });
}

// ---------------------------------------------------------------------
// state machine: mark_qualified
// ---------------------------------------------------------------------

#[test]
fn mark_qualified_from_issued() {
    with_registry(|s| {
        api::create_series(&s, sample_params(1)).unwrap();
        api::mark_qualified(&s, 1).unwrap();
        assert_eq!(
            api::read_series(&s, 1).unwrap().lifecycle_state().unwrap(),
            IntexState::Qualified
        );
    });
}

#[test]
fn mark_qualified_rejected_when_not_issued() {
    with_registry(|s| {
        api::create_series(&s, sample_params(1)).unwrap();
        api::mark_qualified(&s, 1).unwrap();
        // Already Qualified -> rejected.
        let err = api::mark_qualified(&s, 1).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("state"));
    });
}

#[test]
fn mark_qualified_rejected_on_missing() {
    with_registry(|s| {
        assert!(api::mark_qualified(&s, 1).is_err());
    });
}

// ---------------------------------------------------------------------
// state machine: mark_called
// ---------------------------------------------------------------------

#[test]
fn mark_called_from_issued_sets_called_at() {
    with_registry(|s| {
        api::create_series(&s, sample_params(1)).unwrap();
        api::mark_called(&s, 1, ISSUED_AT + 10).unwrap();
        let r = api::read_series(&s, 1).unwrap();
        assert_eq!(r.lifecycle_state().unwrap(), IntexState::Called);
        assert_eq!(r.called_at, ISSUED_AT + 10);
    });
}

#[test]
fn mark_called_from_qualified() {
    with_registry(|s| {
        api::create_series(&s, sample_params(1)).unwrap();
        api::mark_qualified(&s, 1).unwrap();
        api::mark_called(&s, 1, ISSUED_AT + 10).unwrap();
        assert_eq!(
            api::read_series(&s, 1).unwrap().lifecycle_state().unwrap(),
            IntexState::Called
        );
    });
}

#[test]
fn mark_called_rejected_when_already_called() {
    with_registry(|s| {
        api::create_series(&s, sample_params(1)).unwrap();
        api::mark_called(&s, 1, ISSUED_AT + 10).unwrap();
        assert!(api::mark_called(&s, 1, ISSUED_AT + 20).is_err());
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
        assert_eq!(api::series_id_at(&s, 0).unwrap(), 11);
        assert_eq!(api::series_id_at(&s, 1).unwrap(), 22);
        assert_eq!(api::series_id_at(&s, 2).unwrap(), 33);
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
        api::mark_qualified(&s, 7).unwrap();

        let call = IIntexRegistry::seriesDataCall { seriesId: 7 }.abi_encode();
        let out = dispatch(s.clone(), &call, Address::ZERO, U256::ZERO).unwrap();
        let data = IIntexRegistry::seriesDataCall::abi_decode_returns(&out).unwrap();

        assert_eq!(data.seriesId, 7);
        assert_eq!(data.promisLoadMinor, U256::from(PROMIS_LOAD_MINOR));
        assert_eq!(data.costAmountMinor, 2_000);
        assert_eq!(data.floorPriceMinor, U256::from(1_500u64));
        assert_eq!(data.issuedIntexCount, 100);
        assert_eq!(data.callWindowDays, 30);
        assert_eq!(data.callThresholdDays, 5);
        assert_eq!(data.coenPriceCallTrigger, U256::from(900u64));
        assert_eq!(data.state, IntexState::Qualified as u8);
        assert_eq!(data.issuedAt, ISSUED_AT);
        assert_eq!(data.intexCallPeriod, CALL_PERIOD);
    });
}

#[test]
fn precompile_series_data_missing_reverts() {
    with_registry(|s| {
        let call = IIntexRegistry::seriesDataCall { seriesId: 99 }.abi_encode();
        assert!(dispatch(s.clone(), &call, Address::ZERO, U256::ZERO).is_err());
    });
}

#[test]
fn precompile_series_exists() {
    with_registry(|s| {
        api::create_series(&s, sample_params(7)).unwrap();

        let yes = IIntexRegistry::seriesExistsCall { seriesId: 7 }.abi_encode();
        let out = dispatch(s.clone(), &yes, Address::ZERO, U256::ZERO).unwrap();
        assert!(IIntexRegistry::seriesExistsCall::abi_decode_returns(&out).unwrap());

        let no = IIntexRegistry::seriesExistsCall { seriesId: 8 }.abi_encode();
        let out = dispatch(s.clone(), &no, Address::ZERO, U256::ZERO).unwrap();
        assert!(!IIntexRegistry::seriesExistsCall::abi_decode_returns(&out).unwrap());
    });
}

#[test]
fn precompile_total_and_at() {
    with_registry(|s| {
        api::create_series(&s, sample_params(11)).unwrap();
        api::create_series(&s, sample_params(22)).unwrap();

        let total = IIntexRegistry::totalSeriesCall {}.abi_encode();
        let out = dispatch(s.clone(), &total, Address::ZERO, U256::ZERO).unwrap();
        assert_eq!(
            IIntexRegistry::totalSeriesCall::abi_decode_returns(&out).unwrap(),
            2
        );

        let at1 = IIntexRegistry::seriesAtCall { index: 1 }.abi_encode();
        let out = dispatch(s.clone(), &at1, Address::ZERO, U256::ZERO).unwrap();
        assert_eq!(
            IIntexRegistry::seriesAtCall::abi_decode_returns(&out).unwrap(),
            22
        );
    });
}

#[test]
fn precompile_rejects_value() {
    with_registry(|s| {
        let call = IIntexRegistry::totalSeriesCall {}.abi_encode();
        assert!(dispatch(s.clone(), &call, Address::ZERO, U256::from(1)).is_err());
    });
}
