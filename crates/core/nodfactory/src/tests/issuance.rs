use super::*;

#[test]
fn nod_pow_binds_owner_and_zero_sequence() {
    let owner = Address::repeat_byte(0x11);
    let other = Address::repeat_byte(0x22);
    let nod_id = outbe_nod::test_support::nod_id_of(owner, 20_241_201, 840, 840);
    let nonce = 42;
    let bound = runtime::compute_pow_hash(nod_id, owner, nonce);
    assert_ne!(bound, runtime::compute_pow_hash(nod_id, other, nonce));
    assert_ne!(
        bound,
        outbe_core_common::pow::compute_mining_pow_hash(
            outbe_core_common::pow::MiningDomain::Nod,
            nod_id,
            owner,
            1,
            nonce,
        )
    );
    let solved = find_valid_nonce(nod_id, owner);
    assert!(runtime::validate_pow(nod_id, owner, solved).is_ok());
    assert_ne!(
        runtime::compute_pow_hash(nod_id, owner, solved),
        runtime::compute_pow_hash(nod_id, other, solved)
    );
}

#[test]
fn issue_is_immediately_readable_and_keeps_product_event_order() {
    let mut world = World::new();
    let input = params(address!("1111111111111111111111111111111111111111"));
    let nod_id = world.issue(&input);
    let item = world
        .enter(|storage| nod_api::get_item(&storage, nod_id))
        .unwrap()
        .unwrap();
    assert_eq!(item.owner, input.owner);
    assert_eq!(
        world
            .enter(|storage| { nod_api::list_by_owner(&storage, input.owner) })
            .unwrap()
            .len(),
        1
    );

    let events: Vec<_> = world
        .provider
        .get_ordered_events()
        .iter()
        .filter(|event| event.address == NOD_ADDRESS || event.address == NOD_FACTORY_ADDRESS)
        .map(|event| (event.address, event.data.topics()[0]))
        .collect();
    assert_eq!(
        events,
        [
            (NOD_ADDRESS, INod::Transfer::SIGNATURE_HASH),
            (NOD_FACTORY_ADDRESS, INodFactory::NodIssued::SIGNATURE_HASH),
        ]
    );
}

#[test]
fn second_same_block_issue_reuses_the_group() {
    let mut world = World::new();
    let first = params(Address::repeat_byte(0x18));
    let second = params(Address::repeat_byte(0x19));
    let first_id = world.issue(&first);
    let second_id = world.issue(&second);
    assert_ne!(first_id, second_id);

    let (first_item, second_item) = world
        .enter(|storage| {
            outbe_primitives::error::Result::Ok((
                nod_api::get_item(&storage, first_id)?.unwrap(),
                nod_api::get_item(&storage, second_id)?.unwrap(),
            ))
        })
        .unwrap();
    let group_key = first_item.group_key;
    assert_eq!(second_item.group_key, group_key);
    assert_eq!(first_item.entry_price_minor(), first.entry_price_minor);
    assert_eq!(
        world
            .enter(|storage| NodContract::new(storage).group_nod_count.read(&group_key))
            .unwrap(),
        2
    );
    for owner in [first_item.owner, second_item.owner] {
        assert_eq!(
            world
                .enter(|storage| nod_api::balance_of(&storage, owner))
                .unwrap(),
            1
        );
    }
}

