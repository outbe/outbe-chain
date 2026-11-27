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
            (NOD_ADDRESS, INod::NodIssued::SIGNATURE_HASH),
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
    // Dummy MAC is rejected regardless of who submits; the settled Nod remains.
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
    // After the offering of its emission day no Nod can join the group, so it closes.
    world.set_timestamp(
        outbe_system_common::time::date_key_to_utc_timestamp(input.emission_day).unwrap()
            + 3 * 86_400,
    );
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
            (NOD_ADDRESS, INod::NodSettled::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBurned::SIGNATURE_HASH),
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

const GROUP_DAY: u32 = 20_261_019;

/// The offer hour of a [`GROUP_DAY`] Nod: 10:00 UTC of the day before.
fn offered_at() -> u64 {
    outbe_system_common::time::date_key_to_utc_timestamp(20_261_018).unwrap() + 10 * 3_600
}

/// Issues at the offer the Nod of `owner` in sub-series 0 of the `issuance_currency`
/// series of [`GROUP_DAY`], reference 840 and entry 0.5.
fn issue_offered(
    storage: &StorageHandle<'_>,
    owner: Address,
    issuance_currency: u16,
    load: u64,
) -> U256 {
    let mut input = params(owner);
    input.emission_day = GROUP_DAY;
    input.issuance_currency = issuance_currency;
    input.gratis_load_minor = U256::from(load);
    let encrypted = outbe_nod::test_support::encrypted_fixture(&input, CHAIN_ID);
    runtime::issue_offer_nod(storage, &encrypted, offered_at(), 0).unwrap()
}

fn series(issuance_currency: u16) -> outbe_core_common::series_id::SeriesId {
    outbe_core_common::series_id::SeriesId::for_pair(GROUP_DAY, issuance_currency, 840).unwrap()
}

fn group_of(storage: &StorageHandle<'_>, nod_id: U256) -> B256 {
    nod_api::get_item(storage, nod_id)
        .unwrap()
        .unwrap()
        .group_key
}

/// Calls `group` a day after its emission day and returns the settlement deadline.
fn call(storage: &StorageHandle<'_>, group: B256) -> (u64, u64) {
    let mut nod = outbe_nod::NodContract::new(storage.clone());
    let terms = nod.read_call_terms(group).unwrap();
    let called_at =
        outbe_system_common::time::date_key_to_utc_timestamp(GROUP_DAY).unwrap() + 86_400;
    crate::call::call_series(&mut nod, group, called_at, terms.call_notice_period_seconds).unwrap();
    (
        called_at,
        nod_api::settlement_deadline_of(called_at, terms.call_notice_period_seconds),
    )
}

fn block_at<'s>(
    storage: &StorageHandle<'s>,
    block_number: u64,
    timestamp: u64,
) -> outbe_primitives::block::BlockRuntimeContext<'s> {
    outbe_primitives::block::BlockRuntimeContext::new(
        outbe_primitives::block::BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number,
            timestamp,
            chain_id: CHAIN_ID,
            genesis_hash: B256::ZERO,
            genesis_timestamp: offered_at() - 86_400,
            proposer: Address::ZERO,
            validators: Vec::new(),
        },
        storage.clone(),
    )
}

fn close_at(
    storage: &StorageHandle<'_>,
    block_number: u64,
    timestamp: u64,
    writes: u32,
) -> outbe_core_common::deadline_queue::Progress {
    let ctx = block_at(storage, block_number, timestamp);
    let through_day = outbe_core_common::deadline_queue::deadline_day(timestamp).unwrap();
    let mut budget = outbe_system_common::pass_budget::PassBudget::new(100, writes);
    crate::close::close_through(&ctx, through_day, &mut budget).unwrap()
}

fn promis_limit(storage: &StorageHandle<'_>) -> U256 {
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .promis_limit()
        .unwrap()
}

