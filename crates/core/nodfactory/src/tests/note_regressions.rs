use super::*;

#[test]
fn a_note_in_a_wider_asset_pays_the_cost_scaled_to_its_decimals() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x61));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    world.set_asset_decimals(NOTE_ASSET, 18);
    let cost = U256::from(cost_of(&input)) * U256::from(1_000_000_000_000u64);
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, nod_id, cost, cost);
    let nonce = world.pow_nonce(nod_id);

    let minted = world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
}

#[test]
fn a_covering_paynote_mines_a_paid_nod_and_books_the_nullifier() {
    assert_covering_paynote_mines_nod(params(Address::repeat_byte(0x61)));
}

#[test]
fn a_one_minor_unit_paynote_mines_a_dust_cost_nod_without_reducing_gratis() {
    let mut input = params(Address::repeat_byte(0x61));
    input.entry_price_minor = U256::from(19);
    input.gratis_load_minor = U256::from(25_629);
    assert_eq!(cost_of(&input), 1);
    assert_covering_paynote_mines_nod(input);
}

/// The surplus of an over-covering spend has already reached the reserve vault
/// and nothing returns it, so the spend must equal the cost exactly.
#[test]
fn a_paynote_over_the_cost_leaves_the_nod_and_the_note_intact() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x66));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let (proof, nullifier) = world.fund_note(NOTE_ASSET, nod_id, cost + 1, cost + 1);
    let nonce = world.pow_nonce(nod_id);

    let error = world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::PayNoteCostMismatch {
                covered: U256::from(cost + 1),
                required: U256::from(cost),
            }
            .to_string()),
        "unexpected error: {error:?}"
    );
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
    let spent =
        world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap());
    assert!(!spent, "a rejected over-cover must not consume the note");
}

#[test]
fn a_paynote_short_of_the_cost_leaves_the_nod_and_the_note_intact() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x62));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, nod_id, cost, cost - 1);
    let nonce = world.pow_nonce(nod_id);

    let error = world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::PayNoteCostMismatch {
                covered: U256::from(cost - 1),
                required: U256::from(cost),
            }
            .to_string()),
        "unexpected error: {error:?}"
    );
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
}

/// `consume` books the nullifier before the cover check runs, so this is the
/// test that proves settlement is one rollback unit: rejected settlement must
/// leave the note spendable rather than destroying it for nothing.
#[test]
fn rejected_settlement_unbooks_the_nullifier_it_had_already_spent() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x63));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let (proof, nullifier) = world.fund_note(NOTE_ASSET, nod_id, cost, cost - 1);
    let nonce = world.pow_nonce(nod_id);

    world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();

    let spent =
        world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap());
    assert!(!spent, "reverted settlement must not consume the note");
}

#[test]
fn a_paynote_bound_to_another_nod_cannot_pay_this_one() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x64));
    let nod_id = world.issue(&input);
    let other = NodIssueParams {
        worldwide_day: WorldwideDay::new(20_241_221),
        ..params(input.owner)
    };
    let other_id = world.issue(&other);
    world.qualify(nod_id);
    world.qualify(other_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let (proof, nullifier) = world.fund_note(NOTE_ASSET, other_id, cost, cost);

    let error = world.settle(nod_id, input.owner, &proof).unwrap_err();
    assert_eq!(
        error.to_string(),
        PrecompileError::from(NodFactoryError::PayNoteContextMismatch {
            expected: nod_context(nod_id, U256::ZERO),
            actual: nod_context(other_id, U256::ZERO),
        })
        .to_string()
    );
    assert!(
        !world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap())
    );
    assert!(!is_settled(&mut world, nod_id));
    assert!(!is_settled(&mut world, other_id));
}

#[test]
fn a_stranger_can_relay_a_paynote_proof_naming_the_nod_owner() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x68));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let stranger = Address::repeat_byte(0x69);
    let (proof, nullifier) = world.fund_note(NOTE_ASSET, nod_id, cost, cost);

    world.settle(nod_id, stranger, &proof).unwrap();

    let item = world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .unwrap();
    assert!(item.is_settled);
    assert_eq!(item.owner, input.owner);
    assert!(world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap()));
    let paid = world
        .provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| INodFactory::NodPaid::decode_log_data(&event.data).ok())
        .last()
        .expect("NodPaid event");
    assert_eq!(paid.owner, input.owner);
    assert_eq!(paid.nodId, nod_id.to_u256());
}