#[test]
fn invalid_and_duplicate_issuance_leave_one_canonical_item() {
    let mut world = World::new();
    let mut invalid = params(Address::ZERO);
    let error = world
        .enter(|storage| {
            api::issue_nod(
                &storage,
                &outbe_nod::test_support::encrypted_fixture(&invalid, CHAIN_ID),
            )
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        PrecompileError::from(NodFactoryError::InvalidOwner).to_string()
    );

    invalid.owner = Address::repeat_byte(0x22);
    let nod_id = world.issue(&invalid);
    assert!(world
        .enter(|storage| api::issue_nod(
            &storage,
            &outbe_nod::test_support::encrypted_fixture(&invalid, CHAIN_ID)
        ))
        .is_err());
    assert!(world
        .enter(|storage| nod_api::get_item(&storage, nod_id))
        .unwrap()
        .is_some());
}

#[test]
fn direct_issuance_beyond_the_issuable_entry_writes_nothing() {
    let mut world = World::new();
    let mut input = params(Address::repeat_byte(0x2E));
    input.entry_price_minor = U256::MAX / U256::from(100 + u32::from(u16::MAX)) + U256::from(1);
    assert!(!outbe_nod::pricing::is_issuable_entry(
        input.entry_price_minor
    ));
    let storage_before = world.provider.storage.clone();
    let events_before = world.provider.get_ordered_events().len();

    let error = world
        .enter(|storage| {
            api::issue_nod(
                &storage,
                &outbe_nod::test_support::encrypted_fixture(&input, CHAIN_ID),
            )
        })
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        PrecompileError::from(NodFactoryError::EntryPriceOutOfBounds).to_string()
    );
    assert_eq!(world.provider.storage, storage_before);
    assert_eq!(world.provider.get_ordered_events().len(), events_before);
    let nod_id = outbe_nod::test_support::nod_id_of(
        input.owner,
        input.emission_day,
        input.issuance_currency,
        input.reference_currency,
    );
    assert!(world
        .enter(|storage| nod_api::get_item(&storage, nod_id))
        .unwrap()
        .is_none());
}

#[test]
fn failed_authorization_preserves_the_loaded_nod() {
    let mut world = World::new();
    let input = free(Address::repeat_byte(0x33));
    let nod_id = world.issue(&input);
    world.settle(nod_id, input.owner).unwrap();
    let nonce = world.pow_nonce(nod_id);
    world
        .enter(|storage| {
            api::mine_gratis(
                &storage,
                api::MineGratisRequest {
                    caller: Address::repeat_byte(0x44),
                    nod_id,
                    nonce,
                    auth: dummy_auth(),
                },
            )
        })
        .unwrap_err();
    // Dummy MAC is rejected regardless of who submits; the paid Nod remains.
    assert!(world
        .enter(|storage| nod_api::get_item(&storage, nod_id))
        .unwrap()
        .is_some());
}

#[test]
fn invalid_gratis_mac_rolls_back_the_nod_burn() {
    let mut world = World::new();
    let input = free(Address::repeat_byte(0x45));
    let nod_id = world.issue(&input);
    world.settle(nod_id, input.owner).unwrap();
    let nonce = world.pow_nonce(nod_id);

    world
        .enter(|storage| {
            api::mine_gratis(
                &storage,
                api::MineGratisRequest {
                    caller: input.owner,
                    nod_id,
                    nonce,
                    auth: dummy_auth(),
                },
            )
        })
        .unwrap_err();
    assert!(world
        .enter(|storage| nod_api::get_item(&storage, nod_id))
        .unwrap()
        .is_some());
}

#[test]
fn mining_deletes_item_and_last_group_then_emits_burn() {
    let mut world = World::new();
    let input = free(Address::repeat_byte(0x55));
    let nod_id = world.issue(&input);
    let group_key = world
        .enter(|storage| nod_api::get_item(&storage, nod_id))
        .unwrap()
        .unwrap()
        .group_key;
    world.provider.clear_events(NOD_ADDRESS);
    world.provider.clear_events(NOD_FACTORY_ADDRESS);
    world.settle(nod_id, input.owner).unwrap();
    let nonce = world.pow_nonce(nod_id);
    let minted = world
        .mine_gratis(api::MineGratisRequest {
            caller: input.owner,
            nod_id,
            nonce,
            auth: mine_auth(input.owner, input.gratis_load_minor),
        })
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
    assert!(world
        .enter(|storage| nod_api::get_item(&storage, nod_id))
        .unwrap()
        .is_none());
    assert!(world
        .enter(|storage| { nod_api::get_group(&storage, group_key) })
        .unwrap()
        .is_none());

    let signatures: Vec<_> = world
        .provider
        .get_ordered_events()
        .iter()
        .filter(|event| event.address == NOD_ADDRESS || event.address == NOD_FACTORY_ADDRESS)
        .map(|event| (event.address, event.data.topics()[0]))
        .collect();
    assert_eq!(
        signatures,
        [
            (NOD_ADDRESS, INod::MetadataUpdate::SIGNATURE_HASH),
            (NOD_FACTORY_ADDRESS, INodFactory::NodSettled::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::Transfer::SIGNATURE_HASH),
        ]
    );
}

