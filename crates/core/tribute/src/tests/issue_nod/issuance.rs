use super::*;

#[test]
fn v1_mechanics_fi_groups_fractions_and_late_issue_nod_burns_tribute() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let base_ts = 1_750_000_000u64;
    provider.set_timestamp(U256::from(base_ts));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 3);

    let day: outbe_system_common::time::WorldwideDay = 20241220u32.into();
    let owner1 = address!("0x1111111111111111111111111111111111111111");
    let owner2 = address!("0x2222222222222222222222222222222222222222");
    let stranger = address!("0x9999999999999999999999999999999999999999");

    let mut t1 = sample_tribute();
    set_owner(&mut t1, owner1);
    t1.nominal_amount_minor = U256::from(400_000_000u64); // 400 USD

    let mut t2 = sample_tribute();
    set_owner(&mut t2, owner2);
    t2.nominal_amount_minor = U256::from(600_000_000u64); // 600 USD

    let expected_nod_ids = StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut tc = TributeContract::new(storage.clone());

        // Stage 4: Issue Tributes in leagues 100 and 4000
        tc.issue_with_league(&t1, Some(100)).unwrap();
        tc.issue_with_league(&t2, Some(4000)).unwrap();
        assert_eq!(tc.get_league(t1.tribute_id).unwrap(), 100);
        assert_eq!(tc.get_league(t2.tribute_id).unwrap(), 4000);

        // Cannot issueNod before Metadosis has completed
        assert!(tc.issue_nod(owner1, t1.tribute_id).is_err());

        // Stage 3: Seal day and compute Metadosis fractions for WWD
        in_process_ready(&storage, || tc.seal_worldwide_day(day));
        seed_entry_price(
            &storage,
            840,
            outbe_nod::openings::entry_price_day(base_ts).expect("date step"),
            U256::from(1_234_567u64),
        );
        let lysis_limit = U256::from(500_000_000_000u64);
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        let (_total_nominal, allocated) = storage
            .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
                seal_lysis(&mut tc, day, lysis_limit, base_ts)
            })
            .unwrap();
        assert!(test_day_league_fraction(&tc, day, 4000) > 0);
        assert!(test_day_league_fraction(&tc, day, 100) > 0);
        assert!(allocated > U256::ZERO);
        assert!(tc.get_record(t1.tribute_id).unwrap().is_some());
        assert!(tc.get_record(t2.tribute_id).unwrap().is_some());
        assert_eq!(tc.balance_of(owner1).unwrap(), 1);
        assert_eq!(tc.balance_of(owner2).unwrap(), 1);
        assert_eq!(tc.get_day_totals(day).unwrap().tribute_count, 2);

        storage
            .set_block_timestamp(U256::from(base_ts - 1))
            .unwrap();
        let early = tc.issue_nod(owner1, t1.tribute_id).unwrap_err();
        let expected_early: PrecompileError =
            crate::errors::TributeError::NodIssueBeforeCompletion.into();
        assert_eq!(early.to_string(), expected_early.to_string());
        assert!(tc.get_record(t1.tribute_id).unwrap().is_some());
        assert!(outbe_nod::api::get_item(&storage, nod_id_of(&tc, &t1))
            .unwrap()
            .is_none());
        assert_eq!(tc.balance_of(t1.owner).unwrap(), 1);
        storage.set_block_timestamp(U256::from(base_ts)).unwrap();

        // A non-owner claim preserves the Tribute and issues no Nod.
        let stranger_err = tc.issue_nod(stranger, t1.tribute_id).unwrap_err();
        let expected_owner: PrecompileError =
            crate::errors::TributeError::CallerNotTributeOwner.into();
        assert_eq!(stranger_err.to_string(), expected_owner.to_string());
        assert!(tc.get_record(t1.tribute_id).unwrap().is_some());
        assert!(outbe_nod::api::get_item(&storage, nod_id_of(&tc, &t1))
            .unwrap()
            .is_none());

        // Owner1 creates a Nod three days after completion.
        storage
            .set_block_timestamp(U256::from(
                base_ts + 3 * outbe_system_common::time::SECONDS_PER_DAY,
            ))
            .unwrap();
        let call_bytes = crate::precompile::ITribute::issueNodCall {
            tributeId: t1.tribute_id.to_u256(),
        }
        .abi_encode();
        let reverted: outbe_primitives::error::Result<()> = storage.with_checkpoint(|| {
            crate::precompile::dispatch(storage.clone(), &call_bytes, owner1, U256::ZERO)?;
            assert!(tc.get_record(t1.tribute_id)?.is_none());
            Err(PrecompileError::Revert(
                "containing transaction reverted".into(),
            ))
        });
        assert!(reverted.is_err());
        assert!(tc.get_record(t1.tribute_id).unwrap().is_some());
        assert!(outbe_nod::api::get_item(&storage, nod_id_of(&tc, &t1),)
            .unwrap()
            .is_none());
        let ret =
            crate::precompile::dispatch(storage.clone(), &call_bytes, owner1, U256::ZERO).unwrap();
        let nod_id_u256 =
            crate::precompile::ITribute::issueNodCall::abi_decode_returns(&ret).unwrap();
        assert_eq!(nod_id_u256, nod_id_of(&tc, &t1));
        assert_ne!(nod_id_u256, t1.tribute_id.to_u256());

        // Verify Nod exists in outbe-nod and Tribute is burned with consistent partition counters!
        let nod_item = outbe_nod::api::get_item(&storage, nod_id_of(&tc, &t1))
            .unwrap()
            .expect("Nod must be created");
        assert_eq!(nod_item.owner, owner1);
        assert_eq!(nod_item.league_id, 100);
        assert_eq!(
            nod_item.issued_at,
            base_ts + 3 * outbe_system_common::time::SECONDS_PER_DAY
        );
        assert_eq!(
            nod_item.encrypted.terms.entry_price_minor,
            U256::from(1_234_567u64)
        );
        let nod_view_key = outbe_nod::nod_encryption::derive_view_key(
            &crate::enclave_client::test_enclave::state_key(),
            owner1,
        )
        .unwrap();
        let owner_decrypted =
            outbe_nod::test_support::decrypt_for_owner(&nod_view_key, &nod_item.encrypted)
                .expect("owner must decrypt real encrypted Nod");
        let enclave_decrypted = outbe_nod::nod_encryption::decrypt_nod(
            &crate::enclave_client::test_enclave::state_key(),
            &nod_item.encrypted,
        )
        .expect("enclave must decrypt real encrypted Nod");
        assert!(!owner_decrypted.is_zero());
        assert_eq!(owner_decrypted, enclave_decrypted);
        assert!(
            outbe_nod::test_support::decrypt_for_owner(
                &outbe_tee::resident::StateKeyHandle::for_test([0x78; 32]),
                &nod_item.encrypted,
            )
            .is_err(),
            "wrong view key must fail ChaCha20-Poly1305 authentication"
        );
        let raw_amount_be = owner_decrypted.to_be_bytes::<32>();
        assert!(
            !nod_item
                .encrypted
                .encrypted_gratis_amount
                .windows(32)
                .any(|window| window == raw_amount_be),
            "encrypted_gratis_amount must not contain plaintext U256 bytes"
        );
        assert!(tc.get_record(t1.tribute_id).unwrap().is_none());
        assert_eq!(tc.balance_of(t1.owner).unwrap(), 0);
        let totals_after_t1 = tc.get_day_totals(day).unwrap();
        assert_eq!(totals_after_t1.tribute_count, 1);
        assert_eq!(
            totals_after_t1.tribute_nominal_total_minor,
            t2.nominal_amount_minor
        );
        assert_eq!(tc.tribute_leagues.read(&t1.tribute_id).unwrap(), 0);

        // Calling issueNod a second time for the burned Tribute must fail
        assert!(
            crate::precompile::dispatch(storage.clone(), &call_bytes, owner1, U256::ZERO).is_err()
        );

        // A claim after seven days still creates the Nod and deletes only that Tribute.
        let late = base_ts + 7 * outbe_system_common::time::SECONDS_PER_DAY + 1;
        storage.set_block_timestamp(U256::from(late)).unwrap();
        assert_eq!(
            tc.issue_nod(owner2, t2.tribute_id).unwrap(),
            nod_id_of(&tc, &t2)
        );
        assert!(tc.get_record(t2.tribute_id).unwrap().is_none());
        assert_eq!(tc.balance_of(t2.owner).unwrap(), 0);
        let late_nod = outbe_nod::api::get_item(&storage, nod_id_of(&tc, &t2))
            .unwrap()
            .expect("late Nod");
        assert_eq!(late_nod.owner, owner2);
        assert_eq!(late_nod.league_id, 4000);
        assert_eq!(late_nod.issued_at, late);
        let totals_after_t2 = tc.get_day_totals(day).unwrap();
        assert_eq!(totals_after_t2.tribute_count, 0);
        assert!(totals_after_t2.tribute_nominal_total_minor.is_zero());
        assert_eq!(tc.tribute_leagues.read(&t2.tribute_id).unwrap(), 0);
        assert!(outbe_nod::api::get_item(&storage, nod_id_of(&tc, &t1))
            .unwrap()
            .is_some());
        [nod_id_of(&tc, &t1), nod_id_of(&tc, &t2)]
    });

    let events = provider.get_events(TRIBUTE_ADDRESS);
    let burned_events: Vec<_> = events
        .iter()
        .filter_map(|log| crate::precompile::ITribute::TributeBurned::decode_log_data(log).ok())
        .collect();
    assert_eq!(burned_events.len(), 2);
    assert_eq!(burned_events[0].tributeId, t1.tribute_id.to_u256());
    assert_eq!(burned_events[0].owner, owner1);
    assert_eq!(burned_events[1].tributeId, t2.tribute_id.to_u256());
    assert_eq!(burned_events[1].owner, owner2);
    let issued_ids: Vec<_> = provider
        .get_events(outbe_system_common::addresses::NOD_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| {
            outbe_nodfactory::precompile::INodFactory::NodIssued::decode_log_data(log).ok()
        })
        .map(|event| event.nodId)
        .collect();
    assert_eq!(issued_ids, expected_nod_ids);
}

