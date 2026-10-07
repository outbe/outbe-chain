use super::*;

#[test]
fn settlement_minimum_precedes_asset_and_currency_conversion() {
    let price = U256::from(19);
    let load = U256::from(25_629);
    for (decimals, rate, expected) in [
        (6, None, 1_u64),
        (18, None, 1_000_000_000_000),
        (6, Some((2, 1)), 2),
        (18, Some((1, 2)), 500_000_000_000),
    ] {
        let rate = rate.map(|(to, from)| (U256::from(to), U256::from(from)));
        assert_eq!(
            crate::runtime::settlement_units(price, load, rate, decimals).unwrap(),
            U256::from(expected)
        );
    }
    // The existing conversion guard still rejects a positive payment that the
    // asset cannot represent, including after a currency conversion.
    for (decimals, rate) in [(0, None), (6, Some((U256::ONE, U256::from(2))))] {
        let error = crate::runtime::settlement_units(price, load, rate, decimals).unwrap_err();
        assert_eq!(
            error.to_string(),
            PrecompileError::Revert("settlement cost rounds to zero".into()).to_string()
        );
    }
    // Non-dust obligations retain sub-minor-unit precision in wider assets.
    assert_eq!(
        crate::runtime::settlement_units(U256::from(1_500_001), U256::ONE, None, 18).unwrap(),
        U256::from(1_500_001_000_000_u64)
    );
    for (price, load) in [(U256::ZERO, U256::ONE), (U256::ONE, U256::ZERO)] {
        assert_eq!(
            crate::runtime::settlement_units(price, load, None, 6).unwrap(),
            U256::ZERO
        );
    }
    assert!(crate::runtime::settlement_units(U256::MAX, U256::from(2), None, 6).is_err());
    assert!(
        crate::runtime::settlement_units(price, load, Some((U256::ONE, U256::ZERO)), 6).is_err()
    );
}

#[test]
fn a_dust_cost_nod_requires_erc20_payment_and_quotes_one_minor_unit() {
    let mut world = World::new();
    let mut input = params(Address::repeat_byte(0x61));
    input.entry_price_minor = U256::from(19);
    input.gratis_load_minor = U256::from(25_629);
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(PAYMENT_ASSET);
    assert_eq!(
        public_nod_data(&mut world, nod_id).settlementCostMinor,
        U256::ONE
    );
    let quote = world
        .enter(|storage, scope, parent| {
            api::quote_settlement(&storage, scope, parent, nod_id, PAYMENT_ASSET)
        })
        .unwrap();
    assert_eq!(quote, (840, U256::ONE, U256::ZERO));

    world.provider.stub_sub_call_at_selector(
        PAYMENT_ASSET,
        IERC20::transferFromCall::SELECTOR,
        Bytes::from(IERC20::transferFromCall::abi_encode_returns(&true)),
    );
    world.provider.stub_sub_call_at_selector(
        PAYMENT_ASSET,
        IERC20::balanceOfCall::SELECTOR,
        Bytes::from(IERC20::balanceOfCall::abi_encode_returns(&U256::ZERO)),
    );
    // A transfer with no received funds must fail, even for a dust obligation.
    let error = world
        .enter(|storage, scope, parent| {
            api::settle_nod(
                &storage,
                scope,
                parent,
                api::SettleNodRequest {
                    caller: input.owner,
                    nod_id,
                    asset: PAYMENT_ASSET,
                    snapshot_id: U256::ZERO,
                },
            )
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        PrecompileError::from(NodFactoryError::SettlementAmountMismatch).to_string()
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
    world.qualify(nod_id);
    world.register_reference_currency_asset(PAYMENT_ASSET);
    assert_eq!(cost_of(&input), 500);
    assert_eq!(
        public_nod_data(&mut world, nod_id).settlementCostMinor,
        U256::from(500)
    );
    let quote = world
        .enter(|storage, scope, parent| {
            api::quote_settlement(&storage, scope, parent, nod_id, PAYMENT_ASSET)
        })
        .unwrap();
    assert_eq!(quote, (840, U256::from(500), U256::ZERO));
}

#[test]
fn a_wider_asset_quotes_the_cost_scaled_to_its_decimals() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x61));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(PAYMENT_ASSET);
    world.set_asset_decimals(PAYMENT_ASSET, 18);
    let cost = U256::from(cost_of(&input)) * U256::from(1_000_000_000_000u64);
    let quote = world
        .enter(|storage, scope, parent| {
            api::quote_settlement(&storage, scope, parent, nod_id, PAYMENT_ASSET)
        })
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
    world.qualify(nod_id);
    world.register_reference_currency_asset(PAYMENT_ASSET);
    let quote = world
        .enter(|storage, scope, parent| {
            api::quote_settlement(&storage, scope, parent, nod_id, PAYMENT_ASSET)
        })
        .unwrap();
    assert_eq!(quote, (840, cost, U256::ZERO));
}