#[test]
fn a_hundred_dollars_converts_to_ninety_euros_at_every_asset_scale() {
    // 100 USD = entry 2.00 x load 50; R = 2.00 USD/COEN, I = 1.80 EUR/COEN.
    let rate = Some((U256::from(1_800_000), U256::from(2_000_000)));
    for (decimals, expected) in [
        (6, U256::from(90_000_000u64)),
        (8, U256::from(9_000_000_000u64)),
        (
            18,
            U256::from(90u64) * U256::from(10u64).pow(U256::from(18)),
        ),
    ] {
        assert_eq!(
            crate::runtime::settlement_units(
                U256::from(2_000_000),
                U256::from(50_000_000),
                rate,
                decimals
            )
            .unwrap(),
            expected,
            "{decimals} decimals"
        );
    }
}

#[test]
fn a_stranger_can_mine_with_the_owners_auth() {
    let mut world = World::new();
    let input = free(Address::repeat_byte(0x6a));
    let nod_id = world.issue(&input);
    world.settle(nod_id, input.owner).unwrap();
    let nonce = world.pow_nonce(nod_id);
    let stranger = Address::repeat_byte(0x6b);

    let auth = mine_auth(input.owner, input.gratis_load_minor);
    let call = INodFactory::mineGratisCall {
        nodId: nod_id,
        nonce,
        mac: B256::from(auth.mac),
        opNonce: auth.op_nonce,
    };
    let minted = world
        .enter(|storage| {
            crate::precompile::dispatch(storage, &call.abi_encode(), stranger, U256::ZERO)
        })
        .unwrap();
    let minted = INodFactory::mineGratisCall::abi_decode_returns(&minted).unwrap();
    assert_eq!(
        decrypt_gratis(input.owner, &minted),
        input.gratis_load_minor
    );
    world.enter(|storage| {
        assert_eq!(
            gratis_balance(&storage, input.owner),
            input.gratis_load_minor
        );
        assert!(outbe_gratis::api::balance_ct(storage.clone(), stranger)
            .unwrap()
            .is_empty());
        assert_eq!(
            outbe_gratis::api::op_nonce(storage.clone(), input.owner).unwrap(),
            1
        );
        assert_eq!(
            outbe_gratis::api::op_nonce(storage.clone(), stranger).unwrap(),
            0
        );
        assert!(nod_api::get_item(&storage, nod_id).unwrap().is_none());
    });
    let mined = world
        .provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| {
            outbe_gratis::precompile::IGratis::GratisMinedFromNod::decode_log_data(&event.data).ok()
        })
        .last()
        .expect("GratisMinedFromNod event");
    assert_eq!(mined.account, input.owner);
    assert_eq!(mined.nodId, nod_id);
}

