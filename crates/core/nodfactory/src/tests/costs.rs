use super::*;
use outbe_vaultrouter::settlement::SettlementPayment;

#[test]
fn a_dust_cost_nod_requires_erc20_payment_and_quotes_one_minor_unit() {
    let mut world = World::new();
    let mut input = params(Address::repeat_byte(0x61));
    input.entry_price_minor = U256::from(19);
    input.gratis_load_minor = U256::from(25_629);
    let nod_id = world.issue(&input);
    world.register_reference_currency_asset(SETTLEMENT_ASSET);
    let quote = world
        .enter(|storage| quote_settlement_decrypted(&storage, nod_id, SETTLEMENT_ASSET))
        .unwrap();
    assert_eq!(quote, (840, U256::ONE, U256::ZERO));

    // Even a dust cost goes to the router, which takes exactly one minor unit from the payer.
    let error = world
        .enter(|storage| {
            api::settle_nod(
                &storage,
                nod_id,
                SettlementPayment {
                    payer: input.owner,
                    asset: SETTLEMENT_ASSET,
                    snapshot_id: U256::ZERO,
                },
                AMOUNT_CAP,
            )
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        outbe_vaultrouter::test_support::unanswered_deposit_from().to_string()
    );
    let pull = outbe_vaultrouter::test_support::last_deposit_from(&world.provider).unwrap();
    assert_eq!(
        (pull.payer, pull.asset, pull.assetsMinor),
        (input.owner, SETTLEMENT_ASSET, U256::ONE)
    );
    assert!(!is_settled(&mut world, nod_id));
}

#[test]
fn a_cost_that_does_not_divide_evenly_is_floored_in_the_quote() {
    // 500.001 six-decimal units: the chain charges 500, the figure
    // `settlementCostMinor` advertises. Rounding up would demand 501.
    let mut world = World::new();
    let input = NodIssueParams {
        entry_price_minor: U256::from(500_001u64),
        ..params(Address::repeat_byte(0x61))
    };
    let nod_id = world.issue(&input);
    world.register_reference_currency_asset(SETTLEMENT_ASSET);
    assert_eq!(cost_of(&input), 500);
    let quote = world
        .enter(|storage| quote_settlement_decrypted(&storage, nod_id, SETTLEMENT_ASSET))
        .unwrap();
    assert_eq!(quote, (840, U256::from(500), U256::ZERO));
}

#[test]
fn a_wider_asset_quotes_the_cost_scaled_to_its_decimals() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x61));
    let nod_id = world.issue(&input);
    world.register_reference_currency_asset(SETTLEMENT_ASSET);
    world.set_asset_decimals(SETTLEMENT_ASSET, 18);
    let cost = U256::from(cost_of(&input)) * U256::from(1_000_000_000_000u64);
    let quote = world
        .enter(|storage| quote_settlement_decrypted(&storage, nod_id, SETTLEMENT_ASSET))
        .unwrap();
    assert_eq!(quote, (840, cost, U256::ZERO));
}

#[test]
fn a_nod_cost_above_u128_is_quoted_in_full() {
    let mut world = World::new();
    // Above u128, yet inside the price ladder the call index bins by.
    let cost = (U256::from(1) << 129) + U256::from(17);
    let input = NodIssueParams {
        gratis_load_minor: U256::from(1_000_000),
        entry_price_minor: cost,
        ..params(Address::repeat_byte(0x6a))
    };
    let nod_id = world.issue(&input);
    world.register_reference_currency_asset(SETTLEMENT_ASSET);
    let quote = world
        .enter(|storage| quote_settlement_decrypted(&storage, nod_id, SETTLEMENT_ASSET))
        .unwrap();
    assert_eq!(quote, (840, cost, U256::ZERO));
}

fn stub_funds(world: &mut World, funds: U256) {
    let funds = Bytes::from(funds.to_be_bytes::<32>().to_vec());
    world.provider.stub_sub_call_at_selector(
        SETTLEMENT_ASSET,
        IERC20::balanceOfCall::SELECTOR,
        funds.clone(),
    );
    world.provider.stub_sub_call_at_selector(
        SETTLEMENT_ASSET,
        IERC20::allowanceCall::SELECTOR,
        funds,
    );
}

fn settle_with_cap(world: &mut World, nod_id: U256, payer: Address, cap: U256) -> PrecompileError {
    world
        .enter(|storage| {
            api::settle_nod(
                &storage,
                nod_id,
                SettlementPayment {
                    payer,
                    asset: SETTLEMENT_ASSET,
                    snapshot_id: U256::ZERO,
                },
                cap,
            )
        })
        .unwrap_err()
}

/// The funds of the payer for `amountCapMinor` are checked before the cost, so a payer with
/// no funds learns nothing of it. A cost above the cap fails, and the chain takes only the cost.
#[test]
fn settle_nod_checks_the_funds_before_the_cost() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x61));
    let nod_id = world.issue(&input);
    world.register_reference_currency_asset(SETTLEMENT_ASSET);
    let cost = U256::from(cost_of(&input));
    let missing_funds = PrecompileError::from(SettlementError::InsufficientFunds).to_string();

    stub_funds(&mut world, cost / U256::from(2));
    for cap in [cost - U256::ONE, cost + U256::ONE] {
        let error = settle_with_cap(&mut world, nod_id, input.owner, cap);
        assert_eq!(error.to_string(), missing_funds, "cap {cap}");
    }

    stub_funds(&mut world, cost * U256::from(2));
    let error = settle_with_cap(&mut world, nod_id, input.owner, cost - U256::ONE);
    assert_eq!(
        error.to_string(),
        PrecompileError::from(SettlementError::CostAboveCap).to_string()
    );
    assert!(outbe_vaultrouter::test_support::last_deposit_from(&world.provider).is_none());
    assert!(!is_settled(&mut world, nod_id));

    let error = settle_with_cap(&mut world, nod_id, input.owner, cost * U256::from(2));
    assert_eq!(
        error.to_string(),
        outbe_vaultrouter::test_support::unanswered_deposit_from().to_string()
    );
    let pull = outbe_vaultrouter::test_support::last_deposit_from(&world.provider).unwrap();
    assert_eq!((pull.payer, pull.assetsMinor), (input.owner, cost));
}
