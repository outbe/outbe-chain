use super::*;

#[test]
fn quote_prices_both_rails() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa2));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(PAYMENT_ASSET, 840);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SCALE_1E6_U64));
    world.publish_coen_rate(978, U256::from(SCALE_1E6_U64));

    let (ref_iso, ref_amount, iss_iso, iss_amount) = world.enter(|storage| {
        let (ref_iso, ref_amount, _) =
            api::quote_settlement(&storage, nod_id, PAYMENT_ASSET).unwrap();
        let (iss_iso, iss_amount, _) = api::quote_settlement(&storage, nod_id, EUR_ASSET).unwrap();
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
    world.publish_coen_rate(840, U256::from(2 * SCALE_1E6_U64));
    world.publish_coen_rate(978, U256::from(SCALE_1E6_U64));

    let quoted = world.enter(|storage| {
        use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};
        let day = previous_date_key(timestamp_to_date_key(
            storage.timestamp().unwrap().to::<u64>(),
        ));
        let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 978)
            .unwrap()
            .unwrap();
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .record_utc_day_vwap(day, index, U256::from(4 * SCALE_1E6_U64))
            .unwrap();
        api::quote_settlement(&storage, nod_id, EUR_ASSET)
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
    world.publish_coen_rate(840, U256::from(2 * SCALE_1E6_U64));
    world.publish_coen_rate(978, U256::from(SCALE_1E6_U64));
    let quote = |world: &mut World| {
        world
            .enter(|storage| api::quote_settlement(&storage, nod_id, EUR_ASSET))
            .unwrap()
    };
    let (_, amount, quoted) = quote(&mut world);
    let snapshot = outbe_oracle::api::VwapSnapshotId::from_u256(quoted).unwrap();
    let mismatch = PrecompileError::from(SettlementError::AmountMismatch).to_string();

    world.set_timestamp(snapshot.cutoff() + 3_599);
    let same_hour = settle_erc20(&mut world, nod_id, input.owner, EUR_ASSET, quoted);
    assert_eq!(same_hour.unwrap_err().to_string(), mismatch);

    world.set_timestamp(snapshot.cutoff() + 3_600);
    let (_, next_amount, next) = quote(&mut world);
    assert_eq!(next_amount, amount, "the next window holds the same price");
    let stale = settle_erc20(&mut world, nod_id, input.owner, EUR_ASSET, quoted);
    assert_eq!(
        stale.unwrap_err().to_string(),
        PrecompileError::from(SettlementError::SnapshotMismatch {
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
    world.publish_coen_rate(840, U256::from(2 * SCALE_1E6_U64));
    world.publish_coen_rate(978, U256::from(SCALE_1E6_U64));
    let quote = |world: &mut World| {
        world
            .enter(|storage| api::quote_settlement(&storage, nod_id, EUR_ASSET))
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
        PrecompileError::from(SettlementError::SnapshotMismatch {
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
    world.publish_coen_rate(840, U256::from(2 * SCALE_1E6_U64));
    world.publish_coen_rate(978, U256::from(SCALE_1E6_U64));
    let (_, _, quoted) = world
        .enter(|storage| api::quote_settlement(&storage, nod_id, EUR_ASSET))
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
        PrecompileError::from(SettlementError::SnapshotMismatch {
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
    world.publish_coen_rate(840, U256::from(2 * SCALE_1E6_U64));
    world.publish_coen_rate(978, U256::from(SCALE_1E6_U64));
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

    let quoted = world
        .enter(|storage| api::quote_settlement(&storage, nod_id, EUR_ASSET))
        .unwrap();
    assert_eq!(quoted.0, 978);
    assert_eq!(quoted.1, U256::from(cost_of(&input) / 2));

    // The factory's balance as settlement reads it: before the transfer, after
    // it, and after the vault deposit. The token moves exactly the quote.
    world.provider.stub_sub_call_sequence_at_selector(
        EUR_ASSET,
        IERC20::balanceOfCall::SELECTOR,
        [U256::ZERO, quoted.1, U256::ZERO]
            .map(|balance| Bytes::from(IERC20::balanceOfCall::abi_encode_returns(&balance))),
    );
    world
        .enter(|storage| {
            api::settle_nod(
                &storage,
                api::SettleNodRequest {
                    caller: input.owner,
                    nod_id,
                    asset: EUR_ASSET,
                    snapshot_id: quoted.2,
                },
            )
        })
        .unwrap();
    assert!(is_settled(&mut world, nod_id));
    let paid = paid_event(&world);
    assert_eq!(paid.asset, EUR_ASSET);
    assert_eq!(paid.paymentMinor, quoted.1);
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
        .enter(|storage| {
            api::settle_nod(
                &storage,
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
        erc20_error.to_string(),
        PrecompileError::from(SettlementError::AssetNotRegistered {
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
    world.publish_coen_rate(840, U256::from(2 * SCALE_1E6_U64));
    // The euro trades live, but the window it converts at holds no euro price.
    world.publish_coen_spot(978, U256::from(SCALE_1E6_U64));

    let error = settle_erc20(&mut world, nod_id, input.owner, EUR_ASSET, U256::ZERO).unwrap_err();
    assert_eq!(
        error.to_string(),
        PrecompileError::from(SettlementError::OracleUnavailable).to_string()
    );
    assert!(!is_settled(&mut world, nod_id));

    world.publish_coen_rate(978, U256::from(SCALE_1E6_U64));
    let live = world.enter(|storage| {
        api::quote_settlement(&storage, nod_id, EUR_ASSET)
            .unwrap()
            .2
    });
    // Priced now, the payment clears conversion and stops at the balance stub.
    let error = settle_erc20(&mut world, nod_id, input.owner, EUR_ASSET, live).unwrap_err();
    assert_eq!(
        error.to_string(),
        PrecompileError::from(SettlementError::AmountMismatch).to_string()
    );
}

#[test]
fn issuance_rail_rejects_a_missing_cross_rate() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa6));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SCALE_1E6_U64));

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
    world.publish_coen_rate(840, U256::from(2 * SCALE_1E6_U64));
    world.publish_coen_rate(978, U256::from(SCALE_1E6_U64));

    let out = world
        .enter(|storage| {
            crate::precompile::dispatch(
                storage,
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

/// The window holds both prices, but the chain's own block record says every
/// hour ran 225 tally rounds. One observation is below two thirds of them, so
/// the finalized window prices nothing and the issuance rail refuses the
/// payment without a write. Once every hour holds two thirds, the same quote
/// prices.
#[test]
fn issuance_rail_rejects_a_window_whose_prices_fail_coverage() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xac));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_spot(840, U256::from(2 * SCALE_1E6_U64));
    world.publish_coen_spot(978, U256::from(SCALE_1E6_U64));
    let observation = [
        (
            outbe_oracle::api::AddressPair::new_coen_to(840),
            U256::from(2 * SCALE_1E6_U64),
            U256::from(SCALE_1E6_U64),
        ),
        (
            outbe_oracle::api::AddressPair::new_coen_to(978),
            U256::from(SCALE_1E6_U64),
            U256::from(SCALE_1E6_U64),
        ),
    ];

    // Eight hours of 1,800 blocks at a vote period of eight: 225 rounds each.
    // One observation in the first hour.
    let hour = 3_600;
    let snapshot = world.enter(|storage| {
        let snapshot = outbe_oracle::api::current_vwap_snapshot(storage.clone()).unwrap();
        let mut oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle.config_vote_period.write(8).unwrap();
        for k in 0..=8 {
            oracle
                .record_hour_block(snapshot.start() + k * hour, 1_000 + k * 1_800)
                .unwrap();
        }
        oracle
            .write_snapshot(snapshot.start(), &observation)
            .unwrap();
        snapshot
    });
    let slots = world.provider.storage.clone();
    let events = world.provider.get_ordered_events().to_vec();

    let error = settle_erc20(
        &mut world,
        nod_id,
        input.owner,
        EUR_ASSET,
        snapshot.to_u256(),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        PrecompileError::from(SettlementError::OracleUnavailable).to_string()
    );
    assert_eq!(world.provider.storage, slots);
    assert_eq!(world.provider.get_ordered_events(), events);
    assert!(!is_settled(&mut world, nod_id));

    // 180 of 225 rounds in every hour: above two thirds, the window prices.
    world.enter(|storage| {
        let mut oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        let mut timestamp = snapshot.start() + 20;
        while timestamp < snapshot.cutoff() {
            oracle.write_snapshot(timestamp, &observation).unwrap();
            timestamp += 20;
        }
    });
    let quoted = world
        .enter(|storage| api::quote_settlement(&storage, nod_id, EUR_ASSET))
        .unwrap();
    assert_eq!(quoted.1, U256::from(cost_of(&input) / 2));
}