#[test]
fn a_series_unpriced_at_lysis_gives_no_nod_and_cleans_at_once() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let completed_at = 1_750_000_000u64;
    provider.set_timestamp(U256::from(completed_at));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 3);
    let tribute = sample_tribute();
    let day = tribute.worldwide_day;

    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut tc = TributeContract::new(storage.clone());
        tc.issue_with_league(&tribute, Some(100)).unwrap();
        in_process_ready(&storage, || tc.seal_worldwide_day(day));
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
                seal_lysis(&mut tc, day, U256::from(500_000_000_000u64), completed_at)
            })
            .unwrap();
        storage
            .set_block_timestamp(U256::from(
                completed_at + outbe_system_common::time::SECONDS_PER_DAY,
            ))
            .unwrap();

        let call = crate::precompile::ITribute::issueNodCall {
            tributeId: tribute.tribute_id.to_u256(),
        }
        .abi_encode();
        let forfeited: PrecompileError = crate::errors::TributeError::TributeForfeited.into();
        let unpriced =
            crate::precompile::dispatch(storage.clone(), &call, tribute.owner, U256::ZERO);
        assert_eq!(unpriced.unwrap_err().to_string(), forfeited.to_string());
        assert!(tc.get_record(tribute.tribute_id).unwrap().is_some());
        assert_eq!(tc.balance_of(tribute.owner).unwrap(), 1);
        assert!(outbe_nod::api::get_item(&storage, nod_id_of(&tc, &tribute))
            .unwrap()
            .is_none());

        // A price that comes after Lysis issues nothing: Lysis returned the load already.
        seed_entry_price(
            &storage,
            840,
            outbe_nod::openings::entry_price_day(completed_at).expect("date step"),
            U256::from(1_234_567u64),
        );
        let late = crate::precompile::dispatch(storage.clone(), &call, tribute.owner, U256::ZERO);
        assert_eq!(late.unwrap_err().to_string(), forfeited.to_string());
        let clean = crate::precompile::ITribute::cleanCall {
            tributeId: tribute.tribute_id.to_u256(),
        }
        .abi_encode();
        crate::precompile::dispatch(storage.clone(), &clean, tribute.owner, U256::ZERO).unwrap();
        assert!(tc.get_record(tribute.tribute_id).unwrap().is_none());
    });

    let burned = provider
        .get_events(TRIBUTE_ADDRESS)
        .iter()
        .filter(|log| crate::precompile::ITribute::TributeBurned::decode_log_data(log).is_ok())
        .count();
    assert_eq!(burned, 0);
}