fn close_queue_is_empty(storage: &StorageHandle<'_>) -> bool {
    let nod = outbe_nod::NodContract::new(storage.clone());
    outbe_core_common::deadline_queue::first_day(&outbe_nod::state::CloseQueue(&nod))
        .unwrap()
        .is_none()
}

fn closed_series(world: &World) -> Vec<(u16, U256)> {
    world
        .provider
        .get_events(NOD_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| INodFactory::NodSeriesClosed::decode_log_data(log).ok())
        .map(|event| (event.issuanceCurrency, event.forfeitedAmountMinor))
        .collect()
}

/// A called group of a USD Nod with load 30 and a EUR Nod with load 10, both unsettled.
fn lapsed_two_series_group(storage: &StorageHandle<'_>) -> (B256, u64) {
    let usd = issue_offered(storage, Address::repeat_byte(0x71), 840, 30);
    let eur = issue_offered(storage, Address::repeat_byte(0x72), 978, 10);
    let group = group_of(storage, usd);
    assert_eq!(group_of(storage, eur), group);
    (group, call(storage, group).1)
}

#[test]
fn sub_series_of_one_day_and_reference_share_one_call_group() {
    let _enclave = outbe_nod::test_support::scope();
    let mut world = World::new();
    let (nods, called_at, deadline) = world.enter(|storage| {
        let settled = issue_offered(&storage, Address::repeat_byte(0x71), 840, 15);
        let usd = issue_offered(&storage, Address::repeat_byte(0x72), 840, 15);
        let eur = issue_offered(&storage, Address::repeat_byte(0x73), 978, 10);
        let group = group_of(&storage, settled);
        for nod_id in [usd, eur] {
            assert_eq!(group_of(&storage, nod_id), group);
        }
        for currency in [840, 978] {
            assert_eq!(
                nod_api::sub_series_group(&storage, series(currency), 0).unwrap(),
                Some(group)
            );
        }
        let nod = outbe_nod::NodContract::new(storage.clone());
        assert_eq!(nod.read_group_open_load(group).unwrap(), U256::from(40));
        assert_eq!(
            nod_api::get_group(&storage, group)
                .unwrap()
                .unwrap()
                .full_day_after_issue,
            outbe_system_common::time::WorldwideDay::new(GROUP_DAY),
            "the call counts from 00:00 of the emission day"
        );

        let loaded_item = nod_api::load_item(&storage, settled).unwrap().unwrap();
        let loaded_group = nod_api::load_group(&storage, group).unwrap().unwrap();
        nod_api::record_settled(&storage, loaded_item, loaded_group).unwrap();

        let (called_at, deadline) = call(&storage, group);
        close_at(&storage, 10, deadline + 86_400, 100);
        assert_eq!(promis_limit(&storage), U256::from(25));
        crate::forfeit::forfeit_through(&block_at(&storage, 11, deadline + 86_400)).unwrap();
        assert!(nod_api::get_item(&storage, settled).unwrap().is_some());
        for nod_id in [usd, eur] {
            assert!(nod_api::get_item(&storage, nod_id).unwrap().is_none());
        }
        ([settled, usd, eur], called_at, deadline)
    });

    let opened: Vec<_> = world
        .provider
        .get_events(NOD_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| INodFactory::NodSeriesOpened::decode_log_data(log).ok())
        .map(|event| {
            (
                event.subSeriesId,
                event.issuanceCurrency,
                event.referenceCurrency,
                event.entryPriceMinor,
            )
        })
        .collect();
    assert_eq!(
        opened,
        [
            (0, 840, 840, U256::from(500_000)),
            (0, 978, 840, U256::from(500_000))
        ],
        "a sub-series opens once, with its first Nod"
    );

    let issued: Vec<_> = world
        .provider
        .get_events(NOD_ADDRESS)
        .iter()
        .filter_map(|log| INod::NodIssued::decode_log_data(log).ok())
        .collect();
    assert_eq!(issued.len(), 3);
    assert_eq!(
        issued.iter().map(|event| event.nodId).collect::<Vec<_>>(),
        nods
    );
    assert_eq!(
        (
            issued[2].subSeriesId,
            issued[2].issuanceCurrency,
            issued[2].referenceCurrency
        ),
        (0, 978, 840)
    );
    let opened_load = world.enter(|storage| {
        let item = nod_api::get_item(&storage, nods[0]).unwrap().unwrap();
        assert_eq!(
            issued[0].payload.as_ref(),
            item.encrypted.encrypted_gratis_amount.as_slice()
        );
        let view_key = outbe_nod::nod_encryption::derive_view_key(
            &outbe_nod::test_support::state_key(),
            item.owner,
        )
        .expect("owner view key");
        outbe_nod::test_support::decrypt_for_owner(&view_key, &item.encrypted)
            .expect("open nod load")
    });
    assert_eq!(opened_load, U256::from(15));

    let called_series: Vec<_> = world
        .provider
        .get_events(NOD_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| INodFactory::NodSeriesCalled::decode_log_data(log).ok())
        .map(|event| {
            (
                event.issuanceCurrency,
                event.referenceCurrency,
                event.calledAt,
                event.settlementDeadline,
            )
        })
        .collect();
    assert_eq!(
        called_series,
        [
            (840, 840, called_at, deadline),
            (978, 840, called_at, deadline)
        ]
    );

    let closed: Vec<_> = world
        .provider
        .get_events(NOD_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| INodFactory::NodSeriesClosed::decode_log_data(log).ok())
        .map(|event| {
            (
                event.issuanceCurrency,
                event.referenceCurrency,
                event.forfeitedCount,
                event.forfeitedAmountMinor,
            )
        })
        .collect();
    assert_eq!(
        closed,
        [(840, 840, 1, U256::from(15)), (978, 840, 1, U256::from(10))]
    );

    let forfeited: Vec<_> = world
        .provider
        .get_events(NOD_ADDRESS)
        .iter()
        .filter_map(|log| INod::NodForfeited::decode_log_data(log).ok())
        .map(|event| (event.nodId, event.issuanceCurrency, event.referenceCurrency))
        .collect();
    assert_eq!(forfeited, [(nods[1], 840, 840), (nods[2], 978, 840)]);
    assert!(world
        .provider
        .get_events(NOD_ADDRESS)
        .iter()
        .all(|log| INod::NodBurned::decode_log_data(log).is_err()));
}

