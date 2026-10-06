use super::*;

#[test]
fn a_position_reports_its_full_terms() {
    with_storage(Some(U256::from(2u64) * six_decimal_unit()), |storage| {
        let id = seed_and_send(
            storage,
            six_decimal_unit(),
            six_decimal_unit(),
            six_decimal_u128(),
        );
        let data = runtime::position_data(storage, id).unwrap();
        assert_eq!(data.merchant, ALICE);
        assert_eq!(data.sourceEntryPriceMinor, six_decimal_unit());
        assert_eq!(data.sourceFloorPriceMinor, six_decimal_unit());
        assert_eq!(data.issuanceCurrency, 840);
        assert_eq!(data.referenceCurrency, 840);
        assert_eq!(data.issuedAt, T_NOW);
        assert_eq!(data.expiresAt, T_NOW + POSITION_VALIDITY_SECONDS);
        assert_eq!(
            data.remainingCapacityMinor,
            sent_capacity(six_decimal_u128())
        );
    });
}

#[test]
fn cross_currency_settlement_rejects_a_leg_the_window_never_priced() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage_paying(Some(rate), STABLE, |storage, proof| {
        // EUR trades live, but the window it converts at holds no euro price.
        let eur_pair = outbe_oracle::api::AddressPair::new_coen_to(978);
        outbe_oracle::api::register_pair(storage.clone(), eur_pair).unwrap();
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            eur_pair,
            six_decimal_unit(),
            1,
            T_NOW,
        )
        .unwrap();
        OracleContract::new(storage.clone())
            .reference_currencies
            .push(978)
            .unwrap();
        seed_day_vwap(storage, 840, rate);
        let gem_id = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            840,
            978,
        )
        .unwrap();
        seed_qualifying_day(storage, gem_id);

        let error = runtime::settle_gem_with_paynote(storage, ALICE, gem_id, proof).unwrap_err();

        assert!(
            error.to_string().contains("oracle nominal unavailable"),
            "{error}"
        );
        assert_eq!(
            gem_api::get_gem(storage, gem_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn settle_rejects_wrong_settlement_currency() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage_paying(Some(rate), STABLE_EUR, |storage, proof| {
        let gem_id = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            840,
            840,
        )
        .unwrap();
        seed_qualifying_day(storage, gem_id);
        // Paying a USD gem with a EUR (978) stablecoin matches neither of its
        // currencies, so it reverts before any vault interaction.
        let res = runtime::settle_gem_with_paynote(storage, ALICE, gem_id, proof);
        assert!(err_msg(res).contains("does not match the gem"));
    });
}

#[test]
fn anyone_may_pay_for_a_gem_and_it_stays_with_its_owner() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(rate));
    let (gem_id, proof) = note_for_quoted_cost(&mut provider, STABLE, |storage| {
        let gem_id = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            840,
            840,
        )
        .unwrap();
        seed_qualifying_day(storage, gem_id);
        gem_id
    });
    StorageHandle::enter(&mut provider, |storage| {
        runtime::settle_gem_with_paynote(&storage, BOB, gem_id, &proof).unwrap();
        let item = gem_api::get_gem(&storage, gem_id).unwrap().unwrap();
        assert_eq!(item.state, GemState::Settled as u8);
        assert_eq!(item.owner, ALICE, "paying never moves the gem");
    });

    let event = provider
        .get_ordered_events()
        .iter()
        .filter_map(|log| crate::precompile::IGemFactory::GemSettled::decode_log(log).ok())
        .next()
        .expect("settlement emits GemSettled");
    assert_eq!(event.gemId, gem_id);
    assert_eq!(
        event.owner, ALICE,
        "the event names the owner, not the payer"
    );
}

