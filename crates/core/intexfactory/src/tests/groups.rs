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
    const DAY: u32 = 20260212;

    /// Issuance currencies of one day's series. They share every decision input.
    const ISSUANCES: [u16; 3] = [840, 978, 392];

    fn params(emission_day: u32, issuance_currency: u16) -> IssuanceParams {
        IssuanceParams {
            series_id: SeriesId::for_pair(emission_day, issuance_currency, REFERENCE_ISO).unwrap(),
            emission_day: emission_day.into(),
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
    fn issue_day(s: &StorageHandle<'_>, emission_day: u32) {
        for issuance in ISSUANCES {
            runtime::issue_intex(s, params(emission_day, issuance)).unwrap();
        }
    }

    #[test]
    fn a_days_series_share_one_call_group() {
        use alloy_sol_types::SolEvent;
        use outbe_intex::precompile::IIntex;
        use outbe_system_common::addresses::INTEX_ADDRESS;

        let mut storage = super::super::factory_provider();
        let ids: Vec<_> = ISSUANCES
            .iter()
            .map(|issuance| params(DAY, *issuance).series_id)
            .collect();
        let scan_ts = super::super::ISSUED_AT + 60 * super::super::DAY;
        let last_closed_day = outbe_system_common::time::previous_date_key(
            outbe_system_common::time::timestamp_to_date_key(scan_ts),
        )
        .expect("date step");

        StorageHandle::enter(&mut storage, |s| {
            super::super::select_prod_profile(&s);
            issue_day(&s, DAY);
            let mut f = IntexContract::new(s.clone());
            let group = group_of(&s, ids[0]);
            assert_eq!(f.group_members(group).unwrap(), ids);
            let bin =
                outbe_system_common::call_bins::price_to_bin(U256::from(EXPECTED_TRIGGER)).unwrap();
            assert_eq!(f.call_bin_groups(REFERENCE_ISO, bin).unwrap(), vec![group]);

            let oracle = outbe_oracle::schema::OracleContract::new(s.clone());
            let pair = outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO);
            oracle.pair_to_index.write(&pair, 1).unwrap();
            let mut cur = last_closed_day;
            for _ in 0..30 {
                oracle
                    .record_utc_day_vwap(cur, 1, U256::from(EXPECTED_TRIGGER) + U256::ONE)
                    .unwrap();
                cur = outbe_system_common::time::previous_date_key(cur).expect("date step");
            }
            assert_eq!(
                super::super::call_group(&s, &mut f, group, last_closed_day, scan_ts),
                3
            );
            let deadline = scan_ts + u64::from(super::super::CALL_NOTICE_PERIOD);
            let due = IntexContract::day_end(IntexContract::deadline_day(deadline));
            let ctx = outbe_primitives::block::BlockRuntimeContext::new(
                outbe_primitives::block::BlockContext {
                    block_gas_limit: 0,
                    user_gas_used: 0,
                    block_number: 2,
                    timestamp: due,
                    chain_id: super::super::CHAIN_ID,
                    genesis_hash: alloy_primitives::B256::ZERO,
                    genesis_timestamp: scan_ts - super::super::DAY,
                    proposer: alloy_primitives::Address::ZERO,
                    validators: Vec::new(),
                },
                s.clone(),
            );
            super::super::called::daily_pass(&ctx).unwrap();
        });

        let called: Vec<_> = storage
            .get_events(INTEX_ADDRESS)
            .iter()
            .filter_map(|log| IIntex::IntexSeriesCalled::decode_log_data(log).ok())
            .map(|e| (e.seriesId, e.issuanceCurrency, e.referenceCurrency))
            .collect();
        let closed: Vec<_> = storage
            .get_events(INTEX_ADDRESS)
            .iter()
            .filter_map(|log| IIntex::IntexSeriesClosed::decode_log_data(log).ok())
            .map(|e| {
                (
                    e.seriesId,
                    e.issuanceCurrency,
                    e.referenceCurrency,
                    e.forfeitedCount,
                )
            })
            .collect();
        assert_eq!(
            called,
            ids.iter()
                .zip(ISSUANCES)
                .map(|(id, iso)| ((*id).into(), iso, REFERENCE_ISO))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            closed,
            ids.iter()
                .zip(ISSUANCES)
                .map(|(id, iso)| ((*id).into(), iso, REFERENCE_ISO, 100))
                .collect::<Vec<_>>()
        );
    }

    /// Every unit settled and mined before the deadline still closes at `(0, 0)`.
    #[test]
    fn a_fully_settled_series_closes_with_zero_forfeit() {
        use alloy_sol_types::SolEvent;
        use outbe_intex::precompile::IIntex;
        use outbe_system_common::addresses::INTEX_ADDRESS;
        use outbe_system_common::pass_budget::PassBudget;

        let mut storage = super::super::factory_provider();
        let id = super::super::sid(7);
        StorageHandle::enter(&mut storage, |s| {
            super::super::select_prod_profile(&s);
            runtime::issue_intex(&s, super::super::sample(7)).unwrap();
            outbe_intex::api::call_group_of(&s, id, super::super::ISSUED_AT).unwrap();
            outbe_intex::api::record_settled_units(&s, id, super::super::owner(), 100).unwrap();
            outbe_intex::api::record_mined_units(&s, id, super::super::owner(), 100).unwrap();
            assert!(
                !outbe_intex::api::release_if_spent(&s, id).unwrap(),
                "the called series stays until forfeit"
            );

            let record = outbe_intex::api::get_series(&s, id).unwrap();
            let close_at = outbe_intex::api::call_stamp(&s, &record).unwrap().close_at;
            let due = IntexContract::day_end(IntexContract::deadline_day(close_at));
            let ctx = outbe_primitives::block::BlockRuntimeContext::new(
                outbe_primitives::block::BlockContext {
                    block_gas_limit: 0,
                    user_gas_used: 0,
                    block_number: 2,
                    timestamp: due,
                    chain_id: super::super::CHAIN_ID,
                    genesis_hash: alloy_primitives::B256::ZERO,
                    genesis_timestamp: u64::max(due, 1),
                    proposer: alloy_primitives::Address::ZERO,
                    validators: Vec::new(),
                },
                s.clone(),
            );
            let mut budget = PassBudget::new(100, 100);
            crate::forfeited::forfeit_through(&ctx, IntexContract::deadline_day(due), &mut budget)
                .unwrap();
            crate::cleanup::cleanup(&ctx).unwrap();
            assert!(!outbe_intex::api::series_exists(&s, id).unwrap());
        });

        let closed: Vec<_> = storage
            .get_events(INTEX_ADDRESS)
            .iter()
            .filter_map(|log| IIntex::IntexSeriesClosed::decode_log_data(log).ok())
            .map(|event| {
                (
                    event.seriesId,
                    event.issuanceCurrency,
                    event.referenceCurrency,
                    event.forfeitedCount,
                    event.forfeitedAmountMinor,
                )
            })
            .collect();
        assert_eq!(closed, vec![(id.into(), 840, 840, 0, U256::ZERO)]);
    }
}
