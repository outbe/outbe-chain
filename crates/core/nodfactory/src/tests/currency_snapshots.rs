use super::*;

#[test]
fn quote_prices_both_rails() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa2));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(PAYMENT_ASSET, 840);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));

    let (ref_iso, ref_amount, iss_iso, iss_amount) = world.enter(|storage, scope, parent| {
        let (ref_iso, ref_amount, _) =
            api::quote_settlement(&storage, scope, parent, nod_id, PAYMENT_ASSET).unwrap();
        let (iss_iso, iss_amount, _) =
            api::quote_settlement(&storage, scope, parent, nod_id, EUR_ASSET).unwrap();
        (ref_iso, ref_amount, iss_iso, iss_amount)
    });
    assert_eq!(ref_iso, 840);
    assert_eq!(iss_iso, 978);
    assert_eq!(ref_amount, U256::from(cost_of(&input)));
    assert_eq!(iss_amount, ref_amount / U256::from(2u64));
}

#[test]
fn the_issuance_rail_converts_at_the_trailing_window_not_the_closed_day() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa8));
    let nod_id = world.issue(&input);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));

    let quoted = world.enter(|storage, scope, parent| {
        use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};
        let day = previous_date_key(timestamp_to_date_key(
            storage.timestamp().unwrap().to::<u64>(),
        ));
        let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 978)
            .unwrap()
            .unwrap();
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .record_utc_day_vwap(day, index, U256::from(4 * SIX_DECIMALS))
            .unwrap();
        api::quote_settlement(&storage, scope, parent, nod_id, EUR_ASSET)
            .unwrap()
            .1
    });
    assert_eq!(quoted, U256::from(cost_of(&input) / 2));
}

#[test]
fn an_issuance_payment_must_name_the_snapshot_required_at_execution() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa9));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(PAYMENT_ASSET, 840);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));
    let quote = |world: &mut World| {
        world
            .enter(|storage, scope, parent| {
                api::quote_settlement(&storage, scope, parent, nod_id, EUR_ASSET)
            })
            .unwrap()
    };
    let (_, amount, quoted) = quote(&mut world);
    let snapshot = outbe_oracle::api::VwapSnapshotId::from_u256(quoted).unwrap();
    let mismatch = PrecompileError::from(NodFactoryError::SettlementAmountMismatch).to_string();

    world.set_timestamp(snapshot.cutoff() + 3_599);
    let same_hour = settle_erc20(&mut world, nod_id, input.owner, EUR_ASSET, quoted);
    assert_eq!(same_hour.unwrap_err().to_string(), mismatch);

    world.set_timestamp(snapshot.cutoff() + 3_600);
    let (_, next_amount, next) = quote(&mut world);
    assert_eq!(next_amount, amount, "the next window holds the same price");
    let stale = settle_erc20(&mut world, nod_id, input.owner, EUR_ASSET, quoted);
    assert_eq!(
        stale.unwrap_err().to_string(),
        PrecompileError::from(NodFactoryError::VwapSnapshotMismatch {
            authorized: quoted,
            required: next,
        })
        .to_string()
    );
    assert!(!is_settled(&mut world, nod_id));

    let reference = settle_erc20(&mut world, nod_id, input.owner, PAYMENT_ASSET, quoted);
    assert_eq!(reference.unwrap_err().to_string(), mismatch);
}

#[test]
fn the_hourly_rollover_at_the_settlement_deadline_grants_no_grace() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xab));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));
    let quote = |world: &mut World| {
        world
            .enter(|storage, scope, parent| {
                api::quote_settlement(&storage, scope, parent, nod_id, EUR_ASSET)
            })
            .unwrap()
    };
    let (_, _, quoted) = quote(&mut world);
    let deadline = outbe_oracle::api::VwapSnapshotId::from_u256(quoted)
        .unwrap()
        .cutoff()
        + 3_600;
    world.mark_called(nod_id, deadline - u64::from(CALL_NOTICE_PERIOD));

    world.set_timestamp(deadline);
    let (_, _, required) = quote(&mut world);
    let stale = settle_erc20(&mut world, nod_id, input.owner, EUR_ASSET, quoted);
    assert_eq!(
        stale.unwrap_err().to_string(),
        PrecompileError::from(NodFactoryError::VwapSnapshotMismatch {
            authorized: quoted,
            required,
        })
        .to_string()
    );

    world.set_timestamp(deadline + 1);
    let late = settle_erc20(&mut world, nod_id, input.owner, EUR_ASSET, required);
    assert_eq!(
        late.unwrap_err().to_string(),
        PrecompileError::from(NodFactoryError::CallDeadlineExpired).to_string()
    );
    assert!(!is_settled(&mut world, nod_id));
}

