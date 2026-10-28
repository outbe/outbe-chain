//! Series records: create, read, call and enumerate.

use super::*;
use outbe_primitives::time::SECONDS_PER_DAY_U32;

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
        assert_eq!(r.entry_price_minor, U256::from(ENTRY_PRICE_MINOR));
        assert_eq!(r.issued_units, 100);
        assert_eq!(
            r.call_trigger(),
            IntexCallTrigger {
                call_window_seconds: 30 * SECONDS_PER_DAY_U32,
                call_threshold_seconds: 5 * SECONDS_PER_DAY_U32,
                call_notice_period_seconds: CALL_NOTICE_PERIOD,
                transit_margin_seconds: 3_600,
            }
        );
        assert_eq!(api::state(&s, &r).unwrap(), IntexState::Issued);
        assert_eq!(r.issued_at, ISSUED_AT);
        assert_eq!(api::call_stamp(&s, &r).unwrap().called_at, 0);
        assert_eq!(r.worldwide_day, 20260101.into());
        // The ledger stores the call period verbatim. The caller must apply
        // any default.
        assert_eq!(r.call_notice_period_seconds, CALL_NOTICE_PERIOD);
        assert_eq!(r.issuance_currency, 840);
        assert_eq!(r.reference_currency, 840);
    });
}

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

#[test]
fn reads_on_missing_series() {
    with_registry(|s| {
        assert!(api::read_series(&s, sid(42)).is_err());
        assert_eq!(api::get_series(&s, sid(42)).unwrap(), None);
        assert!(!api::series_exists(&s, sid(42)).unwrap());
    });
}

#[test]
fn a_spent_series_is_deleted_and_leaves_the_enumeration() {
    with_registry(|s| {
        api::create_series(&s, sample_params(11)).unwrap();
        api::create_series(&s, sample_params(22)).unwrap();
        api::record_settled_units(&s, sid(11), owner(), 100).unwrap();
        assert!(
            !api::release_if_spent(&s, sid(11)).unwrap(),
            "settled units still to mine"
        );

        api::record_mined_units(&s, sid(11), owner(), 100).unwrap();
        assert!(api::release_if_spent(&s, sid(11)).unwrap());

        assert!(!api::series_exists(&s, sid(11)).unwrap());
        assert_eq!(api::total_series(&s).unwrap(), 1);
        assert_eq!(api::series_id_at(&s, 0).unwrap(), sid(22));
        assert_eq!(api::settled_units(&s, sid(11)).unwrap(), 0);
    });
}

/// Past its deadline a series' unpaid units wait for the group's expiry to return them,
/// so the series is spent only once the group handed it to the cleanup.
#[test]
fn a_forfeited_series_is_spent_once_its_group_returned_its_units() {
    with_registry(|s| {
        api::create_series(&s, sample_params(11)).unwrap();
        assert!(
            !api::release_if_spent(&s, sid(11)).unwrap(),
            "unpaid units are still live"
        );

        api::call_group_of(&s, sid(11), 1).unwrap();
        assert!(
            !api::release_if_spent(&s, sid(11)).unwrap(),
            "the group's expiry has not returned its units yet"
        );

        let group = api::read_series(&s, sid(11))
            .unwrap()
            .group_key()
            .expect("date step");
        IntexContract::new(s.clone())
            .hand_to_cleanup(group)
            .unwrap();
        assert!(api::release_if_spent(&s, sid(11)).unwrap());
    });
}

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

#[test]
fn intex_state_encoding_matches_solidity() {
    assert_eq!(IntexState::Issued as u8, 0);
    assert_eq!(IntexState::Called as u8, 1);
    assert_eq!(IntexState::Forfeited as u8, 2);
    assert_eq!(IntexState::from_u8(0).unwrap(), IntexState::Issued);
    assert_eq!(IntexState::from_u8(1).unwrap(), IntexState::Called);
    assert_eq!(IntexState::from_u8(2).unwrap(), IntexState::Forfeited);
    assert!(IntexState::from_u8(3).is_err());
}
