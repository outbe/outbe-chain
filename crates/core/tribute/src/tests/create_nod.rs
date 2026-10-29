use super::{in_process_ready, sample_tribute, set_day, set_owner};
use crate::TributeContract;
use alloy_primitives::{address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::storage::{
    metadosis_process_ready_binding, MetadosisCycleLifecycle, MetadosisMutationPurposeTag,
};
use outbe_primitives::wwd_entity_id::WwdEntityId;

fn seed_entry_price(storage: &StorageHandle<'_>, iso: u16, utc_day: u32, vwap: U256) {
    let index = outbe_oracle::api::register_pair(
        storage.clone(),
        outbe_oracle::api::AddressPair::new_coen_to(iso),
    )
    .unwrap();
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .record_utc_day_vwap(utc_day, index, vwap)
        .unwrap();
}

#[test]
fn v1_mechanics_fi_groups_fractions_and_late_create_nod_burns_tribute() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let base_ts = 1_750_000_000u64;
    provider.set_timestamp(U256::from(base_ts));
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 3);

    let day: outbe_primitives::time::WorldwideDay = 20241220u32.into();
    let owner1 = address!("0x1111111111111111111111111111111111111111");
    let owner2 = address!("0x2222222222222222222222222222222222222222");
    let stranger = address!("0x9999999999999999999999999999999999999999");

    let mut t1 = sample_tribute();
    set_owner(&mut t1, owner1);
    t1.nominal_amount_minor = U256::from(400_000_000u64); // 400 USD

    let mut t2 = sample_tribute();
    set_owner(&mut t2, owner2);
    t2.nominal_amount_minor = U256::from(600_000_000u64); // 600 USD

    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut tc = TributeContract::new(storage.clone());

        // Stage 4: Issue Tributes in leagues 100 and 4000
        tc.issue_with_league(&t1, Some(100)).unwrap();
        tc.issue_with_league(&t2, Some(4000)).unwrap();
        assert_eq!(tc.get_league(t1.tribute_id).unwrap(), 100);
        assert_eq!(tc.get_league(t2.tribute_id).unwrap(), 4000);

        // Stage 2: Verify fi_groups[wwd]
        let groups = tc.get_fi_groups(day).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups.get(&100), Some(&(1, t1.nominal_amount_minor)));
        assert_eq!(groups.get(&4000), Some(&(1, t2.nominal_amount_minor)));

        // Cannot createNod before Metadosis has completed
        assert!(tc.create_nod(owner1, t1.tribute_id).is_err());

        // Stage 3: Seal day and compute Metadosis fractions for WWD
        in_process_ready(&storage, || tc.seal_day(day));
        seed_entry_price(
            &storage,
            840,
            outbe_nod::openings::entry_price_day(base_ts).expect("date step"),
            U256::from(1_234_567u64),
        );
        let lysis_limit = U256::from(500_000_000_000u64); // large enough so nod_max = lysis_limit / 1024 >= loads
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        let (fractions, allocated) = storage
            .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                tc.compute_and_record_metadosis_fractions(day, lysis_limit, base_ts)
            })
            .unwrap();
        assert!(fractions.get(&4000).copied().unwrap_or(0) > 0);
        assert!(fractions.get(&100).copied().unwrap_or(0) > 0);
        assert!(allocated > U256::ZERO);
        assert!(tc.get_record(t1.tribute_id).unwrap().is_some());
        assert!(tc.get_record(t2.tribute_id).unwrap().is_some());
        assert_eq!(tc.total_supply().unwrap(), 2);
        assert_eq!(tc.balance_of(owner1).unwrap(), 1);
        assert_eq!(tc.balance_of(owner2).unwrap(), 1);
        assert_eq!(tc.get_day_totals(day).unwrap().tribute_count, 2);

        storage
            .set_block_timestamp(U256::from(base_ts - 1))
            .unwrap();
        let early = tc.create_nod(owner1, t1.tribute_id).unwrap_err();
        let expected_early: PrecompileError =
            crate::errors::TributeError::NodCreationBeforeCompletion.into();
        assert_eq!(early.to_string(), expected_early.to_string());
        assert!(tc.get_record(t1.tribute_id).unwrap().is_some());
        assert!(outbe_nod::api::get_item(&storage, t1.tribute_id)
            .unwrap()
            .is_none());
        assert_eq!(tc.total_supply().unwrap(), 2);
        storage.set_block_timestamp(U256::from(base_ts)).unwrap();

        // A non-owner claim preserves the Tribute and issues no Nod.
        let stranger_err = tc.create_nod(stranger, t1.tribute_id).unwrap_err();
        let expected_owner: PrecompileError =
            crate::errors::TributeError::CallerNotTributeOwner.into();
        assert_eq!(stranger_err.to_string(), expected_owner.to_string());
        assert!(tc.get_record(t1.tribute_id).unwrap().is_some());
        assert!(outbe_nod::api::get_item(&storage, t1.tribute_id)
            .unwrap()
            .is_none());

        // Owner1 creates a Nod three days after completion.
        storage
            .set_block_timestamp(U256::from(
                base_ts + 3 * outbe_primitives::time::SECONDS_PER_DAY,
            ))
            .unwrap();
        let call_bytes = crate::precompile::ITribute::createNodCall {
            tributeId: t1.tribute_id.to_u256(),
        }
        .abi_encode();
        let ret =
            crate::precompile::dispatch(storage.clone(), &call_bytes, owner1, U256::ZERO).unwrap();
        let nod_id_u256 =
            crate::precompile::ITribute::createNodCall::abi_decode_returns(&ret).unwrap();
        assert_eq!(WwdEntityId::from(nod_id_u256), t1.tribute_id);

        // Verify Nod exists in outbe-nod and Tribute is burned with consistent partition counters!
        let nod_item = outbe_nod::api::get_item(&storage, t1.tribute_id)
            .unwrap()
            .expect("Nod must be created");
        assert_eq!(nod_item.owner, owner1);
        assert_eq!(nod_item.league_id, 100);
        assert_eq!(
            nod_item.issued_at,
            base_ts + 3 * outbe_primitives::time::SECONDS_PER_DAY
        );
        assert_eq!(
            nod_item.encrypted.terms.entry_price_minor,
            U256::from(1_234_567u64)
        );
        let nod_view_key = outbe_tee_enclave::nod_encryption::derive_view_key(
            &crate::enclave_client::test_enclave::NETWORK_SECRET,
            owner1,
        )
        .unwrap();
        let owner_decrypted =
            outbe_tee::nod_decrypt::decrypt_nod_for_owner(&nod_view_key, &nod_item.encrypted)
                .expect("owner must decrypt real encrypted Nod");
        let enclave_decrypted = outbe_tee_enclave::nod_encryption::decrypt_nod(
            &crate::enclave_client::test_enclave::NETWORK_SECRET,
            &nod_item.encrypted,
        )
        .expect("enclave must decrypt real encrypted Nod");
        assert!(!owner_decrypted.is_zero());
        assert_eq!(owner_decrypted, enclave_decrypted);
        assert!(
            outbe_tee::nod_decrypt::decrypt_nod_for_owner(&[0x78; 32], &nod_item.encrypted,)
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
        assert_eq!(tc.total_supply().unwrap(), 1);
        let totals_after_t1 = tc.get_day_totals(day).unwrap();
        assert_eq!(totals_after_t1.tribute_count, 1);
        assert_eq!(
            totals_after_t1.tribute_nominal_total_minor,
            t2.nominal_amount_minor
        );
        let groups_after_t1 = tc.get_fi_groups(day).unwrap();
        assert_eq!(groups_after_t1.len(), 1);
        assert_eq!(
            groups_after_t1.get(&4000),
            Some(&(1, t2.nominal_amount_minor))
        );

        // Calling createNod a second time for the burned Tribute must fail
        assert!(
            crate::precompile::dispatch(storage.clone(), &call_bytes, owner1, U256::ZERO).is_err()
        );

        // A claim after seven days still creates the Nod and deletes only that Tribute.
        let late = base_ts + 7 * outbe_primitives::time::SECONDS_PER_DAY + 1;
        storage.set_block_timestamp(U256::from(late)).unwrap();
        assert_eq!(tc.create_nod(owner2, t2.tribute_id).unwrap(), t2.tribute_id);
        assert!(tc.get_record(t2.tribute_id).unwrap().is_none());
        assert_eq!(tc.total_supply().unwrap(), 0);
        let late_nod = outbe_nod::api::get_item(&storage, t2.tribute_id)
            .unwrap()
            .expect("late Nod");
        assert_eq!(late_nod.owner, owner2);
        assert_eq!(late_nod.league_id, 4000);
        assert_eq!(late_nod.issued_at, late);
        let totals_after_t2 = tc.get_day_totals(day).unwrap();
        assert_eq!(totals_after_t2.tribute_count, 0);
        assert!(totals_after_t2.tribute_nominal_total_minor.is_zero());
        assert!(tc.get_fi_groups(day).unwrap().is_empty());
        assert!(outbe_nod::api::get_item(&storage, t1.tribute_id)
            .unwrap()
            .is_some());
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
}

