mod group_scans {
    //! Group transitions: one day in one reference currency decides once and moves
    //! all of its series together.

    use alloy_primitives::U256;
    use outbe_intex::SeriesId;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;
    use outbe_primitives::time::WorldwideDay;

    use crate::runtime;
    use crate::schema::IssuanceParams;
    use outbe_intex::IntexContract;

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
            call_price_minor: U256::from(EXPECTED_TRIGGER),
            call_trigger: crate::tests::prod_trigger(),
            issuance_currency,
            reference_currency: REFERENCE_ISO,
            snapshot_chains: vec![1],
        }
    }

    /// Issue one day's series, one per issuance currency.
    fn issue_day(s: &StorageHandle<'_>, worldwide_day: u32) {
        for issuance in ISSUANCES {
            runtime::issue_intex(s, params(worldwide_day, issuance)).unwrap();
        }
    }

    #[test]
    fn a_days_series_share_one_call_group() {
        with_factory(|s| {
            issue_day(&s, WWD);
            let f = IntexContract::new(s.clone());
            let members = f.call_bin_group_members(REFERENCE_ISO, day()).unwrap();
            assert_eq!(members.len(), ISSUANCES.len());
            let bin =
                outbe_primitives::call_bins::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
            assert_eq!(f.call_bin_groups(REFERENCE_ISO, bin).unwrap(), vec![day()]);
        });
    }
}