#[test]
fn lysis_series_of_one_day_and_reference_share_one_call_group() {
    use outbe_primitives::series_id::SeriesId;
    let _enclave = outbe_nod::test_support::scope();
    let mut world = World::new();
    let sealed_at = outbe_primitives::time::date_key_to_utc_timestamp(20_261_020).unwrap() + 3_600;
    let (nod1, called_at, deadline) = world.enter(|storage| {
        let seal = |series_id, issuance_currency, count, load: u64| {
            let sealed_load =
                outbe_nod::api::seal_series_load(&storage, series_id, U256::from(load)).unwrap();
            crate::api::seal_lysis_series(
                &storage,
                crate::api::LysisSeriesInput {
                    series_id,
                    sealed_at,
                    issuance_currency,
                    reference_currency: 840,
                    entry_price_minor: U256::from(500_000),
                    unclaimed_tribute_count: count,
                    unclaimed_tribute_load: sealed_load,
                },
            )
        };
        let usd = SeriesId::for_pair(20_261_019, 840, 840).unwrap();
        let eur = SeriesId::for_pair(20_261_019, 978, 840).unwrap();
        seal(usd, 840, 2, 30).unwrap();
        seal(eur, 978, 1, 10).unwrap();

        let group = nod_api::series_group(&storage, usd).unwrap().unwrap();
        assert_eq!(nod_api::series_group(&storage, eur).unwrap(), Some(group));
        let nod = outbe_nod::NodContract::new(storage.clone());
        assert_eq!(nod.group_unclaimed_tribute_count.read(&group).unwrap(), 3);
        assert_eq!(nod.read_group_open_load(group).unwrap(), U256::from(40));
        assert_eq!(
            nod_api::get_group(&storage, group)
                .unwrap()
                .unwrap()
                .full_day_after_issue,
            WorldwideDay::new(20_261_021),
            "the call counts from the first full day after Lysis"
        );
        assert!(seal(usd, 840, 1, 5).is_err(), "a series seals once");

        let mut p1 = params(Address::repeat_byte(0x71));
        p1.emission_day = 20_261_019;
        p1.entry_price_minor = U256::from(500_000);
        p1.gratis_load_minor = U256::from(15);
        let enc1 = outbe_nod::test_support::encrypted_fixture(&p1, CHAIN_ID);
        let nod1 = crate::api::issue_tribute_nod_in_series(&storage, &enc1, usd).unwrap();

        let mut p2 = p1.clone();
        p2.owner = Address::repeat_byte(0x72);
        let enc2 = outbe_nod::test_support::encrypted_fixture(&p2, CHAIN_ID);
        let nod2 = crate::api::issue_tribute_nod_in_series(&storage, &enc2, usd).unwrap();
        assert_eq!(nod_api::series_group(&storage, usd).unwrap(), Some(group));
        assert_eq!(nod_api::series_group(&storage, eur).unwrap(), Some(group));

        let loaded_item = nod_api::load_item(&storage, nod1).unwrap().unwrap();
        let loaded_group = nod_api::load_group(&storage, group).unwrap().unwrap();
        nod_api::record_settled(&storage, loaded_item, loaded_group).unwrap();

        let terms = nod.read_call_terms(group).unwrap();
        let called_at = sealed_at + 86_400;
        let deadline = nod_api::settlement_deadline_of(called_at, terms.call_notice_period_seconds);
        let mut mut_nod = outbe_nod::NodContract::new(storage.clone());
        crate::called::mark_called(
            &mut mut_nod,
            group,
            called_at,
            terms.call_notice_period_seconds,
        )
        .unwrap();
        let forfeit_ts = deadline + 86_400;
        let through_day = outbe_core_common::deadline_queue::deadline_day(forfeit_ts).unwrap();
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 10,
                timestamp: forfeit_ts,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: deadline - outbe_primitives::time::SECONDS_PER_DAY,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        let mut budget = outbe_primitives::pass_budget::PassBudget::new(100, 100);
        crate::forfeited::forfeit_through(&ctx, through_day, &mut budget).unwrap();
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .promis_limit()
                .unwrap(),
            U256::from(25)
        );
        let _ = nod2;
        (nod1, called_at, deadline)
    });
    let opened: Vec<_> = world
        .provider
        .get_events(NOD_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| INodFactory::NodSeriesOpened::decode_log_data(log).ok())
        .collect();
    assert_eq!(opened.len(), 2);
    assert_eq!(opened[0].issuanceCurrency, 840);
    assert_eq!(opened[0].referenceCurrency, 840);
    assert_eq!(opened[0].entryPriceMinor, U256::from(500_000));
    assert_eq!(opened[1].issuanceCurrency, 978);
    assert_eq!(opened[1].referenceCurrency, 840);

    let issued: Vec<_> = world
        .provider
        .get_events(NOD_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| INodFactory::NodIssued::decode_log_data(log).ok())
        .collect();
    assert_eq!(issued.len(), 2);
    assert_eq!(issued[0].issuanceCurrency, 840);
    assert_eq!(issued[0].referenceCurrency, 840);
    assert_eq!(
        issued[0].payload.len(),
        outbe_primitives::nod_encryption::NOD_BLOB_LEN
    );
    let opened_load = world.enter(|storage| {
        let item = nod_api::get_item(&storage, nod1).unwrap().unwrap();
        assert_eq!(
            issued[0].payload.as_ref(),
            item.encrypted.encrypted_gratis_amount.as_slice()
        );
        let view_key = outbe_enclave::nod_encryption::derive_view_key(
            &outbe_nod::test_support::NETWORK_SECRET,
            item.owner,
        )
        .expect("owner view key");
        outbe_tee::nod_decrypt::decrypt_nod_for_owner(&view_key, &item.encrypted)
            .expect("open nod load")
    });
    assert_eq!(opened_load, U256::from(15));

    let called_series: Vec<_> = world
        .provider
        .get_events(NOD_ADDRESS)
        .iter()
        .filter_map(|log| INod::NodSeriesCalled::decode_log_data(log).ok())
        .collect();
    assert_eq!(called_series.len(), 2);
    assert_eq!(
        (
            called_series[0].issuanceCurrency,
            called_series[0].referenceCurrency,
            called_series[0].calledAt,
            called_series[0].settlementDeadline
        ),
        (840, 840, called_at, deadline)
    );
    assert_eq!(
        (
            called_series[1].issuanceCurrency,
            called_series[1].referenceCurrency,
            called_series[1].calledAt,
            called_series[1].settlementDeadline
        ),
        (978, 840, called_at, deadline)
    );

    let closed: Vec<_> = world
        .provider
        .get_events(NOD_ADDRESS)
        .iter()
        .filter_map(|log| INod::NodSeriesClosed::decode_log_data(log).ok())
        .collect();
    assert_eq!(closed.len(), 2);
    assert_eq!(
        (
            closed[0].issuanceCurrency,
            closed[0].referenceCurrency,
            closed[0].forfeitedCount,
            closed[0].forfeitedTributeCount,
            closed[0].forfeitedAmountMinor
        ),
        (840, 840, 1, 0, U256::from(15))
    );
    assert_eq!(
        (
            closed[1].issuanceCurrency,
            closed[1].referenceCurrency,
            closed[1].forfeitedCount,
            closed[1].forfeitedTributeCount,
            closed[1].forfeitedAmountMinor
        ),
        (978, 840, 0, 1, U256::from(10))
    );
}

