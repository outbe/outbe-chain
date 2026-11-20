use super::*;

#[test]
fn series_zero_minted_nods_automatic_breach_call_forfeit_and_clean() {
    use alloy_sol_types::SolCall;
    use outbe_system_common::time::{
        date_key_to_utc_timestamp, next_date_key, previous_date_key, timestamp_to_date_key,
        SECONDS_PER_DAY,
    };

    let mut provider = HashMapStorageProvider::new(1);
    let emission_day_key = 20250110u32;
    let base_ts = date_key_to_utc_timestamp(emission_day_key).unwrap() + 3_600;
    provider.set_timestamp(U256::from(base_ts));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 3);

    let day: outbe_system_common::time::WorldwideDay = 20241220u32.into();
    let owner1 = address!("0x1111111111111111111111111111111111111111");
    let owner2 = address!("0x2222222222222222222222222222222222222222");
    let owner_unquoted = address!("0x3333333333333333333333333333333333333333");
    let stranger = address!("0x9999999999999999999999999999999999999999");

    let mut t1 = sample_tribute();
    set_owner(&mut t1, owner1);
    t1.nominal_amount_minor = U256::from(400_000_000u64);

    let mut t2 = sample_tribute();
    set_owner(&mut t2, owner2);
    t2.nominal_amount_minor = U256::from(600_000_000u64);

    let mut t_unquoted = sample_tribute();
    set_owner(&mut t_unquoted, owner_unquoted);
    t_unquoted.reference_currency = 978;
    t_unquoted.nominal_amount_minor = U256::from(200_000_000u64);

    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let nod_contract = outbe_nod::NodContract::new(storage.clone());
        nod_contract
            .config_profile
            .write(outbe_nod::config::PROFILE_PROD)
            .unwrap();

        // Seed entry price only for 840 (2.0 USD), leaving 978 unquoted
        seed_entry_price(&storage, 840, emission_day_key, U256::from(2_000_000u64));

        let mut tc = TributeContract::new(storage.clone());
        tc.issue_with_league(&t1, Some(100)).unwrap();
        tc.issue_with_league(&t2, Some(100)).unwrap();
        tc.issue_with_league(&t_unquoted, Some(100)).unwrap();
        in_process_ready(&storage, || tc.seal_day(day));

        tc.day_emission_day
            .write(&day, EmissionDate::new(emission_day_key))
            .unwrap();
        let lysis_limit = U256::from(500_000_000_000u64);
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        let (_fractions, allocated) = storage
            .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
                seal_lysis(&mut tc, day, lysis_limit, base_ts)
            })
            .unwrap();

        // Unquoted 978 must not have sealed a series; 840 sealed a 0-Nod series with 2 Tributes
        assert!(outbe_nod::api::series_group(
            &storage,
            outbe_core_common::series_id::SeriesId::for_pair(emission_day_key, 840, 978).unwrap(),
        )
        .unwrap()
        .is_none());
        let clean_unquoted = crate::precompile::ITribute::cleanCall {
            tributeId: t_unquoted.tribute_id.to_u256(),
        }
        .abi_encode();
        crate::precompile::dispatch(storage.clone(), &clean_unquoted, stranger, U256::ZERO)
            .unwrap();
        assert!(tc.get_record(t_unquoted.tribute_id).unwrap().is_none());

        let group_key = outbe_nod::api::series_group(
            &storage,
            outbe_core_common::series_id::SeriesId::for_pair(emission_day_key, 840, 840).unwrap(),
        )
        .unwrap()
        .expect("the 840 series is sealed");
        assert_eq!(
            nod_contract
                .group_unclaimed_tribute_count
                .read(&group_key)
                .unwrap(),
            2
        );
        assert_eq!(nod_contract.group_nod_count.read(&group_key).unwrap(), 0);
        assert_eq!(
            nod_contract.read_group_open_load(group_key).unwrap(),
            allocated
        );

        // Seed 28 consecutive closed UTC days starting at full_day_after_issue above call price (8.00 > 7.12)
        let first_full = next_date_key(emission_day_key).unwrap();
        let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 840)
            .unwrap()
            .unwrap();
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        let mut cursor_day = first_full;
        for _ in 0..28 {
            oracle
                .record_utc_day_vwap(cursor_day, index, U256::from(8_000_000u64))
                .unwrap();
            cursor_day = next_date_key(cursor_day).unwrap();
        }
        let latest_closed = previous_date_key(cursor_day).unwrap();
        oracle
            .utc_day_vwap_last_finalized
            .write(latest_closed)
            .unwrap();
        // Earlier daily passes completed in the established parent state.
        nod_contract
            .call_latest_day
            .write(previous_date_key(latest_closed).unwrap())
            .unwrap();

        // Run automatic call scan (`schedule_call_pass` + `scan_and_call`) with 0 minted Nods!
        let call_ts = date_key_to_utc_timestamp(cursor_day).unwrap() + 3_600;
        storage.set_block_timestamp(U256::from(call_ts)).unwrap();
        let call_ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 20,
                timestamp: call_ts,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: GENESIS_TIMESTAMP,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        outbe_nodfactory::called::schedule_call_pass(&call_ctx).unwrap();
        let called = outbe_nodfactory::called::scan_and_call(&call_ctx).unwrap();
        assert_eq!(called, 1);
        assert_eq!(
            nod_contract.group_called_at.read(&group_key).unwrap(),
            call_ts
        );

        // Advance past call notice period and run the Nod daily pass
        let terms = nod_contract.read_call_terms(group_key).unwrap();
        let deadline =
            outbe_nod::api::settlement_deadline_of(call_ts, terms.call_notice_period_seconds);
        let forfeit_ts = deadline + SECONDS_PER_DAY;
        storage.set_block_timestamp(U256::from(forfeit_ts)).unwrap();
        let promis_before = outbe_promislimit::PromisLimitContract::new(storage.clone())
            .promis_limit()
            .unwrap();
        run_nod_daily_pass(&storage, 21, forfeit_ts);
        let promis_after = outbe_promislimit::PromisLimitContract::new(storage.clone())
            .promis_limit()
            .unwrap();
        assert_eq!(promis_after - promis_before, allocated);
        assert!(nod_contract.group_is_forfeited.read(&group_key).unwrap());

        // Permissionlessly clean both unclaimed Tributes; the 0-Nod series closes when count hits 0
        for id in [t1.tribute_id, t2.tribute_id] {
            let clean_call = crate::precompile::ITribute::cleanCall {
                tributeId: id.to_u256(),
            }
            .abi_encode();
            crate::precompile::dispatch(storage.clone(), &clean_call, stranger, U256::ZERO)
                .unwrap();
            assert!(tc.get_record(id).unwrap().is_none());
        }
        assert_eq!(
            nod_contract
                .group_unclaimed_tribute_count
                .read(&group_key)
                .unwrap(),
            0
        );
        assert!(outbe_nod::api::get_group(&storage, group_key)
            .unwrap()
            .is_none());
        assert_eq!(
            timestamp_to_date_key(forfeit_ts),
            timestamp_to_date_key(deadline + SECONDS_PER_DAY)
        );
    });
}