#[test]
fn a_stranger_can_mine_with_the_owners_auth() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x6a));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    let proof = world.covering_proof(nod_id, &input);
    world.settle(nod_id, input.owner, &proof).unwrap();
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
fn a_paynote_in_the_wrong_asset_cannot_pay_this_nod() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x66));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let other_asset = Address::repeat_byte(0x67);
    world.register_settlement_asset(other_asset, 978);
    let cost = cost_of(&input);
    let (proof, _nullifier) = world.fund_bound(
        other_asset,
        nod_id,
        U256::ZERO,
        U256::from(cost),
        U256::from(cost),
    );
    let nonce = world.pow_nonce(nod_id);

    let error = world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::SettlementCurrencyMismatch { iso_code: 978 }.to_string()),
        "unexpected error: {error:?}"
    );
}

#[test]
fn any_asset_registered_for_the_reference_currency_pays_the_nod() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x6a));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    // The registry lists interchangeable assets for the currency; the payer
    // picks which one their note carries, and it need not be the first.
    let second_asset = Address::repeat_byte(0x6b);
    world.register_reference_currency_assets(vec![NOTE_ASSET, second_asset]);
    let cost = cost_of(&input);
    let (proof, nullifier) = world.fund_note(second_asset, nod_id, cost, cost);
    let nonce = world.pow_nonce(nod_id);

    let minted = world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
    assert!(world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap()));
}

#[test]
fn one_note_cannot_pay_two_nods() {
    let mut world = World::new();
    let first = params(Address::repeat_byte(0x68));
    let first_id = world.issue(&first);
    world.qualify(first_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&first);
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, first_id, cost, cost);

    let first_nonce = world.pow_nonce(first_id);
    world
        .settle_and_mine(
            first_id,
            first.owner,
            first_nonce,
            mine_auth(first.owner, first.gratis_load_minor),
            &proof,
        )
        .unwrap();

    let second = NodIssueParams {
        worldwide_day: WorldwideDay::new(20_241_221),
        ..params(first.owner)
    };
    let second_id = world.issue(&second);
    world.qualify(second_id);
    let second_nonce = world.pow_nonce(second_id);
    let error = world
        .settle_and_mine(
            second_id,
            second.owner,
            second_nonce,
            mine_auth(second.owner, second.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason) if reason.contains("nullifier")),
        "replaying a spent note must revert, got: {error:?}"
    );
}

#[test]
fn a_paynote_can_cover_a_nod_cost_above_u128() {
    let mut world = World::new();
    // Above u128, yet inside the price ladder the call index bins by.
    let cost = (U256::from(1) << 129) + U256::from(17);
    let input = NodIssueParams {
        gratis_load_minor: U256::from(1_000_000),
        entry_price_minor: cost,
        ..params(Address::repeat_byte(0x6a))
    };
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let (proof, nullifier) = world.fund_note(NOTE_ASSET, nod_id, cost, cost);
    let nonce = world.pow_nonce(nod_id);

    let minted = world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .expect("U256 PayNote covers U256 Nod cost");
    assert_eq!(minted, input.gratis_load_minor);
    assert!(world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap()));
    let paid = world
        .provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| INodFactory::NodPaid::decode_log_data(&event.data).ok())
        .last()
        .expect("NodPaid event");
    assert_eq!(paid.paymentMinor, cost);
}

#[test]
fn settlement_charges_zk_verification_base_gas() {
    assert_eq!(
        crate::precompile::base_gas(&INodFactory::settleNodWithPayNoteCall::SELECTOR),
        outbe_primitives::storage::gas::ZK_VERIFY_GAS
    );
    assert_eq!(
        crate::precompile::base_gas(&INodFactory::materializationHeadCall::SELECTOR),
        outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS
    );
    assert_eq!(
        crate::precompile::base_gas(&[]),
        outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS
    );
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
            scope,
            parent,
            &calldata,
            Address::repeat_byte(0x91),
            U256::ZERO,
        )
    });

    assert!(result.is_err());
    assert_eq!(world.provider.storage, storage_before);
    assert_eq!(world.provider.get_ordered_events(), events_before);
}