/// A group wider than the block's write budget forfeits sub-series by sub-series over
/// several blocks, with one Promis Limit credit when its last sub-series closes.
#[test]
fn a_group_forfeits_one_series_per_write_and_credits_once() {
    let _enclave = outbe_nod::test_support::scope();
    let mut world = World::new();
    world.enter(|storage| {
        let (group, deadline) = lapsed_two_series_group(&storage);
        let at = deadline + 86_400;

        close_at(&storage, 10, at, 1);
        assert_eq!(promis_limit(&storage), U256::ZERO);
        assert!(!nod_api::is_group_closed(&storage, group).unwrap());
        assert!(nod_api::is_group_close_started(&storage, group).unwrap());

        let progress = close_at(&storage, 11, at, 1);
        assert!(matches!(
            progress,
            outbe_core_common::deadline_queue::Progress::Complete
        ));
        assert_eq!(promis_limit(&storage), U256::from(40));
        assert!(nod_api::is_group_closed(&storage, group).unwrap());
        assert!(close_queue_is_empty(&storage));
    });
    assert_eq!(
        closed_series(&world),
        [(840, U256::from(30)), (978, U256::from(10))]
    );
}

/// A sub-series that fails to close keeps the group open and in the queue while the healthy
/// one commits its close once; the retry closes only the open one and credits the group once.
#[test]
fn a_series_left_open_defers_the_group_and_the_retry_credits_once() {
    let _enclave = outbe_nod::test_support::scope();
    for (corrupt_usd, expected_after_first, expected_final) in [
        (
            true,
            vec![(978, U256::from(10))],
            vec![(978, U256::from(10)), (840, U256::from(30))],
        ),
        (
            false,
            vec![(840, U256::from(30))],
            vec![(840, U256::from(30)), (978, U256::from(10))],
        ),
    ] {
        let mut world = World::new();
        let (group, target_key, saved_load, deadline) = world.enter(|storage| {
            let (group, deadline) = lapsed_two_series_group(&storage);
            let nod = outbe_nod::NodContract::new(storage.clone());
            let target = series(if corrupt_usd { 840 } else { 978 });
            let target_key = outbe_nod::index_keys::group_series_key(
                group,
                outbe_nod::index_keys::sub_series_key(target, 0),
            );
            let saved_load = nod
                .group_series_unsettled_load_minor_ct
                .bytes(&target_key)
                .read()
                .unwrap();
            nod.group_series_unsettled_load_minor_ct
                .bytes(&target_key)
                .write(&[0xFF; 3])
                .unwrap();

            close_at(&storage, 10, deadline + 86_400, 64);
            assert_eq!(promis_limit(&storage), U256::ZERO);
            assert!(!nod_api::is_group_closed(&storage, group).unwrap());
            assert!(nod_api::is_group_close_started(&storage, group).unwrap());
            assert!(!close_queue_is_empty(&storage));
            (group, target_key, saved_load, deadline)
        });
        assert_eq!(closed_series(&world), expected_after_first);

        world.enter(|storage| {
            let nod = outbe_nod::NodContract::new(storage.clone());
            nod.group_series_unsettled_load_minor_ct
                .bytes(&target_key)
                .write(&saved_load)
                .unwrap();
            close_at(&storage, 11, deadline + 2 * 86_400, 64);
            assert_eq!(promis_limit(&storage), U256::from(40));
            assert!(nod_api::is_group_closed(&storage, group).unwrap());
            assert!(close_queue_is_empty(&storage));
        });
        assert_eq!(closed_series(&world), expected_final);
    }

    // Even when every sub-series fails on the first attempt, `group_closing` latches.
    let mut world = World::new();
    world.enter(|storage| {
        let (group, deadline) = lapsed_two_series_group(&storage);
        let nod = outbe_nod::NodContract::new(storage.clone());
        for currency in [840, 978] {
            let key = outbe_nod::index_keys::group_series_key(
                group,
                outbe_nod::index_keys::sub_series_key(series(currency), 0),
            );
            nod.group_series_unsettled_load_minor_ct
                .bytes(&key)
                .write(&[0xFF; 3])
                .unwrap();
        }
        close_at(&storage, 10, deadline + 86_400, 64);
        assert!(!nod_api::is_group_closed(&storage, group).unwrap());
        assert!(nod_api::is_group_close_started(&storage, group).unwrap());
        assert!(!close_queue_is_empty(&storage));
    });
}

