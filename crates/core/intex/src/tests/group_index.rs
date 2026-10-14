//! Two-level index: price bins hold `(reference currency, worldwide day)`
//! groups, and each group holds its series.

use crate::SeriesId;
use alloy_primitives::U256;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;

use crate::IntexContract;

const CHAIN_ID: u64 = 1;
const ISO: u16 = 840;
const CALL_PRICE: u64 = 2_000;

fn with_registry<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, f)
}

/// Same day, differing only in issuance currency - the members of one group.
fn sid(worldwide_day: u32, issuance: &[u8; 3]) -> SeriesId {
    SeriesId::pack(WorldwideDay::new(worldwide_day), *issuance, b'U').unwrap()
}

fn bin_count(f: &IntexContract<'_>, bin: u32) -> u32 {
    f.call_bin_count
        .read(&IntexContract::scoped(ISO, bin))
        .unwrap()
}

fn call_bin() -> u32 {
    IntexContract::price_to_bin(U256::from(CALL_PRICE)).unwrap()
}

#[test]
fn members_of_one_day_share_a_single_bin_entry() {
    with_registry(|s| {
        let mut f = IntexContract::new(s.clone());
        let price = U256::from(CALL_PRICE);
        f.insert_call_bin(sid(20260212, b"USD"), ISO, price)
            .unwrap();
        f.insert_call_bin(sid(20260212, b"EUR"), ISO, price)
            .unwrap();
        f.insert_call_bin(sid(20260212, b"TRY"), ISO, price)
            .unwrap();

        // Three series, one group: the bin holds days, not series.
        assert_eq!(bin_count(&f, call_bin()), 1);
        assert_eq!(
            f.call_bin_group_members(ISO, WorldwideDay::new(20260212))
                .unwrap(),
            vec![
                sid(20260212, b"USD"),
                sid(20260212, b"EUR"),
                sid(20260212, b"TRY")
            ]
        );
        assert_eq!(
            f.call_bin_groups(ISO, call_bin()).unwrap(),
            vec![WorldwideDay::new(20260212)]
        );
    });
}

#[test]
fn separate_days_are_separate_groups_in_one_bin() {
    with_registry(|s| {
        let mut f = IntexContract::new(s.clone());
        let price = U256::from(CALL_PRICE);
        f.insert_call_bin(sid(20260212, b"USD"), ISO, price)
            .unwrap();
        f.insert_call_bin(sid(20260213, b"USD"), ISO, price)
            .unwrap();

        assert_eq!(bin_count(&f, call_bin()), 2);
        assert_eq!(
            f.call_bin_groups(ISO, call_bin()).unwrap(),
            vec![WorldwideDay::new(20260212), WorldwideDay::new(20260213)]
        );
    });
}

#[test]
fn a_member_priced_into_another_bin_is_refused() {
    with_registry(|s| {
        let mut f = IntexContract::new(s.clone());
        f.insert_call_bin(sid(20260212, b"USD"), ISO, U256::from(CALL_PRICE))
            .unwrap();

        // One decision per group, so a second price for the same day is a split.
        let err = f
            .insert_call_bin(sid(20260212, b"EUR"), ISO, U256::from(CALL_PRICE * 4))
            .unwrap_err();
        assert!(
            err.to_string().contains("indexed in bin"),
            "unexpected error: {err}"
        );
    });
}

#[test]
fn removing_the_group_frees_its_bin_and_keeps_its_members() {
    with_registry(|s| {
        let mut f = IntexContract::new(s.clone());
        let price = U256::from(CALL_PRICE);
        f.insert_call_bin(sid(20260212, b"USD"), ISO, price)
            .unwrap();
        f.insert_call_bin(sid(20260212, b"EUR"), ISO, price)
            .unwrap();
        f.insert_call_bin(sid(20260213, b"USD"), ISO, price)
            .unwrap();

        f.remove_call_bin_group(ISO, WorldwideDay::new(20260212))
            .unwrap();

        // Swap-and-pop keeps the untouched day reachable.
        assert_eq!(bin_count(&f, call_bin()), 1);
        assert_eq!(
            f.call_bin_groups(ISO, call_bin()).unwrap(),
            vec![WorldwideDay::new(20260213)]
        );
        assert_eq!(
            f.call_bin_group_members(ISO, WorldwideDay::new(20260212))
                .unwrap(),
            vec![sid(20260212, b"USD"), sid(20260212, b"EUR")],
            "the members wait for the expiry sweep"
        );

        f.remove_call_bin_group(ISO, WorldwideDay::new(20260213))
            .unwrap();
        assert_eq!(bin_count(&f, call_bin()), 0);
        assert!(f.call_bin_groups(ISO, call_bin()).unwrap().is_empty());
    });
}

#[test]
fn removing_an_unindexed_group_is_a_no_op() {
    with_registry(|s| {
        let mut f = IntexContract::new(s.clone());
        f.insert_call_bin(sid(20260212, b"USD"), ISO, U256::from(CALL_PRICE))
            .unwrap();

        f.remove_call_bin_group(ISO, WorldwideDay::new(20260213))
            .unwrap();

        assert_eq!(bin_count(&f, call_bin()), 1);
        assert_eq!(
            f.call_bin_group_members(ISO, WorldwideDay::new(20260212))
                .unwrap(),
            vec![sid(20260212, b"USD")]
        );
    });
}

#[test]
fn the_currencies_stay_apart() {
    with_registry(|s| {
        let mut f = IntexContract::new(s.clone());
        let price = U256::from(CALL_PRICE);
        let series = sid(20260212, b"USD");
        f.insert_call_bin(series, ISO, price).unwrap();
        f.insert_call_bin(series, 978, price).unwrap();

        f.remove_call_bin_group(ISO, WorldwideDay::new(20260212))
            .unwrap();

        assert_eq!(bin_count(&f, call_bin()), 0);
        assert_eq!(
            f.call_bin_group_members(978, WorldwideDay::new(20260212))
                .unwrap(),
            vec![series]
        );
    });
}