#[test]
fn clean_handles_settled_nod_aborted_emission_day_retired_fi_groups_and_zero_load_tribute() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let base_ts = 1_750_000_000u64;
    provider.set_timestamp(U256::from(base_ts));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 8);

    let day_zero_load: outbe_system_common::time::WorldwideDay = 20241221u32.into();
    let day_aborted: outbe_system_common::time::WorldwideDay = 20241222u32.into();
    let day_retired: outbe_system_common::time::WorldwideDay = 20241223u32.into();
    let owner1 = address!("0x1111111111111111111111111111111111111111");
    let owner2 = address!("0x2222222222222222222222222222222222222222");
    let stranger = address!("0x9999999999999999999999999999999999999999");

    let normal_tribute_id = StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut tc = TributeContract::new(storage.clone());

        // 1. P4.2b: tiny Tribute whose load rounds to 0 does not fail Lysis with ZeroGratisLoad
        let mut t_tiny = sample_tribute();
        set_owner(&mut t_tiny, owner1);
        set_day(&mut t_tiny, day_zero_load);
        t_tiny.nominal_amount_minor = U256::from(1u64);

        let mut t_normal = sample_tribute();
        set_owner(&mut t_normal, owner2);
        set_day(&mut t_normal, day_zero_load);
        t_normal.nominal_amount_minor = U256::from(500_000_000u64);

        let price_day = outbe_nod::openings::entry_price_day(base_ts).unwrap();
        seed_entry_price(&storage, 840, price_day, U256::from(2_000_000u64));
        tc.issue_with_league(&t_tiny, Some(100)).unwrap();
        tc.issue_with_league(&t_normal, Some(4000)).unwrap();
        in_process_ready(&storage, || tc.seal_day(day_zero_load));
        let binding = metadosis_process_ready_binding(1, 0, base_ts);
        let (_fracs, allocated) = storage
            .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
                seal_lysis(&mut tc, day_zero_load, U256::from(100_000u64), base_ts)
            })
            .unwrap();
        assert!(allocated <= U256::from(100_000u64));

        // 2. P4.4c: INod.clean on a settled Nod reverts with NodAlreadySettled
        let nod_id = tc.issue_nod(owner2, t_normal.tribute_id).unwrap();
        let nod_contract = outbe_nod::NodContract::new(storage.clone());
        let mut stored_nod = nod_contract.records.get(nod_id).unwrap().unwrap();
        stored_nod.is_settled = true;
        nod_contract.records.update(&stored_nod).unwrap();
        let clean_nod_call = outbe_nod::precompile::INod::cleanCall { nodId: nod_id }.abi_encode();
        let settled_err =
            outbe_nod::precompile::dispatch(storage.clone(), &clean_nod_call, stranger, U256::ZERO)
                .unwrap_err();
        let expected_settled: PrecompileError =
            outbe_nod::errors::NodError::NodAlreadySettled.into();
        assert_eq!(settled_err.to_string(), expected_settled.to_string());

        // 3. P4.4f: ITribute.clean succeeds when linked EmissionDay has metadosis_state == CANCELLED
        let mut t_aborted = sample_tribute();
        set_owner(&mut t_aborted, owner1);
        set_day(&mut t_aborted, day_aborted);
        tc.issue_with_league(&t_aborted, Some(200)).unwrap();
        in_process_ready(&storage, || tc.seal_day(day_aborted));
        let aborted_emission_day = 20250112u32;
        tc.day_emission_day
            .write(&day_aborted, EmissionDate::new(aborted_emission_day))
            .unwrap();
        outbe_emission::api::record_failed_day_allocation(
            storage.clone(),
            aborted_emission_day,
            U256::from(1_000_000u64),
        )
        .unwrap();
        let clean_aborted = crate::precompile::ITribute::cleanCall {
            tributeId: t_aborted.tribute_id.to_u256(),
        }
        .abi_encode();
        crate::precompile::dispatch(storage.clone(), &clean_aborted, stranger, U256::ZERO).unwrap();
        assert!(tc.get_record(t_aborted.tribute_id).unwrap().is_none());
        assert_eq!(tc.tribute_leagues.read(&t_aborted.tribute_id).unwrap(), 0);

        // 4. P4.4h: ITribute.clean on a retired partition cleans the Tribute league
        let mut t_retired = sample_tribute();
        set_owner(&mut t_retired, owner1);
        set_day(&mut t_retired, day_retired);
        tc.issue_with_league(&t_retired, Some(300)).unwrap();
        in_process_ready(&storage, || tc.seal_day(day_retired));
        tc.retired_days.write(&day_retired, true).unwrap();
        assert_eq!(tc.tribute_leagues.read(&t_retired.tribute_id).unwrap(), 300);
        let clean_retired = crate::precompile::ITribute::cleanCall {
            tributeId: t_retired.tribute_id.to_u256(),
        }
        .abi_encode();
        crate::precompile::dispatch(storage.clone(), &clean_retired, stranger, U256::ZERO).unwrap();
        assert_eq!(tc.tribute_leagues.read(&t_retired.tribute_id).unwrap(), 0);
        t_normal.tribute_id.to_u256()
    });

    // Only step 2 (issue_nod on t_normal) emits TributeBurned; neither clean_aborted (!retired)
    // nor clean_retired (retired) emits TributeBurned.
    let burned: Vec<_> = provider
        .get_events(TRIBUTE_ADDRESS)
        .iter()
        .filter_map(|log| crate::precompile::ITribute::TributeBurned::decode_log_data(log).ok())
        .map(|ev| (ev.tributeId, ev.owner))
        .collect();
    assert_eq!(burned, vec![(normal_tribute_id, owner2)]);
}

