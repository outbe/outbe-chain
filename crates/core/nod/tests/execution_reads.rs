use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_nod::{api, precompile::INod, NodItemState};
use outbe_primitives::time::{first_full_day, WorldwideDay};
use outbe_primitives::{
    addresses::NOD_ADDRESS,
    block::{BlockContext, BlockLifecycle, BlockRuntimeContext},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

fn item(owner: Address, day: WorldwideDay) -> NodItemState {
    let nod_id = outbe_nod::identity::generate_nod_id(owner, day).unwrap();
    outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id,
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day: day,
            league_id: 4,
            bucket_key: outbe_nod::identity::bucket_key(day, U256::from(5), 978),
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 1_752_534_000,
        },
        U256::from(5),
    )
}

fn provider() -> HashMapStorageProvider {
    HashMapStorageProvider::new(1)
}

#[test]
fn same_block_issue_is_visible_to_point_and_list_reads() {
    let mut provider = provider();
    let body = item(Address::repeat_byte(0x21), WorldwideDay::new(20_260_716));

    StorageHandle::enter(&mut provider, |storage| {
        api::add_nod(
            &storage,
            &body,
            U256::from(5),
            outbe_nod::test_support::sealed_terms(&storage, &body, U256::from(5)),
        )
        .unwrap();
        assert_eq!(
            api::get_item(&storage, body.nod_id).unwrap().unwrap().owner,
            body.owner
        );
        assert_eq!(
            api::list_by_owner(&storage, body.owner)
                .unwrap()
                .into_iter()
                .map(|item| item.nod_id)
                .collect::<Vec<_>>(),
            [body.nod_id]
        );
        assert_eq!(api::list_all(&storage).unwrap().len(), 1);
    });

    let signatures: Vec<_> = provider
        .get_events(NOD_ADDRESS)
        .iter()
        .map(|event| event.topics()[0])
        .collect();
    assert_eq!(signatures, [INod::Transfer::SIGNATURE_HASH]);
}

#[test]
fn qualification_reads_the_finalized_day_and_writes_nothing() {
    let mut provider = provider();
    let body = item(Address::repeat_byte(0x31), WorldwideDay::new(20_260_716));
    StorageHandle::enter(&mut provider, |storage| {
        api::add_nod(
            &storage,
            &body,
            U256::from(5),
            outbe_nod::test_support::sealed_terms(&storage, &body, U256::from(5)),
        )
        .unwrap();
        let midnight = outbe_primitives::time::date_key_to_utc_timestamp(20260716);
        let context = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, midnight, 1),
            storage.clone(),
        );
        let mut oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(body.reference_currency);
        outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
        oracle
            .reference_currencies
            .push(body.reference_currency)
            .unwrap();
        oracle.config_is_initialized.write(true).unwrap();
        // The completed day's weighted price is (12*1 + 15*2)/3 = 14.
        // The new day's low sample is outside the half-open daily window.
        for (timestamp, rate, volume) in [
            (midnight - 200, 12, 1),
            (midnight - 100, 15, 2),
            (midnight, 1, 1),
        ] {
            oracle
                .write_snapshot(timestamp, &[(pair, U256::from(rate), U256::from(volume))])
                .unwrap();
        }
        let bucket_id = body.bucket_key;
        let bucket = || api::get_bucket(&storage, bucket_id).unwrap().unwrap();
        assert!(!api::is_qualified(&storage, &bucket()).unwrap());
        outbe_oracle::lifecycle::OracleLifecycle::begin_block(&context).unwrap();
        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair(20260715, oracle.pair_index_of(pair).unwrap())
                .unwrap(),
            Some(U256::from(14))
        );
        assert!(api::is_qualified(&storage, &bucket()).unwrap());
    });
    // Only issuance logged: qualifying writes nothing.
    let signatures: Vec<_> = provider
        .get_events(NOD_ADDRESS)
        .iter()
        .map(|event| event.topics()[0])
        .collect();
    assert_eq!(signatures, [INod::Transfer::SIGNATURE_HASH]);
}

#[test]
fn qualification_takes_only_own_currency_buckets_strictly_below_the_rate() {
    let mut provider = provider();
    let day = WorldwideDay::new(20_260_716);
    // (owner byte, reference currency, entry price) for floors 1000, 1200, 1250, 1300, 1300, 1299.
    let specs = [
        (0x51u8, 840u16, 926u64),
        (0x52, 978, 1112),
        (0x53, 840, 1158),
        (0x54, 840, 1204),
        (0x55, 978, 1204),
        // Shares the rate's bin, so it exercises the tail-bin exact compare.
        (0x56, 840, 1203),
    ];
    // Only the 840 buckets strictly below 1299 qualify: 1299/1300 are at or
    // above the day price and the 978 buckets are priced in another currency.
    let expected = [true, false, true, false, false, false];

    StorageHandle::enter(&mut provider, |storage| {
        let bucket_ids: Vec<B256> = specs
            .iter()
            .map(|&(owner_byte, currency, entry)| {
                let entry = U256::from(entry);
                let mut body = item(Address::repeat_byte(owner_byte), day);
                body.reference_currency = currency;
                outbe_nod::test_support::set_entry_price(&mut body, entry);
                outbe_nod::test_support::set_terms(&mut body);
                body.bucket_key = outbe_nod::identity::bucket_key(day, entry, currency);
                api::add_nod(
                    &storage,
                    &body,
                    entry,
                    outbe_nod::test_support::sealed_terms(&storage, &body, entry),
                )
                .unwrap();
                body.bucket_key
            })
            .collect();

        // COEN/840 closes the first full day at 1299. COEN/978 has no pair at all.
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(840);
        let index = outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
        let full_day = first_full_day(1_752_534_000);
        oracle
            .record_utc_day_vwap(full_day, index, U256::from(1299))
            .unwrap();
        oracle.utc_day_vwap_last_finalized.write(full_day).unwrap();

        let qualified: Vec<bool> = bucket_ids
            .iter()
            .map(|&bucket_id| {
                let bucket = api::get_bucket(&storage, bucket_id).unwrap().unwrap();
                api::is_qualified(&storage, &bucket).unwrap()
            })
            .collect();
        assert_eq!(qualified, expected);
    });
}

#[test]
fn removal_checks_loaded_state() {
    let mut provider = provider();
    let body = item(Address::repeat_byte(0x41), WorldwideDay::new(20_260_716));
    StorageHandle::enter(&mut provider, |storage| {
        api::add_nod(
            &storage,
            &body,
            U256::from(5),
            outbe_nod::test_support::sealed_terms(&storage, &body, U256::from(5)),
        )
        .unwrap();
        let loaded_item = api::load_item(&storage, body.nod_id).unwrap().unwrap();
        let bucket_id = body.bucket_key;
        let loaded_bucket = api::load_bucket(&storage, bucket_id).unwrap().unwrap();

        api::remove_nod(&storage, loaded_item, loaded_bucket).unwrap();

        assert!(api::get_item(&storage, body.nod_id).unwrap().is_none());
    });
}