#[test]
fn a_paynote_bound_to_another_gem_cannot_settle_this_one() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(rate));
    let (gem_id, proof) = note_for_quoted_cost(&mut provider, STABLE, |storage| {
        let gem_id = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            840,
            840,
        )
        .unwrap();
        seed_qualifying_day(storage, gem_id);
        gem_id
    });
    let other = StorageHandle::enter(&mut provider, |storage| {
        // gem_id hashes owner, load, and block. A different load keeps this gem distinct.
        let other = issue_at_live_rate(
            &storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(11u64) * six_decimal_unit(),
            840,
            840,
        )
        .unwrap();
        seed_qualifying_day(&storage, other);
        other
    });
    StorageHandle::enter(&mut provider, |storage| {
        let rejected = runtime::settle_gem_with_paynote(&storage, BOB, other, &proof).unwrap_err();
        assert!(
            rejected.to_string().contains("does not match settlement"),
            "{rejected}"
        );
        assert_eq!(
            gem_api::get_gem(&storage, other).unwrap().unwrap().state,
            GemState::Issued as u8
        );
        runtime::settle_gem_with_paynote(&storage, BOB, gem_id, &proof).unwrap();
        let item = gem_api::get_gem(&storage, gem_id).unwrap().unwrap();
        assert_eq!(item.state, GemState::Settled as u8);
        assert_eq!(item.owner, ALICE);
    });
}

#[test]
fn a_nod_bound_paynote_cannot_settle_the_gem_sharing_its_id() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(rate));
    let (gem_id, cost, snapshot) = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = issue_at_live_rate(
            &storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            840,
            840,
        )
        .unwrap();
        seed_qualifying_day(&storage, gem_id);
        let (_, cost, snapshot) = runtime::quote_settlement(&storage, gem_id, STABLE).unwrap();
        (gem_id, cost, snapshot)
    });
    let nod_context = outbe_paynote::api::settlement_context(
        outbe_paynote::api::SettlementDomain::Nod,
        B256::from(gem_id),
        U256::ONE,
        snapshot,
    )
    .unwrap();
    let nod_bound =
        outbe_paynote::test_support::note_and_spend_proof(1, STABLE, nod_context, cost, cost);
    outbe_paynote::test_support::seed_pool(&mut provider, 1, &[nod_bound.commitment]);
    let before = provider.storage.clone();

    let rejected = StorageHandle::enter(&mut provider, |storage| {
        runtime::settle_gem_with_paynote(&storage, BOB, gem_id, &nod_bound.proof)
    });
    assert_eq!(
        err_msg(rejected),
        format!(
            "{:?}",
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::PayNoteContextMismatch {
                    expected: gem_context(gem_id, snapshot),
                    actual: nod_context,
                }
            )
        )
    );
    assert_eq!(
        provider.storage, before,
        "the note and the gem are untouched"
    );

    let gem_bound = outbe_paynote::test_support::note_and_spend_proof(
        1,
        STABLE,
        gem_context(gem_id, snapshot),
        cost,
        cost,
    );
    StorageHandle::enter(&mut provider, |storage| {
        runtime::settle_gem_with_paynote(&storage, BOB, gem_id, &gem_bound.proof).unwrap();
        assert_eq!(
            gem_api::get_gem(&storage, gem_id).unwrap().unwrap().state,
            GemState::Settled as u8
        );
    });
}

#[test]
fn settle_rejects_non_qualified_state() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let gem_id = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            840,
            840,
        )
        .unwrap();
        // WALLET is born Issued and no closed day has cleared its floor yet.
        let res = runtime::settle_gem_with_paynote(storage, ALICE, gem_id, &[]);
        assert!(err_msg(res).contains("invalid state"));
    });
}

#[test]
fn gem_pow_binds_the_owner_and_its_own_domain() {
    use outbe_common::pow::{compute_mining_pow_hash, MiningDomain, SINGLE_EXERCISE_SEQUENCE};

    let gem_id = U256::from(0x1234_5678u64);
    let nonce = find_valid_nonce(gem_id, ALICE);

    assert!(runtime::validate_pow(gem_id, ALICE, nonce).is_ok());
    assert_ne!(
        compute_mining_pow_hash(
            MiningDomain::Gem,
            gem_id,
            ALICE,
            SINGLE_EXERCISE_SEQUENCE,
            nonce
        ),
        compute_mining_pow_hash(
            MiningDomain::Gem,
            gem_id,
            BOB,
            SINGLE_EXERCISE_SEQUENCE,
            nonce
        )
    );
    assert_ne!(
        compute_mining_pow_hash(
            MiningDomain::Gem,
            gem_id,
            ALICE,
            SINGLE_EXERCISE_SEQUENCE,
            nonce
        ),
        compute_mining_pow_hash(
            MiningDomain::Nod,
            gem_id,
            ALICE,
            SINGLE_EXERCISE_SEQUENCE,
            nonce
        ),
        "a Nod nonce must not settle a Gem"
    );
}
