//! Two-level index: price bins hold `(reference currency, worldwide day)`
//! groups, and each group holds its series.

use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_intex::SeriesId;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::schema::IntexFactoryContract;

const CHAIN_ID: u64 = 1;
const ISO: u16 = 840;
const FLOOR: u64 = 2_000;

fn with_factory<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, f)
}

/// Same day, differing only in issuance currency — the members of one group.
fn sid(worldwide_day: u32, issuance: &[u8; 3]) -> SeriesId {
    SeriesId::pack(WorldwideDay::new(worldwide_day), *issuance, b'U').unwrap()
}

fn bin_count(f: &IntexFactoryContract<'_>, bin: u32) -> u32 {
    f.unqualified_bin_count
        .read(&IntexFactoryContract::scoped(ISO, bin))
        .unwrap()
}

fn floor_bin() -> u32 {
    IntexFactoryContract::price_to_bin(U256::from(FLOOR)).unwrap()
}

#[test]
fn members_of_one_day_share_a_single_bin_entry() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        let floor = U256::from(FLOOR);
        f.insert_unqualified(sid(20260212, b"USD"), ISO, floor)
            .unwrap();
        f.insert_unqualified(sid(20260212, b"EUR"), ISO, floor)
            .unwrap();
        f.insert_unqualified(sid(20260212, b"TRY"), ISO, floor)
            .unwrap();

        // Three series, one group: the bin holds days, not series.
        assert_eq!(bin_count(&f, floor_bin()), 1);
        assert_eq!(
            f.unqualified_group_members(ISO, WorldwideDay::new(20260212))
                .unwrap(),
            vec![
                sid(20260212, b"USD"),
                sid(20260212, b"EUR"),
                sid(20260212, b"TRY")
            ]
        );
        assert_eq!(
            f.unqualified_groups_in_bin(ISO, floor_bin()).unwrap(),
            vec![WorldwideDay::new(20260212)]
        );
    });
}

#[test]
fn separate_days_are_separate_groups_in_one_bin() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        let floor = U256::from(FLOOR);
        f.insert_unqualified(sid(20260212, b"USD"), ISO, floor)
            .unwrap();
        f.insert_unqualified(sid(20260213, b"USD"), ISO, floor)
            .unwrap();

        assert_eq!(bin_count(&f, floor_bin()), 2);
        assert_eq!(
            f.unqualified_groups_in_bin(ISO, floor_bin()).unwrap(),
            vec![WorldwideDay::new(20260212), WorldwideDay::new(20260213)]
        );
    });
}

#[test]
fn a_member_priced_into_another_bin_is_refused() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        f.insert_unqualified(sid(20260212, b"USD"), ISO, U256::from(FLOOR))
            .unwrap();

        // One decision per group, so a second price for the same day is a split.
        let err = f
            .insert_unqualified(sid(20260212, b"EUR"), ISO, U256::from(FLOOR * 4))
            .unwrap_err();
        assert!(
            err.to_string().contains("indexed in bin"),
            "unexpected error: {err}"
        );
    });
}

#[test]
fn the_last_member_takes_the_group_out_of_the_bin() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        let floor = U256::from(FLOOR);
        f.insert_unqualified(sid(20260212, b"USD"), ISO, floor)
            .unwrap();
        f.insert_unqualified(sid(20260212, b"EUR"), ISO, floor)
            .unwrap();

        // Swap-and-pop leaves the surviving member reachable.
        f.remove_unqualified(sid(20260212, b"USD"), ISO).unwrap();
        assert_eq!(bin_count(&f, floor_bin()), 1);
        assert_eq!(
            f.unqualified_group_members(ISO, WorldwideDay::new(20260212))
                .unwrap(),
            vec![sid(20260212, b"EUR")]
        );

        f.remove_unqualified(sid(20260212, b"EUR"), ISO).unwrap();
        assert_eq!(bin_count(&f, floor_bin()), 0);
        assert!(f
            .unqualified_group_members(ISO, WorldwideDay::new(20260212))
            .unwrap()
            .is_empty());
        assert!(f
            .unqualified_groups_in_bin(ISO, floor_bin())
            .unwrap()
            .is_empty());
    });
}

#[test]
fn removing_an_absent_series_is_a_no_op() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        f.insert_unqualified(sid(20260212, b"USD"), ISO, U256::from(FLOOR))
            .unwrap();

        f.remove_unqualified(sid(20260212, b"EUR"), ISO).unwrap();
        f.remove_unqualified(sid(20260213, b"USD"), ISO).unwrap();

        assert_eq!(bin_count(&f, floor_bin()), 1);
        assert_eq!(
            f.unqualified_group_members(ISO, WorldwideDay::new(20260212))
                .unwrap(),
            vec![sid(20260212, b"USD")]
        );
    });
}

#[test]
fn the_two_indexes_and_the_currencies_stay_apart() {
    with_factory(|s| {
        let mut f = IntexFactoryContract::new(s.clone());
        let floor = U256::from(FLOOR);
        let series = sid(20260212, b"USD");
        f.insert_unqualified(series, ISO, floor).unwrap();
        f.insert_qualified(series, ISO, floor).unwrap();
        f.insert_unqualified(series, 978, floor).unwrap();

        f.remove_unqualified(series, ISO).unwrap();

        assert_eq!(bin_count(&f, floor_bin()), 0);
        assert_eq!(
            f.qualified_group_members(ISO, WorldwideDay::new(20260212))
                .unwrap(),
            vec![series]
        );
        assert_eq!(
            f.unqualified_group_members(978, WorldwideDay::new(20260212))
                .unwrap(),
            vec![series]
        );
    });
}
