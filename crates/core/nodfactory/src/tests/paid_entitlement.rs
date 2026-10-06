use super::*;

/// Settlement at the deadline remains valid; the paid Nod can then be mined.
#[test]
fn a_called_nod_still_mines_at_the_settlement_deadline() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x55));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);

    let called_at = 1_700_000_000;
    world.mark_called(nod_id, called_at);
    world.set_timestamp(called_at + u64::from(CALL_NOTICE_PERIOD));
    assert_eq!(public_nod_data(&mut world, nod_id).effectiveState, 2);

    let proof = world.covering_proof(nod_id, &input);
    world.settle(nod_id, input.owner, &proof).unwrap();
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
}

/// A called Nod settles inside its notice period whether or not its bucket has
/// qualified: the call alone opens settlement.
#[test]
fn a_called_nod_settles_without_qualifying() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x56));
    let nod_id = world.issue(&input);
    let proof = world.covering_proof(nod_id, &input);
    assert_eq!(
        world
            .settle(nod_id, input.owner, &proof)
            .unwrap_err()
            .to_string(),
        PrecompileError::from(NodFactoryError::NodNotQualified).to_string()
    );

    let called_at = 1_700_000_000;
    world.mark_called(nod_id, called_at);
    world.set_timestamp(called_at + 1);
    assert!(!public_nod_data(&mut world, nod_id).isQualified);
    world.settle(nod_id, input.owner, &proof).unwrap();
    assert!(public_nod_data(&mut world, nod_id).isSettled);
}

/// Past the deadline the Nod is forfeit. The daily sweep burns it, but this gate
/// closes the window between the deadline and the sweep reaching it.
#[test]
fn settlement_is_rejected_once_the_deadline_has_passed() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x55));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);

    let called_at = 1_700_000_000;
    world.mark_called(nod_id, called_at);
    world.set_timestamp(called_at + u64::from(CALL_NOTICE_PERIOD) + 1);

    let data = public_nod_data(&mut world, nod_id);
    assert_eq!(data.effectiveState, 4);
    assert!(data.isQualified);
    assert!(!data.isSettled);
    assert_eq!(
        data.settlementDeadline,
        called_at + u64::from(CALL_NOTICE_PERIOD)
    );

    let error = world.settle(nod_id, input.owner, &[]).unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::CallDeadlineExpired.to_string()),
        "unexpected error: {error:?}"
    );
    // The Nod survives for the sweep to burn; the gate only refuses to mine it.
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
}

#[test]
fn settlement_preserves_entitlement_and_failed_mining_can_retry_after_deadline() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x81));
    let nod_id = world.issue(&input);
    let proof = world.covering_proof(nod_id, &input);
    assert!(
        world.settle(nod_id, input.owner, &proof).is_err(),
        "unqualified"
    );
    world.qualify(nod_id);
    let called_at = 1_700_000_000;
    world.mark_called(nod_id, called_at);
    world.set_timestamp(called_at + u64::from(CALL_NOTICE_PERIOD));
    world.settle(nod_id, input.owner, &proof).unwrap();
    let stored = world.enter(|storage, scope, parent| {
        let item = nod_api::get_item(&storage, scope, parent, nod_id)
            .unwrap()
            .unwrap();
        assert!(item.is_settled);
        assert_eq!(item.owner, input.owner);
        let bucket = nod_api::get_bucket(
            &storage,
            scope,
            parent,
            WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key),
        )
        .unwrap()
        .unwrap();
        assert_eq!(bucket.settled_nods, 1);
        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.total_supply().unwrap(), 1);
        assert_eq!(nod.bucket_nod_count.read(&item.bucket_key).unwrap(), 0);
        assert_eq!(
            nod.bucket_called_at.read(&item.bucket_key).unwrap(),
            called_at
        );
        outbe_nod::canonical_item(&item)
    });
    let before = world.provider.storage.clone();
    let events = world.provider.get_ordered_events().to_vec();
    assert!(
        world.settle(nod_id, input.owner, &proof).is_err(),
        "duplicate settlement"
    );
    world.set_timestamp(called_at + u64::from(CALL_NOTICE_PERIOD) + 365 * 86_400);
    assert_eq!(public_nod_data(&mut world, nod_id).effectiveState, 3);
    let nonce = world.pow_nonce(nod_id);
    for (caller, candidate, auth) in [
        (Address::repeat_byte(0x82), nonce, dummy_auth()),
        (
            input.owner,
            (0..100_000)
                .find(|n| runtime::validate_pow(nod_id, input.owner, *n).is_err())
                .unwrap(),
            dummy_auth(),
        ),
        (input.owner, nonce, dummy_auth()),
    ] {
        assert!(world
            .enter(|storage, scope, parent| api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller,
                    nod_id,
                    nonce: candidate,
                    auth
                }
            ))
            .is_err());
        assert_eq!(world.provider.storage, before);
        assert_eq!(world.provider.get_ordered_events(), events);
        world.enter(|storage, scope, parent| {
            let item = nod_api::get_item(&storage, scope, parent, nod_id)
                .unwrap()
                .unwrap();
            assert_eq!(outbe_nod::canonical_item(&item), stored);
        });
    }
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
        .enter(|storage, scope, parent| api::mine_gratis(
            &storage,
            scope,
            parent,
            api::MineGratisRequest {
                caller: input.owner,
                nod_id,
                nonce,
                auth: dummy_auth()
            }
        ))
        .is_err());
}

