//! Realized units, their classes and expiry.

use super::*;

/// Create a series and drive it to `Called` so expiry can be exercised.
fn called_series(storage: &StorageHandle, worldwide_day: u32) -> SeriesId {
    api::create_series(storage, sample_params(worldwide_day)).unwrap();
    api::call_group_of(storage, sid(worldwide_day), ISSUED_AT).unwrap();
    sid(worldwide_day)
}

#[test]
fn expiry_forfeits_every_unrealized_unit() {
    with_registry(|s| {
        let id = called_series(&s, 40);
        assert_eq!(
            api::forfeitable_units(&s, &api::read_series(&s, id).unwrap())
                .unwrap()
                .units,
            100
        );
        assert_eq!(
            api::state(&s, &api::read_series(&s, id).unwrap()).unwrap(),
            IntexState::Called
        );
    });
}

#[test]
fn expiry_forfeits_only_what_was_left_unrealized() {
    with_registry(|s| {
        let id = called_series(&s, 41);
        api::record_settled_units(&s, id, 30).unwrap();
        api::record_gem_factory_units(&s, id, owner(), 25).unwrap();

        assert_eq!(api::settled_units(&s, id).unwrap(), 30);
        assert_eq!(api::gem_factory_units(&s, id).unwrap(), 25);
        assert_eq!(
            api::forfeitable_units(&s, &api::read_series(&s, id).unwrap())
                .unwrap()
                .units,
            45
        );
    });
}

#[test]
fn a_fully_realized_series_forfeits_nothing() {
    with_registry(|s| {
        let id = called_series(&s, 42);
        api::record_settled_units(&s, id, 60).unwrap();
        api::record_gem_factory_units(&s, id, owner(), 40).unwrap();

        assert_eq!(
            api::forfeitable_units(&s, &api::read_series(&s, id).unwrap())
                .unwrap()
                .units,
            0
        );
    });
}

#[test]
fn the_view_carries_the_whole_split() {
    with_registry(|s| {
        let id = called_series(&s, 46);
        api::record_settled_units(&s, id, 30).unwrap();
        api::record_exercised_units(&s, id, owner(), 5).unwrap();
        api::record_gem_factory_units(&s, id, owner(), 25).unwrap();

        // Every class the series record can answer for, without a second call.
        let data = dispatch_series_data(&s, id);
        assert_eq!(data.issuedUnits, 100);
        assert_eq!(data.settledUnits, 25);
        assert_eq!(data.exercisedUnits, 5);
        assert_eq!(data.gemFactoryUnits, 25);

        // The counters survive expiry, so the split stays readable afterwards.
        api::forfeitable_units(&s, &api::read_series(&s, id).unwrap()).unwrap();
        let data = dispatch_series_data(&s, id);
        assert_eq!(data.settledUnits, 25);
        assert_eq!(data.exercisedUnits, 5);
        assert_eq!(data.gemFactoryUnits, 25);
    });
}

fn dispatch_series_data(storage: &StorageHandle, series_id: SeriesId) -> IIntex::SeriesData {
    let call = IIntex::getIntexSeriesCall {
        seriesId: series_id.into(),
    };
    let out = dispatch(
        storage.clone(),
        &call.abi_encode(),
        Address::ZERO,
        U256::ZERO,
    )
    .unwrap();
    IIntex::getIntexSeriesCall::abi_decode_returns(&out).unwrap()
}

/// The registry with its clock at `now`; series still carry `ISSUED_AT`.
fn with_registry_at<R>(now: u64, f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(now));
    StorageHandle::enter(&mut storage, f)
}

const NOTICE_END: u64 = ISSUED_AT as u64 + CALL_NOTICE_PERIOD as u64;

#[test]
fn series_data_reports_the_settlement_deadline_once_called() {
    with_registry_at(NOTICE_END, |s| {
        api::create_series(&s, sample_params(62)).unwrap();
        assert_eq!(dispatch_series_data(&s, sid(62)).settlementDeadline, 0);

        let id = called_series(&s, 63);
        assert_eq!(dispatch_series_data(&s, id).settlementDeadline, NOTICE_END);
    });
}

#[test]
fn a_called_series_reads_expired_from_its_deadline_not_from_the_sweep() {
    // Strictly after: on the deadline itself the notice has not expired.
    with_registry_at(NOTICE_END, |s| {
        let id = called_series(&s, 60);
        assert_eq!(dispatch_series_data(&s, id).state, IntexState::Called as u8);
    });

    with_registry_at(NOTICE_END + 1, |s| {
        let id = called_series(&s, 61);
        assert_eq!(
            dispatch_series_data(&s, id).state,
            IntexState::Expired as u8
        );
    });
}

#[test]
fn realized_units_can_never_exceed_the_issued_count() {
    with_registry(|s| {
        let id = called_series(&s, 45);
        api::record_settled_units(&s, id, 100).unwrap();
        // One unit past the cap means the two ledgers disagree. The forfeit
        // arithmetic would underflow later, so this call refuses it here instead.
        assert!(api::record_gem_factory_units(&s, id, owner(), 1).is_err());
        assert_eq!(
            api::forfeitable_units(&s, &api::read_series(&s, id).unwrap())
                .unwrap()
                .units,
            0
        );
    });
}