#[test]
fn lysis_keys_its_result_by_the_emission_day_and_binds_the_nod_to_it() {
    let mut provider = HashMapStorageProvider::new(1);
    let completed_at = 1_750_000_000u64;
    provider.set_timestamp(U256::from(completed_at));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 6);
    let tribute = sample_tribute();
    let day = tribute.worldwide_day;
    let mut other_tribute = sample_tribute();
    let other_wwd = outbe_system_common::time::WorldwideDay::new(20241221);
    set_day(&mut other_tribute, other_wwd);

    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut tc = TributeContract::new(storage.clone());
        let emission_day =
            EmissionDate::new(outbe_nod::openings::entry_price_day(completed_at).unwrap());
        let other_day = emission_day.next().unwrap();
        for priced_day in [emission_day, other_day] {
            seed_entry_price(&storage, 840, priced_day.value(), U256::from(2_000_000u64));
        }
        tc.issue_with_league(&tribute, Some(100)).unwrap();
        tc.issue_with_league(&other_tribute, Some(100)).unwrap();
        tc.day_emission_day.write(&other_wwd, other_day).unwrap();
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        for sealed in [day, other_wwd] {
            in_process_ready(&storage, || tc.seal_worldwide_day(sealed));
            storage
                .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
                    seal_lysis(
                        &mut tc,
                        sealed,
                        U256::from(500_000_000_000u64),
                        completed_at,
                    )
                })
                .unwrap();
        }

        assert_eq!(tc.lysis_emission_day(day).unwrap(), Some(emission_day));
        assert_eq!(
            tc.day_metadosis_completed_at.read(&emission_day).unwrap(),
            completed_at
        );
        let wwd_as_day = EmissionDate::new(day.value());
        assert_eq!(tc.day_metadosis_completed_at.read(&wwd_as_day).unwrap(), 0);
        assert!(tc.encrypted_day_fractions(wwd_as_day).unwrap().is_empty());
        let day_fractions = tc.day_fractions_record(emission_day).unwrap().unwrap();
        assert_eq!(day_fractions.emission_day, emission_day);
        assert_eq!(day_fractions.worldwide_day, day);

        let record = tc.get_record(tribute.tribute_id).unwrap().unwrap();
        let request_for = |fractions_day: EmissionDate| {
            let series = outbe_core_common::series_id::SeriesId::for_pair(
                fractions_day.value(),
                tribute.issuance_currency,
                tribute.reference_currency,
            )
            .unwrap();
            crate::confidential::IssueNodFromTributeRequestV1 {
                tribute: record.encrypted().unwrap().clone(),
                day_fractions: tc.day_fractions_record(fractions_day).unwrap().unwrap(),
                terms: outbe_core_common::nod_encryption::NodTermsV1 {
                    chain_id: storage.chain_id().unwrap(),
                    nod_id: outbe_core_common::nod_rules::derive_nod_id(series, tribute.owner),
                    owner: tribute.owner,
                    emission_day: fractions_day.value(),
                    league_id: 100,
                    issuance_currency: tribute.issuance_currency,
                    reference_currency: tribute.reference_currency,
                    entry_price_minor: U256::from(2_000_000u64),
                },
            }
        };
        let mut wrong_terms = request_for(emission_day);
        wrong_terms.terms = request_for(other_day).terms;
        let rejected = crate::enclave_client::issue_nod_from_tribute(wrong_terms).unwrap_err();
        assert!(rejected.to_string().contains("NOD source terms mismatch"));
        let other_days_fractions =
            crate::enclave_client::issue_nod_from_tribute(request_for(other_day)).unwrap_err();
        assert!(other_days_fractions
            .to_string()
            .contains("NOD source terms mismatch"));
    });
}