/// A sub-series whose group empties inside the offering window keeps its terms, so the
/// next Nod of its hour joins it and the sub-series opens once.
#[test]
fn a_sub_series_emptied_inside_its_offering_window_opens_once() {
    let _enclave = outbe_nod::test_support::scope();
    let mut world = World::new();
    world.enter(|storage| {
        let first = issue_offered(&storage, Address::repeat_byte(0x71), 840, 15);
        let group = group_of(&storage, first);
        settle_item(&storage, first);
        let item = nod_api::load_item(&storage, first).unwrap().unwrap();
        let loaded_group = nod_api::load_group(&storage, group).unwrap().unwrap();
        nod_api::remove_nod(&storage, item, loaded_group).unwrap();
        assert!(nod_api::get_group(&storage, group).unwrap().is_some());
        assert_eq!(
            nod_api::sub_series_group(&storage, series(840), 0).unwrap(),
            Some(group)
        );
        assert_eq!(
            nod_api::sub_series_entry_price(&storage, series(840), 0).unwrap(),
            Some(U256::from(500_000))
        );
        let second = issue_offered(&storage, Address::repeat_byte(0x72), 840, 15);
        assert_eq!(group_of(&storage, second), group);
    });
    let opened = world
        .provider
        .get_events(NOD_FACTORY_ADDRESS)
        .iter()
        .filter(|log| INodFactory::NodSeriesOpened::decode_log_data(log).is_ok())
        .count();
    assert_eq!(opened, 1);
}

