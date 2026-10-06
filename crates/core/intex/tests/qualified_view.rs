use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_intex::{
    api,
    precompile::{dispatch, IIntex},
    schema::{CreateSeriesParams, IntexCallTrigger, IntexState},
    SeriesId,
};
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{
    address_pair::AddressPair,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::{first_full_day, next_date_key, timestamp_to_date_key, WorldwideDay},
};

const ISSUED: u32 = 1_700_000_000;
const FLOOR: u64 = 1_500_000;
fn issue(s: &StorageHandle<'_>) -> SeriesId {
    let day = WorldwideDay::new(timestamp_to_date_key(u64::from(ISSUED)));
    let id = SeriesId::pack(day, *b"USD", b'U').unwrap();
    api::create_series(
        s,
        CreateSeriesParams {
            series_id: id,
            worldwide_day: day,
            issued_units: 10,
            promis_load_minor: 1_000_000,
            entry_price_minor: U256::from(1_000_000),
            floor_price_minor: U256::from(FLOOR),
            call_price_minor: U256::from(2_280_000),
            call_trigger: IntexCallTrigger {
                call_window_seconds: 30 * 86_400,
                call_threshold_seconds: 5 * 86_400,
                call_notice_period_seconds: 7 * 86_400,
            },
            issued_at: ISSUED,
            issuance_currency: 978,
            reference_currency: 840,
        },
    )
    .unwrap();
    id
}
fn close(s: &StorageHandle<'_>, day: u32, iso: u16, price: u64) {
    let oracle = OracleContract::new(s.clone());
    let index = oracle.pair_index_of(AddressPair::new_coen_to(iso)).unwrap();
    let index = if index == 0 {
        outbe_oracle::api::register_pair(s.clone(), AddressPair::new_coen_to(iso)).unwrap()
    } else {
        index
    };
    oracle
        .record_utc_day_vwap(day, index, U256::from(price))
        .unwrap();
    oracle.utc_day_vwap_last_finalized.write(day).unwrap();
}
fn state(s: &StorageHandle<'_>, id: SeriesId) -> u8 {
    let before = api::read_series(s, id).unwrap();
    let result = dispatch(
        s.clone(),
        &IIntex::seriesDataCall {
            seriesId: id.into(),
        }
        .abi_encode(),
        Address::ZERO,
        U256::ZERO,
    )
    .unwrap();
    let data = IIntex::seriesDataCall::abi_decode_returns(&result).unwrap();
    assert_eq!(
        api::read_series(s, id).unwrap(),
        before,
        "view must not write a qualification state"
    );
    data.state
}

#[test]
fn qualified_intex_view_uses_the_permanent_full_day_reference_predicate() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(ISSUED));
    StorageHandle::enter(&mut provider, |s| {
        let id = issue(&s);
        assert_eq!(state(&s, id), 0);
        let first = first_full_day(u64::from(ISSUED));
        close(&s, timestamp_to_date_key(u64::from(ISSUED)), 840, FLOOR + 1);
        assert_eq!(state(&s, id), 0);
        close(&s, first, 978, FLOOR + 100);
        assert_eq!(state(&s, id), 0);
        close(&s, first, 840, FLOOR);
        assert_eq!(state(&s, id), 0);
        let second = next_date_key(first);
        close(&s, second, 840, FLOOR + 1);
        assert_eq!(state(&s, id), 1);
        close(&s, next_date_key(second), 840, FLOOR - 1);
        assert_eq!(state(&s, id), 1);
        assert_eq!(
            api::read_series(&s, id)
                .unwrap()
                .effective_state(u64::from(ISSUED))
                .unwrap(),
            IntexState::Issued
        );
    });
}

#[test]
fn qualified_intex_projection_preserves_called_deadline_and_internal_issued_routing() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(ISSUED));
    let id = StorageHandle::enter(&mut provider, |s| {
        let id = issue(&s);
        close(&s, first_full_day(u64::from(ISSUED)), 840, FLOOR + 1);
        assert_eq!(state(&s, id), 1);
        assert_eq!(
            api::read_series(&s, id)
                .unwrap()
                .effective_state(u64::from(ISSUED))
                .unwrap(),
            IntexState::Issued
        );
        api::mark_called(&s, id, ISSUED).unwrap();
        id
    });
    let deadline = u64::from(ISSUED) + 7 * 86_400;
    provider.set_timestamp(U256::from(deadline));
    StorageHandle::enter(&mut provider, |s| assert_eq!(state(&s, id), 2));
    provider.set_timestamp(U256::from(deadline + 1));
    StorageHandle::enter(&mut provider, |s| assert_eq!(state(&s, id), 3));
}