#[test]
fn the_enclave_refuses_a_lysis_list_that_differs_from_the_frozen_day_nominal() {
    let mut provider = HashMapStorageProvider::new(1);
    let completed_at = 1_750_000_000u64;
    provider.set_timestamp(U256::from(completed_at));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 3);
    let first = sample_tribute();
    let mut second = sample_tribute();
    set_owner(
        &mut second,
        address!("0x2222222222222222222222222222222222222222"),
    );
    let day = first.worldwide_day;

    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut tc = TributeContract::new(storage.clone());
        tc.issue_with_league(&first, Some(100)).unwrap();
        tc.issue_with_league(&second, Some(100)).unwrap();
        in_process_ready(&storage, || tc.seal_worldwide_day(day));
        in_process_ready(&storage, || tc.freeze_day_nominal(&tc.get_day_totals(day)?));
        let mut ids = [first.tribute_id, second.tribute_id];
        ids.sort();
        let inputs: Vec<_> = ids
            .iter()
            .map(|id| crate::confidential::LysisTributeInputV1 {
                tribute: tc
                    .get_record(*id)
                    .unwrap()
                    .unwrap()
                    .encrypted()
                    .unwrap()
                    .clone(),
                league: 100,
                series_scope: None,
            })
            .collect();
        let lysis = |tributes| {
            crate::enclave_client::execute_lysis_fractions(
                crate::confidential::LysisFractionsRequestV1 {
                    chain_id: storage.chain_id().unwrap(),
                    worldwide_day: day,
                    emission_day: EmissionDate::new(20241221),
                    completed_at,
                    metadosis_limit_minor: U256::from(1_000u64),
                    day_nominal: tc.day_amount_record(day, true).unwrap().unwrap(),
                    tributes,
                },
            )
        };

        assert!(lysis(inputs.clone()).is_ok());
        let (a, b) = (inputs[0].clone(), inputs[1].clone());
        for tributes in [
            vec![a.clone()],
            vec![a.clone(), a.clone(), b.clone()],
            vec![b, a],
        ] {
            let refused = lysis(tributes).unwrap_err();
            assert!(
                refused.to_string().contains("frozen day nominal"),
                "{refused}"
            );
        }
    });
}