fn settle_item(storage: &StorageHandle<'_>, nod_id: U256) {
    let item = nod_api::load_item(storage, nod_id).unwrap().unwrap();
    let group = nod_api::load_group(storage, item.body().group_key)
        .unwrap()
        .unwrap();
    nod_api::record_settled(storage, item, group).unwrap();
}

/// The forfeit credits the Promis Limit with the exact sum of the forfeited loads, once. A
/// settled Nod that is not mined yet gets no credit and stays.
#[test]
fn forfeited_nod_loads_go_to_the_promis_limit_once() {
    let _enclave = outbe_nod::test_support::scope();
    let mut world = World::new();
    world.enter(|storage| {
        let forfeited: Vec<_> = [(0x71, 840, 7u64), (0x72, 840, 11), (0x73, 978, 13)]
            .into_iter()
            .map(|(owner, currency, load)| {
                issue_offered(&storage, Address::repeat_byte(owner), currency, load)
            })
            .collect();
        let settled = issue_offered(&storage, Address::repeat_byte(0x74), 840, 100);
        settle_item(&storage, settled);
        let group = group_of(&storage, settled);
        let (_, deadline) = call(&storage, group);
        let at = deadline + 86_400;
        close_at(&storage, 10, at, 100);
        assert_eq!(promis_limit(&storage), U256::from(31));
        crate::forfeit::forfeit_through(&block_at(&storage, 11, at)).unwrap();
        close_at(&storage, 12, at + 86_400, 100);
        crate::forfeit::forfeit_through(&block_at(&storage, 13, at + 86_400)).unwrap();
        assert_eq!(promis_limit(&storage), U256::from(31));
        for nod_id in forfeited {
            assert!(nod_api::get_item(&storage, nod_id).unwrap().is_none());
        }
        assert!(
            nod_api::get_item(&storage, settled)
                .unwrap()
                .unwrap()
                .is_settled
        );
    });
}

/// Two sub-series of one series at two entry prices sit in two groups. The first group
/// closes, and the call of the second still names the currencies of its sub-series.
#[test]
fn a_closed_group_leaves_the_other_groups_of_its_series_their_currencies() {
    let _enclave = outbe_nod::test_support::scope();
    let mut world = World::new();
    world.enter(|storage| {
        let early = issue_offered(&storage, Address::repeat_byte(0x71), 840, 15);
        let mut input = params(Address::repeat_byte(0x73));
        input.emission_day = GROUP_DAY;
        input.issuance_currency = 840;
        input.entry_price_minor = U256::from(700_000);
        let late = runtime::issue_offer_nod(
            &storage,
            &outbe_nod::test_support::encrypted_fixture(&input, CHAIN_ID),
            offered_at() + 3_600,
            1,
        )
        .unwrap();
        let first_group = group_of(&storage, early);
        let second_group = group_of(&storage, late);
        assert_ne!(first_group, second_group);

        settle_item(&storage, early);
        let item = nod_api::load_item(&storage, early).unwrap().unwrap();
        let loaded = nod_api::load_group(&storage, first_group).unwrap().unwrap();
        nod_api::remove_nod(&storage, item, loaded).unwrap();
        assert!(nod_api::close_empty_group(&storage, first_group).unwrap());
        call(&storage, second_group);
    });
    let called: Vec<_> = world
        .provider
        .get_events(NOD_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| INodFactory::NodSeriesCalled::decode_log_data(log).ok())
        .map(|event| {
            (
                event.subSeriesId,
                event.issuanceCurrency,
                event.referenceCurrency,
            )
        })
        .collect();
    assert_eq!(called, [(1, 840, 840)]);
}