#[test]
fn series_group_stays_after_the_last_tribute_and_clears_on_close_group() {
    use outbe_primitives::series_id::SeriesId;
    let _enclave = outbe_nod::test_support::scope();
    let mut world = World::new();
    let sealed_at = outbe_primitives::time::date_key_to_utc_timestamp(20_261_020).unwrap() + 3_600;
    world.enter(|storage| {
        let series_id = SeriesId::for_pair(20_261_019, 840, 840).unwrap();
        let sealed_load =
            outbe_nod::api::seal_series_load(&storage, series_id, U256::from(15)).unwrap();
        crate::api::seal_lysis_series(
            &storage,
            crate::api::LysisSeriesInput {
                series_id,
                sealed_at,
                issuance_currency: 840,
                reference_currency: 840,
                entry_price_minor: U256::from(500_000),
                unclaimed_tribute_count: 1,
                unclaimed_tribute_load: sealed_load,
            },
        )
        .unwrap();
        let group = nod_api::series_group(&storage, series_id).unwrap().unwrap();
        let mut params = params(Address::repeat_byte(0x71));
        params.emission_day = 20_261_019;
        params.entry_price_minor = U256::from(500_000);
        params.gratis_load_minor = U256::from(15);
        let encrypted = outbe_nod::test_support::encrypted_fixture(&params, CHAIN_ID);
        let nod_id =
            crate::api::issue_tribute_nod_in_series(&storage, &encrypted, series_id).unwrap();
        assert_eq!(
            nod_api::series_group(&storage, series_id).unwrap(),
            Some(group)
        );

        let item = nod_api::load_item(&storage, nod_id).unwrap().unwrap();
        let loaded_group = nod_api::load_group(&storage, group).unwrap().unwrap();
        nod_api::remove_nod(&storage, item, loaded_group).unwrap();
        assert_eq!(nod_api::series_group(&storage, series_id).unwrap(), None);
    });
}