#[test]
fn a_zero_load_tribute_stays_out_of_its_series_and_cleans_at_once() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let base_ts = 1_750_000_000u64;
    provider.set_timestamp(U256::from(base_ts));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 3);

    let day: outbe_system_common::time::WorldwideDay = 20241221u32.into();
    let owner1 = address!("0x1111111111111111111111111111111111111111");
    let owner2 = address!("0x2222222222222222222222222222222222222222");
    let stranger = address!("0x9999999999999999999999999999999999999999");

    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut tc = TributeContract::new(storage.clone());

        let mut t_tiny = sample_tribute();
        set_owner(&mut t_tiny, owner1);
        set_day(&mut t_tiny, day);
        t_tiny.nominal_amount_minor = U256::from(1u64);
        let mut t_normal = sample_tribute();
        set_owner(&mut t_normal, owner2);
        set_day(&mut t_normal, day);
        t_normal.nominal_amount_minor = U256::from(500_000_000u64);

        let price_day = outbe_nod::openings::entry_price_day(base_ts).unwrap();
        seed_entry_price(&storage, 840, price_day, U256::from(2_000_000u64));
        tc.issue_with_league(&t_tiny, Some(100)).unwrap();
        tc.issue_with_league(&t_normal, Some(4000)).unwrap();
        in_process_ready(&storage, || tc.seal_day(day));
        let binding = metadosis_process_ready_binding(1, 0, base_ts);
        storage
            .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
                seal_lysis(&mut tc, day, U256::from(100_000u64), base_ts)
            })
            .unwrap();
        let tiny_fraction = test_day_league_fraction(&tc, day, 100);
        assert!(
            (U256::from(tiny_fraction) / U256::from(1_000_000u64)).is_zero(),
            "the tiny Tribute's load rounds to zero"
        );

        let series = outbe_core_common::series_id::SeriesId::for_pair(price_day, 840, 840).unwrap();
        let group = outbe_nod::api::series_group(&storage, series)
            .unwrap()
            .expect("the normal Tribute seals the series");
        let nod = outbe_nod::NodContract::new(storage.clone());
        assert_eq!(nod.group_unclaimed_tribute_count.read(&group).unwrap(), 1);
        assert_eq!(nod.series_unclaimed_tribute_count.read(&series).unwrap(), 1);

        let expected_zero_load: PrecompileError = crate::errors::TributeError::LysisArithmetic(
            outbe_lysis::errors::ProgramErrorV1::ZeroGratisLoad { ordinal: 0 }.to_string(),
        )
        .into();
        assert_eq!(
            tc.issue_nod(owner1, t_tiny.tribute_id)
                .unwrap_err()
                .to_string(),
            expected_zero_load.to_string()
        );
        let clean_tiny = crate::precompile::ITribute::cleanCall {
            tributeId: t_tiny.tribute_id.to_u256(),
        }
        .abi_encode();
        crate::precompile::dispatch(storage.clone(), &clean_tiny, stranger, U256::ZERO).unwrap();
        assert!(tc.get_record(t_tiny.tribute_id).unwrap().is_none());
        assert_eq!(nod.series_unclaimed_tribute_count.read(&series).unwrap(), 1);

        tc.issue_nod(owner2, t_normal.tribute_id).unwrap();
        assert_eq!(nod.group_unclaimed_tribute_count.read(&group).unwrap(), 0);
        assert_eq!(nod.series_unclaimed_tribute_count.read(&series).unwrap(), 0);
        assert_eq!(
            outbe_nod::api::series_group(&storage, series).unwrap(),
            Some(group)
        );
    });
}
