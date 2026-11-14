use super::{in_process_ready, sample_tribute, set_day, set_owner};
use crate::TributeContract;
use alloy_primitives::{address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::storage::{CycleLifecyclePurpose, MutationPurposeTag};
use outbe_system_common::addresses::TRIBUTE_ADDRESS;
use outbe_system_common::metadosis_bindings::metadosis_process_ready_binding;
use outbe_system_common::time::EmissionDate;

// These fixtures share a chain that predates their December 2024 WorldwideDays.
const GENESIS_TIMESTAMP: u64 = 1_733_011_200; // 2024-12-01 00:00 UTC

/// The id `issueNod` gives the Tribute's Nod: the owner in the series of its price day.
fn nod_id_of(tc: &TributeContract<'_>, tribute: &crate::schema::TributeData) -> U256 {
    let series = outbe_core_common::series_id::SeriesId::for_pair(
        tc.lysis_emission_day(tribute.worldwide_day)
            .unwrap()
            .unwrap()
            .value(),
        tribute.issuance_currency,
        tribute.reference_currency,
    )
    .unwrap();
    outbe_core_common::nod_rules::derive_nod_id(series, tribute.owner)
}

/// Seals the Lysis of `day` for its linked emission day, or the price day of `completed_at`.
fn seal_lysis(
    tc: &mut TributeContract<'_>,
    day: outbe_system_common::time::WorldwideDay,
    metadosis_limit: U256,
    completed_at: u64,
) -> outbe_primitives::error::Result<(U256, U256)> {
    let storage = tc.storage_handle();
    let emission_day = match tc.lysis_emission_day(day)? {
        Some(emission_day) => emission_day,
        None => EmissionDate::new(outbe_nod::openings::entry_price_day(completed_at).unwrap()),
    };
    let priced = outbe_oracle::api::priced_reference_currencies_of_day(
        storage.clone(),
        emission_day.value(),
    )?
    .into_iter()
    .filter(|&(_, vwap)| outbe_nod::pricing::is_issuable_entry(vwap))
    .collect();
    let outcome = tc.compute_and_record_metadosis_fractions(
        day,
        emission_day,
        &priced,
        metadosis_limit,
        completed_at,
    )?;
    Ok((outcome.nominal_minor, outcome.allocated_minor))
}

/// Prices `iso` on `utc_day` and on the day before it.
fn seed_entry_price(storage: &StorageHandle<'_>, iso: u16, utc_day: u32, vwap: U256) {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(iso);
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    let index = match oracle.pair_index_of(pair).unwrap() {
        0 => outbe_oracle::api::register_pair(storage.clone(), pair).unwrap(),
        index => index,
    };
    for day in [
        outbe_system_common::time::previous_date_key(utc_day).unwrap(),
        utc_day,
    ] {
        oracle.record_utc_day_vwap(day, index, vwap).unwrap();
    }
}

fn test_day_league_fraction(
    tc: &TributeContract<'_>,
    day: outbe_system_common::time::WorldwideDay,
    league: u16,
) -> u64 {
    let Some(emission_day) = tc.lysis_emission_day(day).unwrap() else {
        return 0;
    };
    let Some(record) = tc.day_fractions_record(emission_day).unwrap() else {
        return 0;
    };
    let fractions = crate::enclave_client::read_day_fractions(&record).unwrap();
    fractions.get(&league).copied().unwrap_or(0)
}

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
        in_process_ready(&storage, || tc.seal_day(day));
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
        let nod_view_key = outbe_enclave::nod_encryption::derive_view_key(
            &crate::enclave_client::test_enclave::NETWORK_SECRET,
            owner1,
        )
        .unwrap();
        let owner_decrypted =
            outbe_enclave::nod_decrypt::decrypt_nod_for_owner(&nod_view_key, &nod_item.encrypted)
                .expect("owner must decrypt real encrypted Nod");
        let enclave_decrypted = outbe_enclave::nod_encryption::decrypt_nod(
            &crate::enclave_client::test_enclave::NETWORK_SECRET,
            &nod_item.encrypted,
        )
        .expect("enclave must decrypt real encrypted Nod");
        assert!(!owner_decrypted.is_zero());
        assert_eq!(owner_decrypted, enclave_decrypted);
        assert!(
            outbe_enclave::nod_decrypt::decrypt_nod_for_owner(&[0x78; 32], &nod_item.encrypted,)
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
        in_process_ready(&storage, || tc.seal_day(day));
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
            in_process_ready(&storage, || tc.seal_day(sealed));
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
            outbe_enclave::nod_mine::IssueNodFromTributeRequestV1 {
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
        in_process_ready(&storage, || tc.seal_day(day));
        in_process_ready(&storage, || tc.freeze_day_nominal(&tc.get_day_totals(day)?));
        let mut ids = [first.tribute_id, second.tribute_id];
        ids.sort();
        let inputs: Vec<_> = ids
            .iter()
            .map(|id| outbe_enclave::tribute_day::LysisTributeInputV1 {
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
                outbe_enclave::tribute_day::LysisFractionsRequestV1 {
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
        let t1_load = outbe_enclave::nod_encryption::decrypt_nod(
            &crate::enclave_client::test_enclave::NETWORK_SECRET,
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

#[test]
fn multi_league_lysis_and_issue_nod_keep_league_nominals_fractions_and_loads_private() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let base_ts = 1_750_000_000u64;
    provider.set_timestamp(U256::from(base_ts));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 3);

    let day: outbe_system_common::time::WorldwideDay = 20241221u32.into();
    let owner1 = address!("0x1111111111111111111111111111111111111111");
    let owner2 = address!("0x2222222222222222222222222222222222222222");
    let owner3 = address!("0x3333333333333333333333333333333333333333");

    let (nod1_id, nod2_id, f1, f2, load1, load2, cost1, cost2) =
        StorageHandle::enter(&mut provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            let mut tc = TributeContract::new(storage.clone());

            let mut t1 = sample_tribute();
            set_owner(&mut t1, owner1);
            set_day(&mut t1, day);
            t1.issuance_amount_minor = U256::from(400_000_000u64);
            t1.nominal_amount_minor = U256::from(200_000_000u64);

            let mut t2 = sample_tribute();
            set_owner(&mut t2, owner2);
            set_day(&mut t2, day);
            t2.issuance_amount_minor = U256::from(800_000_000u64);
            t2.nominal_amount_minor = U256::from(400_000_000u64);

            let mut t3 = sample_tribute();
            set_owner(&mut t3, owner3);
            set_day(&mut t3, day);
            t3.issuance_amount_minor = U256::from(800_000_000u64);
            t3.nominal_amount_minor = U256::from(400_000_000u64);

            let price_day = outbe_nod::openings::entry_price_day(base_ts).unwrap();
            seed_entry_price(&storage, 840, price_day, U256::from(2_000_000u64));
            tc.issue_with_league(&t1, Some(100)).unwrap();
            tc.issue_with_league(&t2, Some(200)).unwrap();
            tc.issue_with_league(&t3, Some(200)).unwrap();
            in_process_ready(&storage, || tc.seal_day(day));

            let binding = metadosis_process_ready_binding(1, 0, base_ts);
            storage
                .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
                    seal_lysis(&mut tc, day, U256::from(500_000_000u64), base_ts)
                })
                .unwrap();

            outbe_emission::api::link_wwd(storage.clone(), price_day, day).unwrap();
            let totals_call = crate::precompile::ITribute::getDayTotalsCall {
                emissionDay: price_day,
            }
            .abi_encode();
            let totals_before = crate::precompile::ITribute::getDayTotalsCall::abi_decode_returns(
                &crate::precompile::dispatch(storage.clone(), &totals_call, owner1, U256::ZERO)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(totals_before.tributeCount, 3);
            assert!(totals_before.isSealed);

            let series =
                outbe_core_common::series_id::SeriesId::for_pair(price_day, 840, 840).unwrap();
            let group_key = outbe_nod::api::series_group(&storage, series)
                .unwrap()
                .unwrap();
            let nod_contract = outbe_nod::NodContract::new(storage.clone());
            let open_load_ct_before = nod_contract.encrypted_group_open_load(group_key).unwrap();
            assert!(!open_load_ct_before.is_empty());

            assert!(!tc
                .encrypted_day_fractions(EmissionDate::new(price_day))
                .unwrap()
                .is_empty());

            let f1 = test_day_league_fraction(&tc, day, 100);
            let f2 = test_day_league_fraction(&tc, day, 200);
            assert!(f1 > 0 && f2 > 0 && f1 != f2);
            let nod1_id = tc.issue_nod(owner1, t1.tribute_id).unwrap();

            // `group_open_load_ct` and `tributeNominalTotalMinor` must be completely unchanged after `issueNod`.
            let open_load_ct_after = nod_contract.encrypted_group_open_load(group_key).unwrap();
            assert_eq!(open_load_ct_after, open_load_ct_before);
            let totals_after = crate::precompile::ITribute::getDayTotalsCall::abi_decode_returns(
                &crate::precompile::dispatch(storage.clone(), &totals_call, owner1, U256::ZERO)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(totals_after.tributeCount, 2);
            assert_eq!(
                totals_after.tributeNominalTotalMinor,
                totals_before.tributeNominalTotalMinor
            );

            let nod2_id = tc.issue_nod(owner2, t2.tribute_id).unwrap();
            let rec1 = outbe_nod::api::get_item(&storage, nod1_id)
                .unwrap()
                .unwrap();
            let rec2 = outbe_nod::api::get_item(&storage, nod2_id)
                .unwrap()
                .unwrap();
            let view_key1 = outbe_enclave::nod_encryption::derive_view_key(
                &crate::enclave_client::test_enclave::NETWORK_SECRET,
                owner1,
            )
            .unwrap();
            let view_key2 = outbe_enclave::nod_encryption::derive_view_key(
                &crate::enclave_client::test_enclave::NETWORK_SECRET,
                owner2,
            )
            .unwrap();
            let load1 =
                outbe_enclave::nod_decrypt::decrypt_nod_for_owner(&view_key1, &rec1.encrypted)
                    .unwrap();
            let load2 =
                outbe_enclave::nod_decrypt::decrypt_nod_for_owner(&view_key2, &rec2.encrypted)
                    .unwrap();
            assert_eq!(
                load1,
                t1.nominal_amount_minor * U256::from(f1) / U256::from(1_000_000u64)
            );
            assert_eq!(
                load2,
                t2.nominal_amount_minor * U256::from(f2) / U256::from(1_000_000u64)
            );
            let cost1 =
                outbe_nod::api::settlement_cost_minor(U256::from(2_000_000u64), load1).unwrap();
            let cost2 =
                outbe_nod::api::settlement_cost_minor(U256::from(2_000_000u64), load2).unwrap();
            (nod1_id, nod2_id, f1, f2, load1, load2, cost1, cost2)
        });

    let settlement_asset = address!("0x7171717171717171717171717171717171717171");
    let asset_vaults_count_selector: [u8; 4] =
        alloy_primitives::keccak256(b"assetVaultsCount(address)")[..4]
            .try_into()
            .unwrap();
    provider.stub_sub_call_at_selector(
        outbe_system_common::addresses::VAULT_ROUTER_ADDRESS,
        asset_vaults_count_selector,
        alloy_primitives::Bytes::from(U256::ONE.to_be_bytes::<32>()),
    );
    provider.stub_sub_call_at_selector(
        settlement_asset,
        outbe_nodfactory::sol_ext::IReferenceCurrency::isoCodeCall::SELECTOR,
        alloy_primitives::Bytes::from(
            outbe_nodfactory::sol_ext::IReferenceCurrency::isoCodeCall::abi_encode_returns(&840u16),
        ),
    );
    provider.stub_sub_call_at_selector(
        settlement_asset,
        outbe_nodfactory::sol_ext::IERC20::decimalsCall::SELECTOR,
        alloy_primitives::Bytes::from(
            outbe_nodfactory::sol_ext::IERC20::decimalsCall::abi_encode_returns(&6u8),
        ),
    );
    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let quote_call = outbe_nodfactory::precompile::INodFactory::quoteSettlementCall {
            nodId: nod1_id,
            asset: settlement_asset,
        }
        .abi_encode();
        let non_owner_err = outbe_nodfactory::precompile::dispatch(
            storage.clone(),
            &quote_call,
            owner2,
            U256::ZERO,
        )
        .unwrap_err();
        assert_eq!(
            non_owner_err.to_string(),
            PrecompileError::from(outbe_nodfactory::errors::NodFactoryError::NotNodOwner)
                .to_string()
        );
        let quote_raw =
            outbe_nodfactory::precompile::dispatch(storage, &quote_call, owner1, U256::ZERO)
                .unwrap();
        let quote_ret =
            outbe_nodfactory::precompile::INodFactory::quoteSettlementCall::abi_decode_returns(
                &quote_raw,
            )
            .unwrap();
        let view_key1 = outbe_enclave::nod_encryption::derive_view_key(
            &crate::enclave_client::test_enclave::NETWORK_SECRET,
            owner1,
        )
        .unwrap();
        let decrypted_quote = outbe_enclave::nod_decrypt::decrypt_nod_quote_for_owner(
            &view_key1,
            &outbe_core_common::nod_encryption::NodQuoteContextV1 {
                chain_id: 1,
                nod_id: nod1_id,
                owner: owner1,
                asset: settlement_asset,
                settlement_currency: quote_ret.settlementCurrency,
                snapshot_id: quote_ret.snapshotId,
            },
            &quote_ret.encryptedPaymentMinor,
        )
        .unwrap();
        assert_eq!(decrypted_quote, cost1);
    });

    let forbidden_words = [
        U256::from(200_000_000u64),
        U256::from(400_000_000u64),
        U256::from(800_000_000u64),
        U256::from(f1),
        U256::from(f2),
        load1,
        load2,
        cost1,
        cost2,
    ];
    for ((addr, _slot), value) in &provider.storage {
        if *addr == TRIBUTE_ADDRESS || *addr == outbe_system_common::addresses::NOD_ADDRESS {
            assert!(
                !forbidden_words.contains(value),
                "sensitive value {value} leaked into storage of {addr}"
            );
        }
    }
    StorageHandle::enter(&mut provider, |storage| {
        for nod_id in [nod1_id, nod2_id] {
            let call = outbe_nod::precompile::INod::getNodCall { nodId: nod_id }.abi_encode();
            let raw = outbe_nod::precompile::dispatch(storage.clone(), &call, owner1, U256::ZERO)
                .unwrap();
            for chunk in raw.chunks_exact(32) {
                let word = U256::from_be_slice(chunk);
                assert!(
                    !forbidden_words.contains(&word),
                    "sensitive value {word} leaked in getNod({nod_id})"
                );
            }
        }
    });
}

/// Runs the Nod daily pass at `timestamp` through the UTC day before it, one day per block
/// from `height`, as the Cycle does.
fn run_nod_daily_pass(storage: &StorageHandle<'_>, mut height: u64, timestamp: u64) {
    use outbe_system_common::time::{previous_date_key, timestamp_to_date_key};

    let closed = previous_date_key(timestamp_to_date_key(timestamp)).unwrap();
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < closed {
        oracle.utc_day_vwap_last_finalized.write(closed).unwrap();
    }
    let nod = outbe_nod::NodContract::new(storage.clone());
    if nod.call_latest_day.read().unwrap() == 0 {
        // This fixture starts from an established parent with only the newest day pending.
        nod.call_latest_day
            .write(previous_date_key(closed).unwrap())
            .unwrap();
    }
    loop {
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: height,
                timestamp,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: GENESIS_TIMESTAMP,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        outbe_nodfactory::called::process_call_pass(&ctx).unwrap();
        let selected = outbe_system_common::pass_budget::BlockAllowance::unpack(
            nod.daily_budget.read().unwrap(),
        )
        .unwrap()
        .map_or(0, |allowance| allowance.day);
        let next = nod.call_sweep_day.read().unwrap();
        if next == 0 || next == selected {
            assert!(
                next == 0 || selected == nod.call_latest_day.read().unwrap(),
                "the pass stopped on day {selected} before the last closed day"
            );
            break;
        }
        height += 1;
    }
}