#[test]
fn the_unit_classes_are_disjoint_and_sum_to_the_issued_count() {
    with_registry(|s| {
        let id = called_series(&s, 50);
        api::record_settled_units(&s, id, 30).unwrap();
        api::record_gem_factory_units(&s, id, owner(), 25).unwrap();
        api::record_exercised_units(&s, id, owner(), 15).unwrap();

        let counts = api::unit_counts(&s, id).unwrap();
        assert_eq!(counts.issued, 100);
        assert_eq!(counts.active, 45);
        assert_eq!(counts.settled, 15);
        assert_eq!(counts.exercised, 15);
        assert_eq!(counts.gem_factory, 25);
        assert_eq!(counts.forfeited, 0);
        assert_eq!(
            counts.active
                + counts.settled
                + counts.exercised
                + counts.gem_factory
                + counts.forfeited,
            counts.issued
        );
    });
}

#[test]
fn expiry_moves_the_active_units_into_forfeited() {
    with_registry_at(NOTICE_END + 1, |s| {
        let id = called_series(&s, 51);
        api::record_settled_units(&s, id, 30).unwrap();
        api::record_gem_factory_units(&s, id, owner(), 25).unwrap();
        api::record_exercised_units(&s, id, owner(), 30).unwrap();

        let counts = api::unit_counts(&s, id).unwrap();
        assert_eq!(counts.active, 0);
        assert_eq!(counts.forfeited, 45);
        assert_eq!(counts.settled, 0);
        assert_eq!(counts.exercised, 30);
        assert_eq!(
            counts.active
                + counts.settled
                + counts.exercised
                + counts.gem_factory
                + counts.forfeited,
            counts.issued
        );
    });
}

#[test]
fn settled_units_alone_count_nothing_as_exercised() {
    with_registry(|s| {
        let id = called_series(&s, 52);
        api::record_settled_units(&s, id, 40).unwrap();

        let counts = api::unit_counts(&s, id).unwrap();
        assert_eq!(counts.exercised, 0);
        assert_eq!(counts.settled, 40);
        assert_eq!(counts.active, 60);
    });
}

#[test]
fn exercising_more_than_was_settled_is_refused() {
    with_registry(|s| {
        let id = called_series(&s, 53);
        api::record_settled_units(&s, id, 10).unwrap();

        assert!(api::record_exercised_units(&s, id, owner(), 11).is_err());
        api::record_exercised_units(&s, id, owner(), 10).unwrap();
        assert!(api::record_exercised_units(&s, id, owner(), 1).is_err());
        assert_eq!(api::exercised_units(&s, id).unwrap(), 10);
    });
}

#[test]
fn exercising_moves_units_out_of_the_settled_ledger() {
    with_registry(|s| {
        let id = called_series(&s, 54);
        api::record_settled_units(&s, id, 40).unwrap();
        api::record_exercised_units(&s, id, owner(), 15).unwrap();

        // The ledger holds what is still settled, not what was ever paid.
        assert_eq!(api::settled_units(&s, id).unwrap(), 25);
        assert_eq!(api::exercised_units(&s, id).unwrap(), 15);
    });
}

/// The worked example of the spec: 100 issued, 20 settled, 10 exercised and 15 sent
/// to the Gem Factory leave 55 unpaid units to forfeit.
#[test]
fn the_unpaid_remainder_excludes_settled_exercised_and_gem_factory_units() {
    with_registry(|s| {
        let id = called_series(&s, 55);
        api::record_settled_units(&s, id, 30).unwrap();
        api::record_exercised_units(&s, id, owner(), 10).unwrap();
        api::record_gem_factory_units(&s, id, owner(), 15).unwrap();

        let counts = api::unit_counts(&s, id).unwrap();
        assert_eq!(counts.settled, 20);
        assert_eq!(counts.exercised, 10);
        assert_eq!(counts.gem_factory, 15);
        assert_eq!(counts.active, 55);

        assert_eq!(
            api::forfeitable_units(&s, &api::read_series(&s, id).unwrap())
                .unwrap()
                .units,
            55
        );
    });
}

/// Burning erases who held the units, so the two per-owner ledgers are written at
/// the moment of the burn. Nothing reads them on chain yet. A later reader cannot
/// reconstruct them.
#[test]
fn the_per_owner_ledgers_record_who_burned_the_units() {
    with_registry(|s| {
        let other = Address::repeat_byte(0xB2);
        let id = called_series(&s, 56);
        api::record_settled_units(&s, id, 40).unwrap();
        api::record_exercised_units(&s, id, owner(), 10).unwrap();
        api::record_gem_factory_units(&s, id, other, 5).unwrap();

        assert_eq!(api::owner_exercised_units(&s, id, owner()).unwrap(), 10);
        assert_eq!(api::owner_gem_factory_units(&s, id, owner()).unwrap(), 0);
        assert_eq!(api::owner_exercised_units(&s, id, other).unwrap(), 0);
        assert_eq!(api::owner_gem_factory_units(&s, id, other).unwrap(), 5);

        // Each owner's share adds up to the series totals.
        assert_eq!(api::exercised_units(&s, id).unwrap(), 10);
        assert_eq!(api::gem_factory_units(&s, id).unwrap(), 5);
    });
}
