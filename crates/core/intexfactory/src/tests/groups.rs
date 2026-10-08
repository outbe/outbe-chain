mod group_index {
    //! Two-level index: price bins hold `(reference currency, worldwide day)`
    //! groups, and each group holds its series.

    use alloy_primitives::U256;
    use outbe_intex::SeriesId;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;
    use outbe_primitives::time::WorldwideDay;

    use crate::schema::IntexFactoryContract;

    const CHAIN_ID: u64 = 1;
    const ISO: u16 = 840;
    const CALL_PRICE: u64 = 2_000;

    fn with_factory<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        StorageHandle::enter(&mut storage, |handle| {
            crate::tests::select_prod_profile(&handle);
            f(handle)
        })
    }

    /// Same day, differing only in issuance currency - the members of one group.
    fn sid(worldwide_day: u32, issuance: &[u8; 3]) -> SeriesId {
        SeriesId::pack(WorldwideDay::new(worldwide_day), *issuance, b'U').unwrap()
    }

    fn bin_count(f: &IntexFactoryContract<'_>, bin: u32) -> u32 {
        f.call_bin_count
            .read(&IntexFactoryContract::scoped(ISO, bin))
            .unwrap()
    }

    fn call_bin() -> u32 {
        IntexFactoryContract::price_to_bin(U256::from(CALL_PRICE)).unwrap()
    }

    #[test]
    fn members_of_one_day_share_a_single_bin_entry() {
        with_factory(|s| {
            let mut f = IntexFactoryContract::new(s.clone());
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
        with_factory(|s| {
            let mut f = IntexFactoryContract::new(s.clone());
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
        with_factory(|s| {
            let mut f = IntexFactoryContract::new(s.clone());
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
        with_factory(|s| {
            let mut f = IntexFactoryContract::new(s.clone());
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
        with_factory(|s| {
            let mut f = IntexFactoryContract::new(s.clone());
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
        with_factory(|s| {
            let mut f = IntexFactoryContract::new(s.clone());
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
}

mod group_scans {
    //! Group transitions: one day in one reference currency decides once and moves
    //! all of its series together.

    use alloy_primitives::U256;
    use outbe_intex::SeriesId;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;
    use outbe_primitives::time::WorldwideDay;

    use crate::runtime;
    use crate::schema::{IntexFactoryContract, IssuanceParams};

    const CHAIN_ID: u64 = 1;
    const REFERENCE_ISO: u16 = 840;
    const ISSUED_AT: u32 = 1_700_000_000;
    const ENTRY_PRICE: u64 = 1_000_000;
    const EXPECTED_TRIGGER: u64 = 2_280_000;
    const WWD: u32 = 20260212;

    /// Issuance currencies of one day's series. They share every decision input.
    const ISSUANCES: [u16; 3] = [840, 978, 392];

    fn with_factory<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        storage.set_timestamp(U256::from(ISSUED_AT as u64));
        storage.stub_sub_call_at(
            crate::constants::INTEX_NFT1155_ADDRESS,
            alloy_primitives::Bytes::from(vec![0u8; 32]),
        );
        storage.stub_sub_call_at(
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            alloy_primitives::Bytes::from(vec![0u8; 32]),
        );
        StorageHandle::enter(&mut storage, |handle| {
            crate::tests::select_prod_profile(&handle);
            f(handle)
        })
    }

    fn day() -> WorldwideDay {
        WorldwideDay::new(WWD)
    }

    fn params(worldwide_day: u32, issuance_currency: u16) -> IssuanceParams {
        IssuanceParams {
            series_id: SeriesId::for_pair(
                WorldwideDay::new(worldwide_day),
                issuance_currency,
                REFERENCE_ISO,
            )
            .unwrap(),
            worldwide_day: worldwide_day.into(),
            issued_units: 100,
            promis_load_minor: 1_000_000_000_000_000_000,
            entry_price_minor: U256::from(ENTRY_PRICE),
            issuance_currency,
            reference_currency: REFERENCE_ISO,
            recipients: vec![],
            units: vec![],
            recipient_chains: vec![],
            snapshot_chains: vec![1],
        }
    }

    /// Issue one day's series, one per issuance currency.
    fn issue_day(s: &StorageHandle<'_>, worldwide_day: u32) {
        for issuance in ISSUANCES {
            runtime::issue(s, params(worldwide_day, issuance)).unwrap();
        }
    }

    #[test]
    fn a_days_series_share_one_call_group() {
        with_factory(|s| {
            issue_day(&s, WWD);
            let f = IntexFactoryContract::new(s.clone());
            let members = f.call_bin_group_members(REFERENCE_ISO, day()).unwrap();
            assert_eq!(members.len(), ISSUANCES.len());
            let bin = IntexFactoryContract::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
            assert_eq!(f.call_bin_groups(REFERENCE_ISO, bin).unwrap(), vec![day()]);
        });
    }
}
