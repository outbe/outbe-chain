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
    world.register_reference_currency_asset(NOTE_ASSET);
    assert_eq!(
        public_nod_data(&mut world, nod_id).settlementCostMinor,
        U256::ONE
    );
    let quote = world
        .enter(|storage, scope, parent| {
            api::quote_settlement(&storage, scope, parent, nod_id, NOTE_ASSET)
        })
        .unwrap();
    assert_eq!(quote, (840, U256::ONE, U256::ZERO));

    world.provider.stub_sub_call_at_selector(
        NOTE_ASSET,
        IERC20::transferFromCall::SELECTOR,
        Bytes::from(IERC20::transferFromCall::abi_encode_returns(&true)),
    );
    world.provider.stub_sub_call_at_selector(
        NOTE_ASSET,
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
                input.owner,
                nod_id,
                NOTE_ASSET,
                U256::ZERO,
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
fn a_cost_that_does_not_divide_evenly_is_floored_and_the_note_matches_it() {
    // 500.001 six-decimal units: the chain charges 500, the figure
    // `settlementCostMinor` advertises. Rounding up would demand 501.
    let mut world = World::new();
    let input = NodIssueParams {
        entry_price_minor: U256::from(500_001u64),
        ..params(Address::repeat_byte(0x61))
    };
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    assert_eq!(cost, 500);
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