#[test]
fn series_sealing_zero_nod_forfeit_returns_unclaimed_load_and_enables_permissionless_clean() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let base_ts = 1_750_000_000u64;
    provider.set_timestamp(U256::from(base_ts));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 3);

    let day: outbe_system_common::time::WorldwideDay = 20241220u32.into();
    let emission_day_key = 20250110u32;
    let owner1 = address!("0x1111111111111111111111111111111111111111");
    let owner2 = address!("0x2222222222222222222222222222222222222222");
    let stranger = address!("0x9999999999999999999999999999999999999999");

    let mut t1 = sample_tribute();
    set_owner(&mut t1, owner1);
    t1.nominal_amount_minor = U256::from(400_000_000u64);

    let mut t2 = sample_tribute();
    set_owner(&mut t2, owner2);
    t2.nominal_amount_minor = U256::from(600_000_000u64);

    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        // Record emission_day price before Lysis so series call_terms seal at Lysis
        seed_entry_price(&storage, 840, emission_day_key, U256::from(2_000_000u64));

        let mut tc = TributeContract::new(storage.clone());
        tc.issue_with_league(&t1, Some(100)).unwrap();
        tc.issue_with_league(&t2, Some(100)).unwrap();
        in_process_ready(&storage, || tc.seal_worldwide_day(day));

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

        let group_key = outbe_nod::api::series_group(
            &storage,
            outbe_core_common::series_id::SeriesId::for_pair(emission_day_key, 840, 840).unwrap(),
        )
        .unwrap()
        .expect("the 840 series is sealed");
        let mut nod = outbe_nod::NodContract::new(storage.clone());
        assert_eq!(
            nod.group_unclaimed_tribute_count.read(&group_key).unwrap(),
            2
        );
        assert_eq!(nod.group_nod_count.read(&group_key).unwrap(), 0);
        assert_eq!(nod.read_group_open_load(group_key).unwrap(), allocated);

        // Cleaning either Tribute while the series is live must revert with TributeNotForfeited
        let clean_t2_call = crate::precompile::ITribute::cleanCall {
            tributeId: t2.tribute_id.to_u256(),
        }
        .abi_encode();
        let live_clean_err =
            crate::precompile::dispatch(storage.clone(), &clean_t2_call, stranger, U256::ZERO)
                .unwrap_err();
        let expected_not_forfeited: PrecompileError =
            crate::errors::TributeError::TributeNotForfeited.into();
        assert_eq!(
            live_clean_err.to_string(),
            expected_not_forfeited.to_string()
        );

        // Owner1 claims t1 -> group open load (unpaid + unclaimed) stays invariant
        let open_load_ct_before = nod.encrypted_group_open_load(group_key).unwrap();
        let nod_id = tc.issue_nod(owner1, t1.tribute_id).unwrap();
        let t1_item = outbe_nod::api::get_item(&storage, nod_id).unwrap().unwrap();
        assert_eq!(t1_item.group_key, group_key);
        let t1_load = outbe_nod::nod_encryption::decrypt_nod(
            &crate::enclave_client::test_enclave::state_key(),
            &t1_item.encrypted,
        )
        .unwrap();
        assert!(!t1_load.is_zero());
        assert_eq!(
            nod.group_unclaimed_tribute_count.read(&group_key).unwrap(),
            1
        );
        assert_eq!(nod.group_nod_count.read(&group_key).unwrap(), 1);
        assert_eq!(
            nod.encrypted_group_open_load(group_key).unwrap(),
            open_load_ct_before
        );
        assert_eq!(nod.read_group_open_load(group_key).unwrap(), allocated);

        // Cleaning live Nod t1 must revert with NodNotForfeited
        let clean_nod_call = outbe_nod::precompile::INod::cleanCall { nodId: nod_id }.abi_encode();
        let live_nod_clean_err =
            outbe_nod::precompile::dispatch(storage.clone(), &clean_nod_call, stranger, U256::ZERO)
                .unwrap_err();
        let expected_nod_not_forfeited: PrecompileError =
            outbe_nod::errors::NodError::NodNotForfeited.into();
        assert_eq!(
            live_nod_clean_err.to_string(),
            expected_nod_not_forfeited.to_string()
        );

        // Call the series and advance past notice period so NodForfeit forfeits it
        let terms = nod.read_call_terms(group_key).unwrap();
        let deadline =
            outbe_nod::api::settlement_deadline_of(base_ts, terms.call_notice_period_seconds);
        nod.record_group_called(group_key, base_ts, deadline)
            .unwrap();
        let forfeit_ts = deadline + outbe_system_common::time::SECONDS_PER_DAY;
        storage.set_block_timestamp(U256::from(forfeit_ts)).unwrap();
        // While the forfeit pass closes the group series by series, no Tribute of it issues.
        nod.group_forfeiting.write(&group_key, true).unwrap();
        let closing_err = tc.issue_nod(owner2, t2.tribute_id).unwrap_err();
        let closing_forfeited: PrecompileError =
            crate::errors::TributeError::TributeForfeited.into();
        assert_eq!(closing_err.to_string(), closing_forfeited.to_string());
        nod.group_forfeiting.clear(&group_key).unwrap();
        let promis_before = outbe_promislimit::PromisLimitContract::new(storage.clone())
            .promis_limit()
            .unwrap();
        run_nod_daily_pass(&storage, 10, forfeit_ts);

        // Both unpaid Nod load and unclaimed Tribute load were returned in one credit
        let promis_after = outbe_promislimit::PromisLimitContract::new(storage.clone())
            .promis_limit()
            .unwrap();
        assert_eq!(promis_after - promis_before, allocated);
        assert!(nod.group_is_forfeited.read(&group_key).unwrap());
        assert_eq!(nod.read_group_open_load(group_key).unwrap(), U256::ZERO);
        assert!(nod.encrypted_group_open_load(group_key).unwrap().is_empty());

        // Subsequent issueNod for unclaimed t2 must revert with TributeForfeited
        let create_t2_err = tc.issue_nod(owner2, t2.tribute_id).unwrap_err();
        let expected_forfeited: PrecompileError =
            crate::errors::TributeError::TributeForfeited.into();
        assert_eq!(create_t2_err.to_string(), expected_forfeited.to_string());

        // Permissionless ITribute.clean(t2) succeeds without moving PromisLimit again
        crate::precompile::dispatch(storage.clone(), &clean_t2_call, stranger, U256::ZERO).unwrap();
        assert!(tc.get_record(t2.tribute_id).unwrap().is_none());
        assert_eq!(
            nod.group_unclaimed_tribute_count.read(&group_key).unwrap(),
            0
        );
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .promis_limit()
                .unwrap(),
            promis_after
        );
        // Group still has 1 unpaid Nod (t1), so group is still open
        assert!(outbe_nod::api::get_group(&storage, group_key)
            .unwrap()
            .is_some());

        // Permissionless INod.clean(t1) succeeds and closes the group once both counters reach 0
        outbe_nod::precompile::dispatch(storage.clone(), &clean_nod_call, stranger, U256::ZERO)
            .unwrap();
        assert!(outbe_nod::api::get_item(&storage, nod_id)
            .unwrap()
            .is_none());
        assert_eq!(nod.group_nod_count.read(&group_key).unwrap(), 0);
        assert!(!nod.group_is_forfeited.read(&group_key).unwrap());
        assert!(outbe_nod::api::get_group(&storage, group_key)
            .unwrap()
            .is_none());
    });
}