#[test]
fn settlement_failure_rolls_back_payment_change_and_body_updates() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x83));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let (proof, nullifier) = world.fund_note(NOTE_ASSET, nod_id, cost * 2, cost);
    // Force a state failure after consume has booked the nullifier and appended change.
    world.enter(|storage, scope, parent| {
        let item = nod_api::get_item(&storage, scope, parent, nod_id)
            .unwrap()
            .unwrap();
        NodContract::new(storage)
            .bucket_nod_count
            .write(&item.bucket_key, 0)
            .unwrap();
    });
    let before = world.provider.storage.clone();
    let events = world.provider.get_ordered_events().to_vec();
    assert!(world.settle(nod_id, input.owner, &proof).is_err());
    assert_eq!(world.provider.storage, before);
    assert_eq!(world.provider.get_ordered_events(), events);
    world.enter(|storage, scope, parent| {
        assert!(!outbe_paynote::api::is_spent(&storage, nullifier).unwrap());
        let item = nod_api::get_item(&storage, scope, parent, nod_id)
            .unwrap()
            .unwrap();
        assert!(!item.is_settled);
        NodContract::new(storage)
            .bucket_nod_count
            .write(&item.bucket_key, 1)
            .unwrap();
    });
    world.settle(nod_id, input.owner, &proof).unwrap();
}

#[test]
fn unpaid_mining_is_rejected() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x84));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    let error = world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller: input.owner,
                    nod_id,
                    nonce: 0,
                    auth: dummy_auth(),
                },
            )
        })
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(reason) if reason == NodFactoryError::NodNotSettled.to_string())
    );
    assert_eq!(
        crate::precompile::base_gas(&INodFactory::mineGratisCall::SELECTOR),
        outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS
    );
}

#[test]
fn settlement_leaves_fidelity_alone_and_mining_records_it() {
    let fidelity = |world: &World| {
        world
            .provider
            .storage
            .iter()
            .filter(|((address, _), _)| *address == outbe_primitives::addresses::FIDELITY_ADDRESS)
            .map(|(slot, value)| (*slot, *value))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x86));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    let proof = world.covering_proof(nod_id, &input);
    let before = fidelity(&world);

    world.settle(nod_id, input.owner, &proof).unwrap();
    assert_eq!(fidelity(&world), before, "settlement acquires no Fidelity");

    let nonce = world.pow_nonce(nod_id);
    world
        .mine_gratis(api::MineGratisRequest {
            caller: input.owner,
            nod_id,
            nonce,
            auth: mine_auth(input.owner, input.gratis_load_minor),
        })
        .unwrap();
    assert_ne!(fidelity(&world), before, "the mint records the acquisition");
}

#[test]
fn fidelity_persistence_failure_preserves_paid_entitlement_and_mint_nonce() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x85));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    let proof = world.covering_proof(nod_id, &input);
    world.settle(nod_id, input.owner, &proof).unwrap();
    let before = world.provider.storage.clone();
    let events = world.provider.get_ordered_events().to_vec();
    let nonce = world.pow_nonce(nod_id);
    world
        .provider
        .fail_mutation_at_address(outbe_primitives::addresses::FIDELITY_ADDRESS);
    let result = world.mine_gratis(api::MineGratisRequest {
        caller: input.owner,
        nod_id,
        nonce,
        auth: mine_auth(input.owner, input.gratis_load_minor),
    });
    assert!(result.is_err());
    world.provider.clear_mutation_failure();
    assert_eq!(world.provider.storage, before);
    assert_eq!(world.provider.get_ordered_events(), events);
    world.enter(|storage, scope, parent| {
        assert!(
            nod_api::get_item(&storage, scope, parent, nod_id)
                .unwrap()
                .unwrap()
                .is_settled
        );
        api::mine_gratis(
            &storage,
            scope,
            parent,
            api::MineGratisRequest {
                caller: input.owner,
                nod_id,
                nonce,
                auth: mine_auth(input.owner, input.gratis_load_minor),
            },
        )
        .unwrap();
    });
}

