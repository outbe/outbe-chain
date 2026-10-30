mod group_scans {
    //! Group transitions: one day in one reference currency decides once and moves
    //! all of its series together.

    use alloy_primitives::U256;
    use outbe_intex::SeriesId;
    use outbe_primitives::storage::StorageHandle;

    use crate::runtime;
    use crate::schema::IssuanceParams;
    use crate::tests::group_of;
    use outbe_intex::IntexContract;

    const REFERENCE_ISO: u16 = 840;
    const ENTRY_PRICE: u64 = 1_000_000;
    const EXPECTED_TRIGGER: u64 = 2_280_000;
    const WWD: u32 = 20260212;

    /// Issuance currencies of one day's series. They share every decision input.
    const ISSUANCES: [u16; 3] = [840, 978, 392];

    use super::super::with_factory;

    fn params(worldwide_day: u32, issuance_currency: u16) -> IssuanceParams {
        IssuanceParams {
            series_id: SeriesId::for_pair(worldwide_day, issuance_currency, REFERENCE_ISO).unwrap(),
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
            let ids: Vec<_> = ISSUANCES
                .iter()
                .map(|issuance| params(WWD, *issuance).series_id)
                .collect();
            let group = group_of(&s, ids[0]);
            assert_eq!(f.group_members(group).unwrap(), ids);
            let bin =
                outbe_primitives::call_bins::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
            assert_eq!(f.call_bin_groups(REFERENCE_ISO, bin).unwrap(), vec![group]);
        });
    }
}
