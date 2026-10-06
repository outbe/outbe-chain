use super::*;

#[test]
fn the_gem_crate_names_the_genesis_type_this_factory_issues() {
    assert_eq!(GemTypes::Genesis as u8, outbe_gem::GENESIS_GEM_TYPE);
}

#[test]
fn issue_genesis_pays_like_agents_but_carries_no_floor() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let load = U256::from(10u64) * six_decimal_unit();
        let gem_id = issue_at_live_rate(storage, ALICE, GemTypes::Genesis, load, 840, 840).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        assert_eq!(
            runtime::gem_cost_minor(&item).unwrap(),
            U256::from(20u64) * six_decimal_unit()
        );
        assert_eq!(item.entry_price_minor, rate);
        assert_eq!(item.floor_price_minor, U256::ZERO);
        assert_eq!(
            item.call_price_minor,
            rate * U256::from(228u64) / U256::from(100u64)
        );
        assert_eq!(item.state, GemState::Issued as u8);
        assert!(
            !gem_api::is_qualified(storage, &item).unwrap(),
            "a zero floor still waits for its first finalized day"
        );
        assert_eq!(item.gem_type, GemTypes::Genesis as u8);

        let factory = GemFactoryContract::new(storage.clone());
        assert_eq!(factory.total_gems_issued.read().unwrap(), U256::from(1u64));
    });
}

/// A Genesis gem is payable once its first full day closes: a zero floor clears at any price.
#[test]
fn a_genesis_gem_is_payable_once_its_first_day_closes() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let load = U256::from(10u64) * six_decimal_unit();
        let gem_id = issue_at_live_rate(storage, ALICE, GemTypes::Genesis, load, 840, 840).unwrap();
        seed_qualifying_day(storage, gem_id);
        admitted_at_quote(storage, gem_id, STABLE);
    });
}

#[test]
fn issue_validator_post_genesis_behaves_like_wallet() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let load = U256::from(5u64) * six_decimal_unit();
        let gem_id =
            issue_at_live_rate(storage, ALICE, GemTypes::Validator, load, 840, 840).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        // Same as WALLET: cost = entry x load, floor with 8% markup, Issued.
        assert_eq!(
            runtime::gem_cost_minor(&item).unwrap(),
            U256::from(10u64) * six_decimal_unit()
        );
        assert_eq!(
            item.floor_price_minor,
            rate * U256::from(108u64) / U256::from(100u64)
        );
        assert_eq!(item.state, GemState::Issued as u8);
        assert_eq!(item.gem_type, GemTypes::Validator as u8);
    });
}

#[test]
fn issue_wallet_cost_and_floor_markup_state_issued() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let load = U256::from(5u64) * six_decimal_unit();
        let gem_id = issue_at_live_rate(storage, ALICE, GemTypes::Wallet, load, 840, 840).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        // entry = coen_rate = 2; cost = entry * load / six_decimal_unit() = 2 * 5 = 10
        assert_eq!(item.entry_price_minor, rate);
        assert_eq!(
            runtime::gem_cost_minor(&item).unwrap(),
            U256::from(10u64) * six_decimal_unit()
        );
        // floor = rate * 108 / 100 = 2 * 1.08 = 2.16
        assert_eq!(
            item.floor_price_minor,
            rate * U256::from(108u64) / U256::from(100u64)
        );
        assert_eq!(item.state, GemState::Issued as u8);
    });
}

#[test]
fn issue_charges_one_minor_unit_when_the_six_decimal_cost_rounds_to_zero() {
    with_storage(Some(U256::ONE), |storage| {
        let gem_id =
            issue_at_live_rate(storage, ALICE, GemTypes::Wallet, U256::ONE, 840, 840).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        assert_eq!(runtime::gem_cost_minor(&item).unwrap(), U256::ONE);
    });
}

#[test]
fn issue_sra_applies_64_percent_discount() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let load = U256::from(10u64) * six_decimal_unit();
        let gem_id = issue_at_live_rate(storage, ALICE, GemTypes::Sra, load, 840, 840).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        // entry = rate = 2; cost = 2 * 10 * 64 / 100 = 12.8 (six-decimal)
        let expected = rate * load * U256::from(64u64) / U256::from(100u64) / six_decimal_unit();
        assert_eq!(runtime::gem_cost_minor(&item).unwrap(), expected);
    });
}

#[test]
fn issue_cca_no_discount() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let load = U256::from(7u64) * six_decimal_unit();
        let gem_id = issue_at_live_rate(storage, ALICE, GemTypes::Cca, load, 840, 840).unwrap();

        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
        // entry = rate = 2; cost = 2 * 7 = 14
        assert_eq!(
            runtime::gem_cost_minor(&item).unwrap(),
            U256::from(14u64) * six_decimal_unit()
        );
    });
}

#[test]
fn issue_gem_rejects_merchant_type() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let res = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Merchant,
            U256::from(1u64) * six_decimal_unit(),
            840,
            840,
        );
        assert!(err_msg(res).contains("unsupported gem type"));
    });
}

#[test]
fn issue_zero_owner_rejected() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        let res = issue_at_live_rate(
            storage,
            Address::ZERO,
            GemTypes::Wallet,
            U256::from(1u64) * six_decimal_unit(),
            840,
            840,
        );
        assert!(err_msg(res).contains("invalid owner"));
    });
}

#[test]
fn issue_no_oracle_setup_rejected() {
    // The reference currency is registered but its COEN pair is not, so the gem
    // has no price to anchor its entry, floor and call to. Issuing reverts.
    with_storage(None, |storage| {
        let res = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(1u64) * six_decimal_unit(),
            840,
            840,
        );
        assert!(err_msg(res).contains("not registered"));
    });
}

#[test]
fn issue_rejects_a_stale_oracle_rate_before_writing_a_gem() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            outbe_oracle::api::DAY_TYPE_PAIR,
            rate,
            1,
            T_NOW - outbe_oracle::constants::FX_RATE_MAX_AGE_SECONDS - 1,
        )
        .unwrap();

        let error = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            six_decimal_unit(),
            840,
            840,
        )
        .unwrap_err();

        assert!(error.to_string().contains("stale"), "{error}");
        assert_eq!(
            GemFactoryContract::new(storage.clone())
                .total_gems_issued
                .read()
                .unwrap(),
            U256::ZERO
        );
    });
}