#[test]
fn create_nod_reverts_without_a_completion_day_price() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let completed_at = 1_750_000_000u64;
    provider.set_timestamp(U256::from(completed_at));
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 3);
    let tribute = sample_tribute();
    let day = tribute.worldwide_day;

    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut tc = TributeContract::new(storage.clone());
        tc.issue_with_league(&tribute, Some(100)).unwrap();
        in_process_ready(&storage, || tc.seal_day(day));
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                tc.compute_and_record_metadosis_fractions(
                    day,
                    U256::from(500_000_000_000u64),
                    completed_at,
                )
            })
            .unwrap();
        storage
            .set_block_timestamp(U256::from(
                completed_at + outbe_primitives::time::SECONDS_PER_DAY,
            ))
            .unwrap();

        let call = crate::precompile::ITribute::createNodCall {
            tributeId: tribute.tribute_id.to_u256(),
        }
        .abi_encode();
        let unpriced =
            crate::precompile::dispatch(storage.clone(), &call, tribute.owner, U256::ZERO);
        let expected_err: PrecompileError = crate::errors::TributeError::EntryPriceUnavailable {
            price_day: outbe_nod::openings::entry_price_day(completed_at).expect("date step"),
            currency: 840,
        }
        .into();
        assert_eq!(unpriced.unwrap_err().to_string(), expected_err.to_string());
        assert!(tc.get_record(tribute.tribute_id).unwrap().is_some());
        assert_eq!(tc.total_supply().unwrap(), 1);
        assert!(outbe_nod::api::get_item(&storage, tribute.tribute_id)
            .unwrap()
            .is_none());

        seed_entry_price(
            &storage,
            840,
            outbe_nod::openings::entry_price_day(completed_at).expect("date step"),
            U256::from(1_234_567u64),
        );
        crate::precompile::dispatch(storage.clone(), &call, tribute.owner, U256::ZERO).unwrap();
        assert!(tc.get_record(tribute.tribute_id).unwrap().is_none());
    });

    let burned = provider
        .get_events(TRIBUTE_ADDRESS)
        .iter()
        .filter(|log| crate::precompile::ITribute::TributeBurned::decode_log_data(log).is_ok())
        .count();
    assert_eq!(burned, 1);
}