#[test]
fn erc20_settlement_enforces_eligibility_before_payment_and_accepts_zero_cost() {
    let mut world = World::new();
    let mut input = params(Address::repeat_byte(0x91));
    input.entry_price_minor = U256::ZERO;
    let nod_id = world.issue(&input);
    world.register_reference_currency_asset(NOTE_ASSET);
    let settle = |world: &mut World, caller, asset| {
        world.enter(|storage, scope, parent| {
            api::settle_nod(&storage, scope, parent, caller, nod_id, asset, U256::ZERO)
        })
    };
    let stranger = Address::repeat_byte(0x92);
    assert_eq!(
        settle(&mut world, stranger, NOTE_ASSET)
            .unwrap_err()
            .to_string(),
        PrecompileError::from(NodFactoryError::NodNotQualified).to_string()
    );
    world.qualify(nod_id);
    let foreign = Address::repeat_byte(0x99);
    world.register_settlement_asset(foreign, 978);
    let error = settle(&mut world, stranger, foreign).unwrap_err();
    assert_eq!(
        error.to_string(),
        PrecompileError::from(NodFactoryError::SettlementCurrencyMismatch { iso_code: 978 })
            .to_string()
    );
    assert!(
        !world
            .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
            .unwrap()
            .unwrap()
            .is_settled
    );

    // No token transfer stubs: zero cost must require neither funds nor approvals.
    settle(&mut world, stranger, NOTE_ASSET).unwrap();
    assert_eq!(
        settle(&mut world, input.owner, NOTE_ASSET)
            .unwrap_err()
            .to_string(),
        PrecompileError::from(NodFactoryError::NodAlreadySettled).to_string()
    );
    let paid = world
        .provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| INodFactory::NodPaid::decode_log_data(&event.data).ok())
        .last()
        .unwrap();
    assert_eq!(paid.owner, input.owner);
    assert_eq!(paid.nullifier, B256::ZERO);
    assert_eq!(paid.paymentMinor, U256::ZERO);
}

#[test]
fn settlement_over_a_broken_member_index_reverts() {
    let mut world = World::new();
    let mut input = params(Address::repeat_byte(0x93));
    input.entry_price_minor = U256::ZERO;
    let nod_id = world.issue(&input);
    world.register_reference_currency_asset(NOTE_ASSET);
    world.qualify(nod_id);
    world.enter(|storage, _, _| {
        NodContract::new(storage)
            .bucket_nod_index
            .write(&nod_id, 7)
            .unwrap();
    });
    let error = world
        .enter(|storage, scope, parent| {
            api::settle_nod(
                &storage,
                scope,
                parent,
                input.owner,
                nod_id,
                NOTE_ASSET,
                U256::ZERO,
            )
        })
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(message) if message.contains("is not indexed"))
    );
}

#[test]
fn erc20_selector_has_no_zk_surcharge_and_old_paynote_selector_is_rejected() {
    assert_eq!(
        crate::precompile::base_gas(&INodFactory::settleNodCall::SELECTOR),
        outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS
    );
    alloy_sol_types::sol! { function settleNod(uint256 nodId, bytes payNoteProof) external; }
    let data = settleNodCall {
        nodId: U256::ONE,
        payNoteProof: Bytes::new(),
    }
    .abi_encode();
    let mut world = World::new();
    assert!(world
        .enter(|storage, scope, parent| crate::precompile::dispatch(
            storage,
            scope,
            parent,
            &data,
            Address::repeat_byte(1),
            U256::ZERO,
        ))
        .is_err());
}

#[test]
fn erc20_settlement_uses_the_existing_inclusive_deadline() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x93));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let called_at = 1_700_000_000;
    world.mark_called(nod_id, called_at);
    let deadline = called_at + u64::from(CALL_NOTICE_PERIOD);
    let foreign = Address::repeat_byte(0x94);
    world.register_settlement_asset(foreign, 978);
    for (timestamp, expected) in [
        (deadline + 1, NodFactoryError::CallDeadlineExpired),
        (
            deadline,
            NodFactoryError::SettlementCurrencyMismatch { iso_code: 978 },
        ),
    ] {
        world.set_timestamp(timestamp);
        let error = world
            .enter(|storage, scope, parent| {
                api::settle_nod(
                    &storage,
                    scope,
                    parent,
                    input.owner,
                    nod_id,
                    foreign,
                    U256::ZERO,
                )
            })
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            PrecompileError::from(expected).to_string()
        );
    }
}