#[test]
fn a_snapshot_of_another_policy_is_rejected() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xaa));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));
    let (_, _, quoted) = world
        .enter(|storage, scope, parent| {
            api::quote_settlement(&storage, scope, parent, nod_id, EUR_ASSET)
        })
        .unwrap();
    let cutoff = outbe_oracle::api::VwapSnapshotId::from_u256(quoted)
        .unwrap()
        .cutoff();
    let other_policy = outbe_oracle::api::get_vwap_snapshot_id(
        cutoff,
        &outbe_oracle::api::VwapPolicy {
            policy_version: 2,
            ..outbe_oracle::api::DEFAULT_VWAP_POLICY
        },
    )
    .unwrap();

    let error = settle_erc20(
        &mut world,
        nod_id,
        input.owner,
        EUR_ASSET,
        other_policy.to_u256(),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        PrecompileError::from(NodFactoryError::VwapSnapshotMismatch {
            authorized: other_policy.to_u256(),
            required: quoted,
        })
        .to_string()
    );
    assert!(!is_settled(&mut world, nod_id));
}

#[test]
fn issuance_erc20_admission_uses_the_quoted_converted_cost() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa3));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));
    world.provider.stub_sub_call_at_selector(
        EUR_ASSET,
        IERC20::transferFromCall::SELECTOR,
        Bytes::from(IERC20::transferFromCall::abi_encode_returns(&true)),
    );
    world.provider.stub_sub_call_at_selector(
        EUR_ASSET,
        IERC20::approveCall::SELECTOR,
        Bytes::from(IERC20::approveCall::abi_encode_returns(&true)),
    );
    world.provider.stub_sub_call_at_selector(
        EUR_ASSET,
        IERC20::balanceOfCall::SELECTOR,
        Bytes::from(IERC20::balanceOfCall::abi_encode_returns(&U256::ZERO)),
    );

    let quoted = world
        .enter(|storage, scope, parent| {
            api::quote_settlement(&storage, scope, parent, nod_id, EUR_ASSET)
        })
        .unwrap();
    assert_eq!(quoted.0, 978);
    assert_eq!(quoted.1, U256::from(cost_of(&input) / 2));

    // Admission and conversion ran; the fixed balance stub cannot show a delta.
    let error = world
        .enter(|storage, scope, parent| {
            api::settle_nod(
                &storage,
                scope,
                parent,
                input.owner,
                nod_id,
                EUR_ASSET,
                quoted.2,
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
fn settle_rejects_an_asset_with_no_registered_vault() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0xa4));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(PAYMENT_ASSET);
    world.provider.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall::SELECTOR,
        Bytes::from(IVaultRouter::assetVaultsCountCall::abi_encode_returns(
            &U256::ZERO,
        )),
    );
    let erc20_error = world
        .enter(|storage, scope, parent| {
            api::settle_nod(
                &storage,
                scope,
                parent,
                input.owner,
                nod_id,
                PAYMENT_ASSET,
                U256::ZERO,
            )
        })
        .unwrap_err();
    assert_eq!(
        erc20_error.to_string(),
        PrecompileError::from(NodFactoryError::SettlementAssetNotRegistered {
            asset: PAYMENT_ASSET
        })
        .to_string()
    );
    assert!(!is_settled(&mut world, nod_id));
}

#[test]
fn issuance_rail_rejects_a_leg_the_window_never_priced() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa5));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    // The euro trades live, but the window it converts at holds no euro price.
    world.publish_coen_spot(978, U256::from(SIX_DECIMALS));

    let error = settle_erc20(&mut world, nod_id, input.owner, EUR_ASSET, U256::ZERO).unwrap_err();
    assert_eq!(
        error.to_string(),
        PrecompileError::from(NodFactoryError::OracleUnavailable).to_string()
    );
    assert!(!is_settled(&mut world, nod_id));

    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));
    let live = world.enter(|storage, scope, parent| {
        api::quote_settlement(&storage, scope, parent, nod_id, EUR_ASSET)
            .unwrap()
            .2
    });
    // Priced now, the payment clears conversion and stops at the balance stub.
    let error = settle_erc20(&mut world, nod_id, input.owner, EUR_ASSET, live).unwrap_err();
    assert_eq!(
        error.to_string(),
        PrecompileError::from(NodFactoryError::SettlementAmountMismatch).to_string()
    );
}

#[test]
fn issuance_rail_rejects_a_missing_cross_rate() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa6));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));

    let error = settle_erc20(&mut world, nod_id, input.owner, EUR_ASSET, U256::ZERO).unwrap_err();
    assert!(
        error.to_string().contains("oracle nominal unavailable"),
        "unexpected error: {error}"
    );
    assert!(!is_settled(&mut world, nod_id));
}

#[test]
fn quote_settlement_dispatch() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa7));
    let nod_id = world.issue(&input);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));

    let out = world
        .enter(|storage, scope, parent| {
            crate::precompile::dispatch(
                storage,
                ExecutionReaders { scope, parent },
                &INodFactory::quoteSettlementCall {
                    nodId: nod_id.to_u256(),
                    asset: EUR_ASSET,
                }
                .abi_encode(),
                input.owner,
                U256::ZERO,
            )
        })
        .unwrap();
    let ret = INodFactory::quoteSettlementCall::abi_decode_returns(&out).unwrap();
    assert_eq!(ret.settlementCurrency, 978);
    assert_eq!(ret.paymentMinor, U256::from(cost_of(&input) / 2));
}