#[test]
fn series_sealing_zero_nod_forfeit_returns_unclaimed_load_and_enables_permissionless_clean() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let base_ts = 1_750_000_000u64;
    provider.set_timestamp(U256::from(base_ts));
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 3);

    let day: outbe_primitives::time::WorldwideDay = 20241220u32.into();
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
        in_process_ready(&storage, || tc.seal_day(day));

        tc.day_emission_day.write(&day, emission_day_key).unwrap();
        let lysis_limit = U256::from(500_000_000_000u64);
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        let (_fractions, allocated) = storage
            .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                tc.compute_and_record_metadosis_fractions(day, lysis_limit, base_ts)
            })
            .unwrap();

        let group_key = outbe_nod::identity::series_group_key(emission_day_key, 840, 840);
        let mut nod = outbe_nod::NodContract::new(storage.clone());
        assert_eq!(
            nod.group_unclaimed_tribute_count.read(&group_key).unwrap(),
            2
        );
        assert_eq!(nod.group_nod_count.read(&group_key).unwrap(), 0);
        assert_eq!(
            nod.group_unclaimed_tribute_load.read(&group_key).unwrap(),
            allocated
        );

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

        // Owner1 claims t1 -> moves t1's load from unclaimed_tribute_load to unpaid_load
        let nod_id = tc.create_nod(owner1, t1.tribute_id).unwrap();
        let t1_item = outbe_nod::api::get_item(&storage, nod_id).unwrap().unwrap();
        assert_eq!(t1_item.group_key, group_key);
        let t1_load = outbe_tee_enclave::nod_encryption::decrypt_nod(
            &crate::enclave_client::test_enclave::NETWORK_SECRET,
            &t1_item.encrypted,
        )
        .unwrap();
        assert_eq!(
            nod.group_unclaimed_tribute_count.read(&group_key).unwrap(),
            1
        );
        assert_eq!(nod.group_nod_count.read(&group_key).unwrap(), 1);
        assert_eq!(nod.group_unpaid_load.read(&group_key).unwrap(), t1_load);
        assert_eq!(
            nod.group_unclaimed_tribute_load.read(&group_key).unwrap(),
            allocated - t1_load
        );

        // Cleaning live Nod t1 must revert with NodNotForfeited
        let clean_nod_call = outbe_nod::precompile::INod::cleanCall {
            nodId: nod_id.to_u256(),
        }
        .abi_encode();
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
        let forfeit_ts = deadline + outbe_primitives::time::SECONDS_PER_DAY;
        storage.set_block_timestamp(U256::from(forfeit_ts)).unwrap();
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(10, forfeit_ts, 1),
            storage.clone(),
        );
        let promis_before = outbe_promislimit::PromisLimitContract::new(storage.clone())
            .promis_limit()
            .unwrap();
        let forfeited = outbe_nodfactory::forfeited::sweep_expired(&ctx).unwrap();
        assert_eq!(forfeited, 1);

        // Both unpaid Nod load and unclaimed Tribute load were returned in one credit
        let promis_after = outbe_promislimit::PromisLimitContract::new(storage.clone())
            .promis_limit()
            .unwrap();
        assert_eq!(promis_after - promis_before, allocated);
        assert!(nod.group_is_forfeited.read(&group_key).unwrap());
        assert_eq!(nod.group_unpaid_load.read(&group_key).unwrap(), U256::ZERO);
        assert_eq!(
            nod.group_unclaimed_tribute_load.read(&group_key).unwrap(),
            U256::ZERO
        );

        // Subsequent createNod for unclaimed t2 must revert with TributeForfeited
        let create_t2_err = tc.create_nod(owner2, t2.tribute_id).unwrap_err();
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

