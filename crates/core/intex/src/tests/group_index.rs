//! Call groups: price bins hold groups, and each group holds the series that count
//! from the same day against the same call price in the same currency.

use alloy_primitives::U256;

use super::*;

fn bin_count(f: &IntexContract<'_>, bin: u32) -> u32 {
    f.call_bin_count
        .read(&IntexContract::scoped(840, bin))
        .unwrap()
}

fn call_bin(price: u64) -> u32 {
    outbe_primitives::call_bins::price_to_bin(U256::from(price)).unwrap()
}

fn issue(s: &StorageHandle, day: u32, issued_at: u32, call_price: u64) -> SeriesId {
    let mut params = sample_params(day);
    params.issued_at = issued_at;
    params.call_price_minor = U256::from(call_price);
    api::create_series(s, params).unwrap();
    sid(day)
}

fn group_of(s: &StorageHandle, id: SeriesId) -> alloy_primitives::B256 {
    api::read_series(s, id)
        .unwrap()
        .group_key()
        .expect("date step")
}

#[test]
fn series_sharing_their_terms_share_one_bin_entry() {
    with_registry(|s| {
        let a = issue(&s, 1, ISSUED_AT, CALL_PRICE_MINOR);
        let b = issue(&s, 2, ISSUED_AT + 60, CALL_PRICE_MINOR);
        let f = IntexContract::new(s.clone());

        assert_eq!(group_of(&s, a), group_of(&s, b));
        assert_eq!(bin_count(&f, call_bin(CALL_PRICE_MINOR)), 1);
        assert_eq!(f.group_members(group_of(&s, a)).unwrap(), vec![a, b]);
        assert_eq!(
            f.call_bin_groups(840, call_bin(CALL_PRICE_MINOR)).unwrap(),
            vec![group_of(&s, a)]
        );
    });
}

#[test]
fn another_call_price_or_first_full_day_opens_another_group() {
    with_registry(|s| {
        let day = u64::from(SECONDS_PER_DAY_U32);
        let midnight = u32::try_from(u64::from(ISSUED_AT).div_ceil(day) * day).unwrap();
        let first = issue(&s, 1, midnight, CALL_PRICE_MINOR);
        let later = issue(&s, 2, midnight + 1, CALL_PRICE_MINOR);
        let pricier = issue(&s, 3, midnight + 1, CALL_PRICE_MINOR * 4);

        assert_ne!(group_of(&s, first), group_of(&s, later));
        assert_ne!(group_of(&s, later), group_of(&s, pricier));
    });
}

#[test]
fn the_last_member_to_leave_closes_the_group() {
    with_registry(|s| {
        let a = issue(&s, 1, ISSUED_AT, CALL_PRICE_MINOR);
        let b = issue(&s, 2, ISSUED_AT, CALL_PRICE_MINOR);
        let group = group_of(&s, a);
        let mut f = IntexContract::new(s.clone());

        f.leave_call_group(group, a).unwrap();
        assert_eq!(f.group_members(group).unwrap(), vec![b]);
        assert_eq!(bin_count(&f, call_bin(CALL_PRICE_MINOR)), 1);

        f.leave_call_group(group, b).unwrap();
        assert_eq!(f.group_member_count(group).unwrap(), 0);
        assert_eq!(bin_count(&f, call_bin(CALL_PRICE_MINOR)), 0);
        assert_eq!(f.group_currency(group).unwrap(), 0);
    });
}

#[test]
fn a_called_group_leaves_its_bin_and_calls_every_member() {
    with_registry(|s| {
        let a = issue(&s, 1, ISSUED_AT, CALL_PRICE_MINOR);
        let b = issue(&s, 2, ISSUED_AT, CALL_PRICE_MINOR);
        api::call_group_of(&s, a, ISSUED_AT + 10).unwrap();
        let f = IntexContract::new(s.clone());

        assert_eq!(bin_count(&f, call_bin(CALL_PRICE_MINOR)), 0);
        for id in [a, b] {
            let record = api::read_series(&s, id).unwrap();
            assert_eq!(
                api::call_stamp(&s, &record).unwrap().called_at,
                ISSUED_AT + 10
            );
            assert_eq!(api::state(&s, &record).unwrap(), IntexState::Called);
        }
        assert!(
            api::call_group_of(&s, b, ISSUED_AT + 20).is_err(),
            "a group is called once"
        );
    });
}

#[test]
fn the_currencies_stay_apart() {
    with_registry(|s| {
        let usd = issue(&s, 1, ISSUED_AT, CALL_PRICE_MINOR);
        let mut params = sample_params(2);
        params.reference_currency = 978;
        api::create_series(&s, params).unwrap();

        assert_ne!(group_of(&s, usd), group_of(&s, sid(2)));
    });
}
