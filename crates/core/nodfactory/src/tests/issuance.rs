use super::*;

#[test]
fn nod_pow_binds_owner_and_zero_sequence() {
    let owner = Address::repeat_byte(0x11);
    let other = Address::repeat_byte(0x22);
    let nod_id = NodContract::generate_nod_id(owner, WorldwideDay::new(20_241_201)).unwrap();
    let nonce = 42;
    let bound = runtime::compute_pow_hash(nod_id, owner, nonce);
    assert_ne!(bound, runtime::compute_pow_hash(nod_id, other, nonce));
    assert_ne!(
        bound,
        outbe_common::pow::compute_mining_pow_hash(
            outbe_common::pow::MiningDomain::Nod,
            nod_id.to_u256(),
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
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .unwrap();
    assert_eq!(item.owner, input.owner);
    assert_eq!(
        world
            .enter(|storage, scope, parent| {
                nod_api::list_by_owner(&storage, scope, parent, input.owner)
            })
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
            (NOD_ADDRESS, INod::NodBodyStored::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBucketBodyStored::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::Transfer::SIGNATURE_HASH),
            (NOD_FACTORY_ADDRESS, INodFactory::NodIssued::SIGNATURE_HASH),
        ]
    );
}

#[test]
fn second_same_block_issue_reuses_the_pending_bucket_without_parent_projection() {
    let mut world = World::new();
    let first = params(Address::repeat_byte(0x18));
    let second = params(Address::repeat_byte(0x19));
    let first_id = world.issue(&first);
    let second_id = world.issue(&second);
    assert_ne!(first_id, second_id);

    let bucket_key = NodContract::bucket_key(
        first.worldwide_day,
        first.entry_price_minor,
        first.reference_currency,
    );
    let bucket_id = WwdEntityId::from_day_and_digest(first.worldwide_day, bucket_key.0);
    let bucket = world
        .enter(|storage, scope, parent| nod_api::get_bucket(&storage, scope, parent, bucket_id))
        .unwrap()
        .unwrap();
    assert_eq!(bucket.entry_price_minor, first.entry_price_minor);
    assert_eq!(
        world
            .enter(|storage, _, _| NodContract::new(storage).bucket_nod_count.read(&bucket_key))
            .unwrap(),
        2
    );
    assert_eq!(
        world
            .provider
            .get_ordered_events()
            .iter()
            .filter(|event| {
                event.address == NOD_ADDRESS
                    && event.data.topics().first()
                        == Some(&INod::NodBucketBodyStored::SIGNATURE_HASH)
            })
            .count(),
        1,
        "only the first member creates the bucket body"
    );
    assert_eq!(
        world
            .enter(|storage, scope, parent| nod_api::list_all(&storage, scope, parent))
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
        .enter(|storage, scope, parent| api::issue_nod(&storage, scope, parent, &invalid))
        .unwrap_err();
    assert!(matches!(
        error,
        PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::InvalidOwner.to_string()
    ));

    invalid.owner = Address::repeat_byte(0x22);
    let nod_id = world.issue(&invalid);
    assert!(world
        .enter(|storage, scope, parent| api::issue_nod(&storage, scope, parent, &invalid))
        .is_err());
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
}

#[test]
fn direct_issuance_beyond_the_issuable_entry_writes_nothing() {
    let mut world = World::new();
    let mut input = params(Address::repeat_byte(0x2E));
    input.entry_price_minor = U256::MAX / U256::from(100 + u32::from(u16::MAX)) + U256::from(1);
    assert!(!NodContract::is_issuable_entry(input.entry_price_minor));
    let storage_before = world.provider.storage.clone();
    let events_before = world.provider.get_ordered_events().len();

    let error = world
        .enter(|storage, scope, parent| api::issue_nod(&storage, scope, parent, &input))
        .unwrap_err();

    assert!(matches!(
        error,
        PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::EntryPriceOutOfBounds.to_string()
    ));
    assert_eq!(world.provider.storage, storage_before);
    assert_eq!(world.provider.get_ordered_events().len(), events_before);
    let nod_id = NodContract::generate_nod_id(input.owner, input.worldwide_day).unwrap();
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_none());
}

#[test]
fn failed_authorization_preserves_the_loaded_nod() {
    let mut world = World::new();
    let input = free(Address::repeat_byte(0x33));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.settle(nod_id, input.owner).unwrap();
    let nonce = world.pow_nonce(nod_id);
    world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
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
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
}

#[test]
fn invalid_gratis_mac_rolls_back_the_nod_burn() {
    let mut world = World::new();
    let input = free(Address::repeat_byte(0x45));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.settle(nod_id, input.owner).unwrap();
    let nonce = world.pow_nonce(nod_id);

    world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
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
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
}

#[test]
fn qualified_mine_deletes_item_and_last_bucket_then_emits_burn() {
    let mut world = World::new();
    let input = free(Address::repeat_byte(0x55));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
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
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_none());
    let bucket_key = NodContract::bucket_key(
        input.worldwide_day,
        input.entry_price_minor,
        input.reference_currency,
    );
    let bucket_id = WwdEntityId::from_day_and_digest(input.worldwide_day, bucket_key.0);
    assert!(world
        .enter(|storage, scope, parent| { nod_api::get_bucket(&storage, scope, parent, bucket_id) })
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
            (NOD_ADDRESS, INod::NodBodyStored::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBucketBodyStored::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::MetadataUpdate::SIGNATURE_HASH),
            (NOD_FACTORY_ADDRESS, INodFactory::NodPaid::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBodyDeleted::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBucketBodyDeleted::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::Transfer::SIGNATURE_HASH),
            (
                NOD_FACTORY_ADDRESS,
                INodFactory::NodExercised::SIGNATURE_HASH
            ),
            (NOD_FACTORY_ADDRESS, INodFactory::NodBurned::SIGNATURE_HASH),
        ]
    );
}

/// Mining stays available to a Nod that qualified after issuance, with a
/// payment step after qualification.
#[test]
fn a_nod_qualifying_after_issuance_still_mines() {
    let mut world = World::new();
    let input = free(Address::repeat_byte(0x5a));
    let nod_id = world.issue(&input);

    world.qualify(nod_id);

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
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_none());
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
    world.qualify(nod_id);
    world.settle(nod_id, input.owner).unwrap();
    let nonce = world.pow_nonce(nod_id);
    let stranger = Address::repeat_byte(0x6b);

    let minted = world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller: stranger,
                    nod_id,
                    nonce,
                    auth: mine_auth(input.owner, input.gratis_load_minor),
                },
            )
        })
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_none());
    let exercised = world
        .provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| INodFactory::NodExercised::decode_log_data(&event.data).ok())
        .last()
        .expect("NodExercised event");
    assert_eq!(exercised.owner, input.owner);
    assert_eq!(exercised.nodId, nod_id.to_u256());
}

#[test]
fn certified_generation_has_no_public_installation_selector() {
    let mut world = World::new();
    let selector_hash = alloy_primitives::keccak256("installCertifiedGeneration(bytes)".as_bytes());
    let calldata = selector_hash[..4].to_vec();
    let storage_before = world.provider.storage.clone();
    let events_before = world.provider.get_ordered_events().to_vec();

    let result = world.enter(|storage, scope, parent| {
        crate::precompile::dispatch(
            storage,
            ExecutionReaders { scope, parent },
            &calldata,
            Address::repeat_byte(0x91),
            U256::ZERO,
        )
    });

    assert!(result.is_err());
    assert_eq!(world.provider.storage, storage_before);
    assert_eq!(world.provider.get_ordered_events(), events_before);
}