#[test]
fn series_zero_minted_nods_automatic_breach_call_forfeit_and_clean() {
    use alloy_sol_types::SolCall;
    use outbe_primitives::time::{
        date_key_to_utc_timestamp, next_date_key, previous_date_key, timestamp_to_date_key,
        SECONDS_PER_DAY,
    };

    let mut provider = HashMapStorageProvider::new(1);
    let emission_day_key = 20250110u32;
    let base_ts = date_key_to_utc_timestamp(emission_day_key) + 3_600;
    provider.set_timestamp(U256::from(base_ts));
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 3);

    let day: outbe_primitives::time::WorldwideDay = 20241220u32.into();
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

        tc.day_emission_day.write(&day, emission_day_key).unwrap();
        let lysis_limit = U256::from(500_000_000_000u64);
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        let (_fractions, allocated) = storage
            .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                tc.compute_and_record_metadosis_fractions(day, lysis_limit, base_ts)
            })
            .unwrap();

        // Unquoted 978 must not have sealed a series; 840 sealed a 0-Nod series with 2 Tributes
        let unquoted_group_key = outbe_nod::identity::series_group_key(emission_day_key, 840, 978);
        assert!(outbe_nod::api::get_group(&storage, unquoted_group_key)
            .unwrap()
            .is_none());
        let clean_unquoted = crate::precompile::ITribute::cleanCall {
            tributeId: t_unquoted.tribute_id.to_u256(),
        }
        .abi_encode();
        crate::precompile::dispatch(storage.clone(), &clean_unquoted, stranger, U256::ZERO)
            .unwrap();
        assert!(tc.get_record(t_unquoted.tribute_id).unwrap().is_none());

        let group_key = outbe_nod::identity::series_group_key(emission_day_key, 840, 840);
        assert_eq!(
            nod_contract
                .group_unclaimed_tribute_count
                .read(&group_key)
                .unwrap(),
            2
        );
        assert_eq!(nod_contract.group_nod_count.read(&group_key).unwrap(), 0);
        assert_eq!(
            nod_contract
                .group_unclaimed_tribute_load
                .read(&group_key)
                .unwrap(),
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
        nod_contract.daily_genesis_day.write(latest_closed).unwrap();

        // Run explicit call maintenance with no minted Nods.
        let call_ts = date_key_to_utc_timestamp(cursor_day) + 3_600;
        storage.set_block_timestamp(U256::from(call_ts)).unwrap();
        let call_ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(20, call_ts, 1),
            storage.clone(),
        );
        outbe_nodfactory::called::schedule_call_pass(&call_ctx).unwrap();
        let called = outbe_nodfactory::called::scan_and_call(
            &call_ctx,
            outbe_oracle::call_pass::Selection {
                process_call: true,
                process_forfeit: true,
                process_cleanup: true,
            },
        )
        .unwrap();
        assert_eq!(called, 1);
        assert_eq!(
            nod_contract.group_called_at.read(&group_key).unwrap(),
            call_ts
        );

        // Advance past the call notice period and explicitly process forfeiture.
        let terms = nod_contract.read_call_terms(group_key).unwrap();
        let deadline =
            outbe_nod::api::settlement_deadline_of(call_ts, terms.call_notice_period_seconds);
        let forfeit_ts = deadline + SECONDS_PER_DAY;
        storage.set_block_timestamp(U256::from(forfeit_ts)).unwrap();
        let forfeit_ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(21, forfeit_ts, 1),
            storage.clone(),
        );
        let promis_before = outbe_promislimit::PromisLimitContract::new(storage.clone())
            .promis_limit()
            .unwrap();
        let forfeited = outbe_nodfactory::forfeited::sweep_expired(&forfeit_ctx).unwrap();
        assert_eq!(forfeited, 1);
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
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 8);

    let day_zero_load: outbe_primitives::time::WorldwideDay = 20241221u32.into();
    let day_aborted: outbe_primitives::time::WorldwideDay = 20241222u32.into();
    let day_retired: outbe_primitives::time::WorldwideDay = 20241223u32.into();
    let owner1 = address!("0x1111111111111111111111111111111111111111");
    let owner2 = address!("0x2222222222222222222222222222222222222222");
    let stranger = address!("0x9999999999999999999999999999999999999999");

    StorageHandle::enter(&mut provider, |storage| {
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
            .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                tc.compute_and_record_metadosis_fractions(
                    day_zero_load,
                    U256::from(100_000u64),
                    base_ts,
                )
            })
            .unwrap();
        assert!(allocated <= U256::from(100_000u64));

        // 2. P4.4c: INod.clean on a settled Nod reverts with NodAlreadySettled
        let nod_id = tc.create_nod(owner2, t_normal.tribute_id).unwrap();
        let nod_contract = outbe_nod::NodContract::new(storage.clone());
        let mut stored_nod = nod_contract.records.get(nod_id).unwrap().unwrap();
        stored_nod.is_settled = true;
        nod_contract.records.update(&stored_nod).unwrap();
        let clean_nod_call = outbe_nod::precompile::INod::cleanCall {
            nodId: nod_id.to_u256(),
        }
        .abi_encode();
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
            .write(&day_aborted, aborted_emission_day)
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
        assert!(tc.get_fi_groups(day_aborted).unwrap().is_empty());

        // 4. P4.4h: ITribute.clean on a retired partition cleans fi_group_counts and fi_group_nominals
        let mut t_retired = sample_tribute();
        set_owner(&mut t_retired, owner1);
        set_day(&mut t_retired, day_retired);
        tc.issue_with_league(&t_retired, Some(300)).unwrap();
        in_process_ready(&storage, || tc.seal_day(day_retired));
        in_process_ready(&storage, || {
            tc.forfeit_sealed_partition(day_retired).map(|_| ())
        });
        assert!(!tc.get_fi_groups(day_retired).unwrap().is_empty());
        let clean_retired = crate::precompile::ITribute::cleanCall {
            tributeId: t_retired.tribute_id.to_u256(),
        }
        .abi_encode();
        crate::precompile::dispatch(storage.clone(), &clean_retired, stranger, U256::ZERO).unwrap();
        assert!(tc.get_fi_groups(day_retired).unwrap().is_empty());
    });
}
