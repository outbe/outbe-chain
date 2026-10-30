//! The read-only precompile.

use super::*;
use outbe_primitives::time::SECONDS_PER_DAY_U32;

#[test]
fn precompile_series_data_round_trip() {
    with_registry(|s| {
        api::create_series(&s, sample_params(7)).unwrap();

        let call = IIntex::getIntexSeriesCall {
            intexId: crate::schema::intex_id(sid(7)),
        }
        .abi_encode();
        let out = dispatch(s.clone(), &call, Address::ZERO, U256::ZERO).unwrap();
        let data = IIntex::getIntexSeriesCall::abi_decode_returns(&out).unwrap();

        assert_eq!(data.seriesId, alloy_primitives::FixedBytes::from(sid(7)));
        assert_eq!(data.promisLoadMinor, U256::from(PROMIS_LOAD_MINOR));
        assert_eq!(data.entryPriceMinor, U256::from(ENTRY_PRICE_MINOR));
        assert_eq!(data.issuedUnits, 100);
        assert_eq!(data.callWindow, 30 * SECONDS_PER_DAY_U32);
        assert_eq!(data.callThreshold, 5 * SECONDS_PER_DAY_U32);
        assert_eq!(data.callPriceMinor, U256::from(CALL_PRICE_MINOR));
        assert_eq!(data.state, IntexState::Issued as u8);
        assert_eq!(data.issuedAt, ISSUED_AT);
        assert_eq!(data.callNoticePeriod, CALL_NOTICE_PERIOD);
    });
}

#[test]
fn precompile_series_data_missing_reverts() {
    with_registry(|s| {
        let call = IIntex::getIntexSeriesCall {
            intexId: crate::schema::intex_id(sid(99)),
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
            intexId: crate::schema::intex_id(sid(7)),
        }
        .abi_encode();
        let out = dispatch(s.clone(), &yes, Address::ZERO, U256::ZERO).unwrap();
        assert!(IIntex::seriesExistsCall::abi_decode_returns(&out).unwrap());

        let no = IIntex::seriesExistsCall {
            intexId: crate::schema::intex_id(sid(8)),
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
            U256::from(2)
        );

        let at1 = IIntex::seriesAtCall {
            index: U256::from(1),
        }
        .abi_encode();
        let out = dispatch(s.clone(), &at1, Address::ZERO, U256::ZERO).unwrap();
        assert_eq!(
            IIntex::seriesAtCall::abi_decode_returns(&out).unwrap(),
            alloy_primitives::FixedBytes::from(sid(22))
        );
    });
}

#[test]
fn precompile_series_at_past_the_end_reverts() {
    with_registry(|s| {
        api::create_series(&s, sample_params(11)).unwrap();

        for index in [U256::from(1), U256::MAX] {
            let call = IIntex::seriesAtCall { index }.abi_encode();
            assert!(dispatch(s.clone(), &call, Address::ZERO, U256::ZERO).is_err());
        }
    });
}

#[test]
fn precompile_rejects_value() {
    with_registry(|s| {
        let call = IIntex::totalSeriesCall {}.abi_encode();
        assert!(dispatch(s.clone(), &call, Address::ZERO, U256::from(1)).is_err());
    });
}
