use super::*;

#[test]
fn nod_pow_binds_owner_and_zero_sequence() {
    let owner = Address::repeat_byte(0x11);
    let other = Address::repeat_byte(0x22);
    let nod_id = outbe_nod::test_support::nod_id_of(owner, WorldwideDay::new(20_241_201), 840, 840);
    let nonce = 42;
    let bound = runtime::compute_pow_hash(nod_id, owner, nonce);
    assert_ne!(bound, runtime::compute_pow_hash(nod_id, other, nonce));
    assert_ne!(
        bound,
        outbe_common::pow::compute_mining_pow_hash(
            outbe_common::pow::MiningDomain::Nod,
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
    assert_eq!(
        world
            .enter(|storage| nod_api::list_all(&storage))
            .unwrap()
            .len(),
        2
    );
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
        input.worldwide_day,
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
            (NOD_FACTORY_ADDRESS, INodFactory::NodMined::SIGNATURE_HASH),
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
        .filter_map(|event| INodFactory::NodMined::decode_log_data(&event.data).ok())
        .last()
        .expect("NodMined event");
    assert_eq!(mined.owner, input.owner);
    assert_eq!(mined.nodId, nod_id);
}

#[test]
fn lysis_series_of_one_day_and_reference_share_one_call_group() {
    use outbe_primitives::series_id::SeriesId;
    let mut world = World::new();
    let sealed_at = outbe_primitives::time::date_key_to_utc_timestamp(20_261_020) + 3_600;
    world.enter(|storage| {
        let seal = |series_id, count, load: u64| {
            crate::api::seal_lysis_series(
                &storage,
                crate::api::LysisSeriesInput {
                    series_id,
                    sealed_at,
                    reference_currency: 840,
                    entry_price_minor: U256::from(500_000),
                    unclaimed_tribute_count: count,
                    unclaimed_tribute_load: U256::from(load),
                },
            )
        };
        let usd = SeriesId::for_pair(20_261_019, 840, 840).unwrap();
        let eur = SeriesId::for_pair(20_261_019, 978, 840).unwrap();
        seal(usd, 2, 30).unwrap();
        seal(eur, 1, 10).unwrap();

        let group = nod_api::series_group(&storage, usd).unwrap().unwrap();
        assert_eq!(nod_api::series_group(&storage, eur).unwrap(), Some(group));
        let nod = outbe_nod::NodContract::new(storage.clone());
        assert_eq!(nod.group_unclaimed_tribute_count.read(&group).unwrap(), 3);
        assert_eq!(
            nod.group_unclaimed_tribute_load.read(&group).unwrap(),
            U256::from(40)
        );
        assert_eq!(
            nod_api::get_group(&storage, group)
                .unwrap()
                .unwrap()
                .full_day_after_issue,
            WorldwideDay::new(20_261_021),
            "the call counts from the first full day after Lysis"
        );
        assert!(seal(usd, 1, 5).is_err(), "a series seals once");
    });
}
