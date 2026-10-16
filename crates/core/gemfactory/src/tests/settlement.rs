use super::*;

#[test]
fn settlement_takes_the_rail_the_asset_matched() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage, world| {
        let gem_id = issue_at_live_rate(
            storage,
            world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            949,
            840,
        )
        .unwrap();
        assert_eq!(admitted_at_quote(storage, world, gem_id, STABLE).0, 840);
    });
}

/// The stubbed token answers `true` without moving a balance, so the direct path
/// must refuse the payment and leave the gem unsettled.
#[test]
fn erc20_settle_refuses_a_transfer_that_moves_nothing() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage, world| {
        let gem_id = issue_at_live_rate(
            storage,
            world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            840,
            840,
        )
        .unwrap();

        let call = crate::precompile::IGemFactory::settleGemCall {
            gemId: gem_id,
            asset: STABLE,
            snapshotId: U256::ZERO,
        };
        let res = crate::precompile::dispatch(
            storage.clone(),
            world.readers(),
            &call.abi_encode(),
            BOB,
            U256::ZERO,
        );
        assert!(err_msg(res).contains("unexpected amount"));
        assert_eq!(
            gem_view(storage, world, gem_id).item.state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn erc20_settle_rejects_a_foreign_currency_asset_before_paying() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage, world| {
        let gem_id = issue_at_live_rate(
            storage,
            world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            840,
            840,
        )
        .unwrap();

        let res = runtime::settle_gem(
            storage,
            &world.scope,
            &world.parent,
            crate::schema::GemSettlementParams {
                caller: ALICE,
                gem_id,
                asset: STABLE_EUR,
                snapshot_id: U256::ZERO,
            },
        );
        assert!(err_msg(res).contains("does not match"));
        assert_eq!(
            gem_view(storage, world, gem_id).item.state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn the_issuance_currency_settles_through_the_coen_pivot() {
    // COEN/USD 2.0, COEN/EUR 1.0: the same cost converts to half as many EUR units.
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(usd_rate), |storage, world| {
        register_currency(storage, 978, six_decimal_unit());
        seed_day_vwap(storage, 840, usd_rate);
        let gem_id = issue_at_live_rate(
            storage,
            world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        // Paying with the EUR asset picks the issuance rail.
        assert_eq!(
            admitted_at_quote(storage, world, gem_id, STABLE_EUR),
            (978, U256::from(10u64) * six_decimal_unit())
        );
    });
}

#[test]
fn the_issuance_rail_floors_the_whole_obligation_in_the_payers_favour() {
    // Exact obligation 2.5 EUR units: flooring charges 2, rounding up charged 3.
    with_storage(Some(U256::from(3_000_000u64)), |storage, world| {
        register_currency(storage, 978, U256::from(2_500_000u64));
        seed_day_vwap(storage, 840, U256::from(3_000_000u64));
        let gem_id =
            issue_at_live_rate(storage, world, ALICE, GemTypes::Wallet, U256::ONE, 978, 840)
                .unwrap();
        assert_eq!(
            runtime::gem_cost_minor(&gem_view(storage, world, gem_id)).unwrap(),
            U256::from(3u64)
        );
        assert_eq!(
            admitted_at_quote(storage, world, gem_id, STABLE_EUR).1,
            U256::from(2u64)
        );
    });
}

#[test]
fn a_wider_asset_keeps_what_the_six_decimal_cost_dropped() {
    // The reference cost floors to 1. The obligation is 1.500001. An
    // eighteen-decimal asset carries all of it. Scaling the floored 1 charged 1e12.
    with_storage(Some(U256::from(1_500_001u64)), |storage, world| {
        let gem_id =
            issue_at_live_rate(storage, world, ALICE, GemTypes::Wallet, U256::ONE, 840, 840)
                .unwrap();
        assert_eq!(
            runtime::gem_cost_minor(&gem_view(storage, world, gem_id)).unwrap(),
            U256::ONE
        );
        assert_eq!(
            admitted_at_quote(storage, world, gem_id, STABLE_18).1,
            U256::from(1_500_001_000_000u64)
        );
    });
}

#[test]
fn the_settlement_minimum_precedes_asset_and_currency_conversion() {
    with_storage(Some(U256::ONE), |storage, world| {
        let gem_id =
            issue_at_live_rate(storage, world, ALICE, GemTypes::Wallet, U256::ONE, 840, 840)
                .unwrap();
        let item = gem_view(storage, world, gem_id);
        let fx = |to: u64, from: u64| Some((U256::from(to), U256::from(from)));

        for (rate, decimals, expected) in [
            (None, 6, 1u64),
            (None, 8, 100),
            (None, 18, 1_000_000_000_000),
            (fx(2, 1), 6, 2),
            (fx(1, 2), 18, 500_000_000_000),
        ] {
            assert_eq!(
                runtime::settlement_units(&item, rate, decimals).unwrap(),
                U256::from(expected)
            );
        }
        // The minimum is one reference minor unit, which a narrower asset and an
        // unfavourable rate still floor away, as they do for Nod.
        for (rate, decimals) in [(None, 0), (fx(1, 2), 6)] {
            assert!(err_msg(runtime::settlement_units(&item, rate, decimals))
                .contains("settlement cost rounds to zero"));
        }
    });
}

#[test]
fn a_dust_gem_settles_for_one_minor_unit() {
    with_storage(Some(U256::ONE), |storage, world| {
        let gem_id =
            issue_at_live_rate(storage, world, ALICE, GemTypes::Wallet, U256::ONE, 840, 840)
                .unwrap();
        assert_eq!(
            admitted_at_quote(storage, world, gem_id, STABLE).1,
            U256::ONE
        );
    });
}

#[test]
fn the_settlement_minimum_ignores_the_sra_discount() {
    with_storage(Some(U256::ONE), |storage, world| {
        let gem_id =
            issue_at_live_rate(storage, world, ALICE, GemTypes::Sra, U256::ONE, 840, 840).unwrap();
        let item = gem_view(storage, world, gem_id);

        assert_eq!(
            runtime::settlement_units(&item, None, 6).unwrap(),
            U256::ONE
        );
    });
}

#[test]
fn settling_on_an_unregistered_issuance_leg_is_refused() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(usd_rate), |storage, world| {
        // The EUR asset is a valid vault asset, but COEN/978 was never registered,
        // so the pivot has no leg to convert through.
        seed_day_vwap(storage, 840, usd_rate);
        let gem_id = issue_at_live_rate(
            storage,
            world,
            ALICE,
            GemTypes::Wallet,
            six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        let res = runtime::settle_gem(
            storage,
            &world.scope,
            &world.parent,
            crate::schema::GemSettlementParams {
                caller: ALICE,
                gem_id,
                asset: STABLE_EUR,
                snapshot_id: U256::ZERO,
            },
        );
        assert!(err_msg(res).contains("oracle nominal unavailable"));
        assert_eq!(
            gem_view(storage, world, gem_id).item.state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn the_reference_currency_settles_without_reading_any_issuance_rate() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(usd_rate), |storage, world| {
        // Issuance 978 is never registered, so it carries no rate at all.
        let gem_id = issue_at_live_rate(
            storage,
            world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        assert_eq!(
            admitted_at_quote(storage, world, gem_id, STABLE),
            (840, U256::from(20u64) * six_decimal_unit())
        );
    });
}

#[test]
fn settle_rejects_an_asset_with_no_registered_vault() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let (mut provider, world) = test_storage(Some(rate));
    // Override the blanket vault count: this asset has none.
    provider.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall::SELECTOR,
        word(0),
    );
    StorageHandle::enter(&mut provider, |storage| {
        let gem_id = issue_at_live_rate(
            &storage,
            &world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            840,
            840,
        )
        .unwrap();
        let res = runtime::settle_gem(
            &storage,
            &world.scope,
            &world.parent,
            crate::schema::GemSettlementParams {
                caller: ALICE,
                gem_id,
                asset: STABLE,
                snapshot_id: U256::ZERO,
            },
        );
        assert!(err_msg(res).contains("no registered vault"));
    });
}

#[test]
fn settlement_scales_the_cost_to_the_asset_decimals() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(usd_rate), |storage, world| {
        let gem_id = issue_at_live_rate(
            storage,
            world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            840,
            840,
        )
        .unwrap();
        assert_eq!(
            admitted_at_quote(storage, world, gem_id, STABLE_18).1,
            U256::from(20u64) * six_decimal_unit() * U256::from(1_000_000_000_000u64)
        );
    });
}

#[test]
fn an_unassigned_issuance_code_mints_and_settles_on_the_reference_rail() {
    // 899 is inside the three-digit range but is not an assigned ISO 4217 code.
    // Gem no longer refuses it. Nothing prices against it, and no settlement
    // asset can ever report it. So it is inert, exactly as it is for a bid.
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage, world| {
        let gem_id = issue_at_live_rate(
            storage,
            world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            899,
            840,
        )
        .unwrap();
        assert_eq!(
            admitted_at_quote(storage, world, gem_id, STABLE),
            (840, U256::from(20u64) * six_decimal_unit())
        );
    });
}

#[test]
fn issue_rejects_an_issuance_code_outside_the_three_digit_range() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage, world| {
        for iso in [0u16, 1000u16] {
            let res = issue_at_live_rate(
                storage,
                world,
                ALICE,
                GemTypes::Wallet,
                six_decimal_unit(),
                iso,
                840,
            );
            assert!(err_msg(res).contains("is not an ISO 4217 currency code"));
        }
    });
}

#[test]
fn sending_rejects_a_series_whose_reference_currency_is_unregistered() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage, _world| {
        outbe_intex::api::create_series(
            storage,
            outbe_intex::CreateSeriesParams {
                series_id: source_intex_id(),
                worldwide_day: WorldwideDay::new(0),
                issued_units: SENT_UNITS as u32,
                promis_load_minor: six_decimal_u128(),
                entry_price_minor: six_decimal_unit(),
                floor_price_minor: six_decimal_unit(),
                call_price_minor: U256::ZERO,
                call_trigger: outbe_intex::IntexCallTrigger::default(),
                issued_at: T_NOW as u32,
                issuance_currency: 840,
                // 978 is never pushed into the reference registry here.
                reference_currency: 978,
            },
        )
        .unwrap();
        let res =
            runtime::issue_gem_position(storage, ALICE, source_intex_id(), U256::from(SENT_UNITS));
        assert!(err_msg(res).contains("reference currency"));
    });
}

#[test]
fn the_quote_prices_both_rails() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(usd_rate), |storage, world| {
        register_currency(storage, 978, six_decimal_unit());
        seed_day_vwap(storage, 840, usd_rate);
        let gem_id = issue_at_live_rate(
            storage,
            world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        let (ref_iso, ref_amount) = admitted_at_quote(storage, world, gem_id, STABLE);
        let (iss_iso, iss_amount) = admitted_at_quote(storage, world, gem_id, STABLE_EUR);
        assert_eq!(ref_iso, 840);
        assert_eq!(iss_iso, 978);
        assert_eq!(iss_amount, ref_amount / U256::from(2u64));
    });
}

/// The settlement deadline of a called Gem falls on an hour boundary, where the
/// required snapshot changes. The quote taken before it is stale at the
/// deadline, and one second later the deadline itself refuses the payment.
/// Neither attempt writes.
#[test]
fn the_hourly_rollover_at_the_settlement_deadline_grants_no_grace() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    let (mut provider, world) = test_storage(Some(usd_rate));
    let (gem_id, quoted) = StorageHandle::enter(&mut provider, |storage| {
        register_currency(&storage, 978, six_decimal_unit());
        seed_day_vwap(&storage, 840, usd_rate);
        let gem_id = issue_at_live_rate(
            &storage,
            &world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        let (_, _, quoted) =
            runtime::quote_settlement(&storage, &world.scope, &world.parent, gem_id, STABLE_EUR)
                .unwrap();
        (gem_id, quoted)
    });
    let deadline = outbe_oracle::api::VwapSnapshotId::from_u256(quoted)
        .unwrap()
        .cutoff()
        + 3_600;
    StorageHandle::enter(&mut provider, |storage| {
        let view = gem_view(&storage, &world, gem_id);
        let bucket = view.item.bucket_key;
        GemContract::new(storage.clone())
            .bucket_called_at
            .write(
                &bucket,
                deadline - u64::from(view.terms.call_notice_period_seconds),
            )
            .unwrap();
    });

    provider.set_timestamp(U256::from(deadline));
    StorageHandle::enter(&mut provider, |storage| {
        let view = gem_view(&storage, &world, gem_id);
        assert_eq!(view.effective_state(deadline), GemState::Called as u8);
        let (_, _, required) =
            runtime::quote_settlement(&storage, &world.scope, &world.parent, gem_id, STABLE_EUR)
                .unwrap();
        let stale = runtime::settle_gem(
            &storage,
            &world.scope,
            &world.parent,
            crate::schema::GemSettlementParams {
                caller: ALICE,
                gem_id,
                asset: STABLE_EUR,
                snapshot_id: quoted,
            },
        );
        assert_eq!(
            err_msg(stale),
            format!(
                "{:?}",
                outbe_primitives::error::PrecompileError::from(
                    outbe_vaultrouter::settlement::SettlementError::SnapshotMismatch {
                        authorized: quoted,
                        required,
                    }
                )
            )
        );
    });

    provider.set_timestamp(U256::from(deadline + 1));
    StorageHandle::enter(&mut provider, |storage| {
        let (_, _, required) =
            runtime::quote_settlement(&storage, &world.scope, &world.parent, gem_id, STABLE_EUR)
                .unwrap();
        let late = runtime::settle_gem(
            &storage,
            &world.scope,
            &world.parent,
            crate::schema::GemSettlementParams {
                caller: ALICE,
                gem_id,
                asset: STABLE_EUR,
                snapshot_id: required,
            },
        );
        assert_eq!(
            err_msg(late),
            format!(
                "{:?}",
                outbe_primitives::error::PrecompileError::from(
                    crate::errors::GemFactoryError::DeadlineExpired
                )
            )
        );
        assert_eq!(
            gem_view(&storage, &world, gem_id).item.state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn an_issuance_payment_must_name_the_snapshot_required_at_execution() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    let (mut provider, world) = test_storage(Some(usd_rate));
    let (gem_id, amount, quoted) = StorageHandle::enter(&mut provider, |storage| {
        register_currency(&storage, 978, six_decimal_unit());
        seed_day_vwap(&storage, 840, usd_rate);
        let gem_id = issue_at_live_rate(
            &storage,
            &world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        let (_, amount, quoted) =
            runtime::quote_settlement(&storage, &world.scope, &world.parent, gem_id, STABLE_EUR)
                .unwrap();
        (gem_id, amount, quoted)
    });
    let cutoff = outbe_oracle::api::VwapSnapshotId::from_u256(quoted)
        .unwrap()
        .cutoff();

    provider.set_timestamp(U256::from(cutoff + 3_599));
    StorageHandle::enter(&mut provider, |storage| {
        let res = runtime::settle_gem(
            &storage,
            &world.scope,
            &world.parent,
            crate::schema::GemSettlementParams {
                caller: ALICE,
                gem_id,
                asset: STABLE_EUR,
                snapshot_id: quoted,
            },
        );
        assert!(err_msg(res).contains("unexpected amount"));
    });

    provider.set_timestamp(U256::from(cutoff + 3_600));
    StorageHandle::enter(&mut provider, |storage| {
        let (_, next_amount, required) =
            runtime::quote_settlement(&storage, &world.scope, &world.parent, gem_id, STABLE_EUR)
                .unwrap();
        assert_eq!(next_amount, amount, "the next window holds the same price");
        let res = runtime::settle_gem(
            &storage,
            &world.scope,
            &world.parent,
            crate::schema::GemSettlementParams {
                caller: ALICE,
                gem_id,
                asset: STABLE_EUR,
                snapshot_id: quoted,
            },
        );
        assert_eq!(
            err_msg(res),
            format!(
                "{:?}",
                outbe_primitives::error::PrecompileError::from(
                    outbe_vaultrouter::settlement::SettlementError::SnapshotMismatch {
                        authorized: quoted,
                        required,
                    }
                )
            )
        );
        let other_policy = outbe_oracle::api::get_vwap_snapshot_id(
            cutoff + 3_600,
            &outbe_oracle::api::VwapPolicy {
                policy_version: 2,
                ..outbe_oracle::api::DEFAULT_VWAP_POLICY
            },
        )
        .unwrap();
        let res = runtime::settle_gem(
            &storage,
            &world.scope,
            &world.parent,
            crate::schema::GemSettlementParams {
                caller: ALICE,
                gem_id,
                asset: STABLE_EUR,
                snapshot_id: other_policy.to_u256(),
            },
        );
        assert_eq!(
            err_msg(res),
            format!(
                "{:?}",
                outbe_primitives::error::PrecompileError::from(
                    outbe_vaultrouter::settlement::SettlementError::SnapshotMismatch {
                        authorized: other_policy.to_u256(),
                        required,
                    }
                )
            )
        );
        let reference = runtime::settle_gem(
            &storage,
            &world.scope,
            &world.parent,
            crate::schema::GemSettlementParams {
                caller: ALICE,
                gem_id,
                asset: STABLE,
                snapshot_id: quoted,
            },
        );
        assert!(err_msg(reference).contains("unexpected amount"));
        assert_eq!(
            gem_view(&storage, &world, gem_id).item.state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn a_hundred_dollars_converts_to_ninety_euros_at_every_asset_scale() {
    let eur_8 = address!("0x00000000000000000000000000000000000000E8");
    let eur_18 = address!("0x00000000000000000000000000000000000000E9");
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    let (mut provider, world) = test_storage(Some(usd_rate));
    stub_stablecoin(&mut provider, eur_8, 978, 8);
    stub_stablecoin(&mut provider, eur_18, 978, 18);
    StorageHandle::enter(&mut provider, |storage| {
        register_currency(&storage, 978, U256::from(1_800_000u64));
        seed_day_vwap(&storage, 840, usd_rate);
        let load = U256::from(50u64) * six_decimal_unit();
        let gem_id =
            issue_at_live_rate(&storage, &world, ALICE, GemTypes::Wallet, load, 978, 840).unwrap();
        for (asset, expected) in [
            (STABLE_EUR, U256::from(90_000_000u64)),
            (eur_8, U256::from(9_000_000_000u64)),
            (
                eur_18,
                U256::from(90u64) * U256::from(10u64).pow(U256::from(18)),
            ),
        ] {
            let (_, cost, _) =
                runtime::quote_settlement(&storage, &world.scope, &world.parent, gem_id, asset)
                    .unwrap();
            assert_eq!(cost, expected, "{asset}");
        }
    });
}

#[test]
fn an_sra_gem_keeps_its_sixty_four_percent_through_the_currency_conversion() {
    // 100 USD standard basis (entry 2.00 x load 50) at R = 2.00, I = 1.80.
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(usd_rate), |storage, world| {
        register_currency(storage, 978, U256::from(1_800_000u64));
        seed_day_vwap(storage, 840, usd_rate);
        let load = U256::from(50u64) * six_decimal_unit();
        let sra = issue_at_live_rate(storage, world, ALICE, GemTypes::Sra, load, 978, 840).unwrap();
        let wallet =
            issue_at_live_rate(storage, world, BOB, GemTypes::Wallet, load, 978, 840).unwrap();
        let quote = |gem_id| {
            runtime::quote_settlement(storage, &world.scope, &world.parent, gem_id, STABLE_EUR)
                .unwrap()
                .1
        };
        assert_eq!(quote(wallet), U256::from(90_000_000u64));
        assert_eq!(quote(sra), U256::from(57_600_000u64));
        let item = gem_view(storage, world, sra);
        assert_eq!(
            (item.bucket.entry_price_minor, item.item.promis_load_minor),
            (usd_rate, load)
        );
    });
}

#[test]
fn the_issuance_rail_converts_at_the_trailing_window_not_the_closed_day() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(usd_rate), |storage, world| {
        register_currency(storage, 978, six_decimal_unit());
        seed_day_vwap(storage, 840, usd_rate);
        let gem_id = issue_at_live_rate(
            storage,
            world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        let (_, before, _) =
            runtime::quote_settlement(storage, &world.scope, &world.parent, gem_id, STABLE_EUR)
                .unwrap();
        let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 978)
            .unwrap()
            .unwrap();
        OracleContract::new(storage.clone())
            .record_utc_day_vwap(
                previous_date_key(timestamp_to_date_key(T_NOW)),
                index,
                U256::from(4u64) * six_decimal_unit(),
            )
            .unwrap();
        let (_, after, _) =
            runtime::quote_settlement(storage, &world.scope, &world.parent, gem_id, STABLE_EUR)
                .unwrap();
        let (_, reference, _) =
            runtime::quote_settlement(storage, &world.scope, &world.parent, gem_id, STABLE)
                .unwrap();
        assert_eq!(after, before);
        assert_eq!(after, reference / U256::from(2u64));
    });
}

/// The gem body carries the load and the type. It has no entry-price field:
/// adding one fails this destructure.
fn gem_body_cost_inputs(item: &GemItemState) -> (u8, U256) {
    let GemItemState {
        gem_id: _,
        owner: _,
        gem_type,
        promis_load_minor,
        issuance_currency: _,
        reference_currency: _,
        issued_at: _,
        bucket_key: _,
        state: _,
        called_at: _,
        settled_at: _,
    } = item;
    (*gem_type, *promis_load_minor)
}

/// `entry x load x cost_rate / 100`, in six-decimal reference units.
fn six_decimal_settlement_cost(entry: U256, load: U256, gem_type: u8) -> U256 {
    let cost_rate = if gem_type == GemTypes::Sra as u8 {
        outbe_gem::constants::SRA_RATE
    } else {
        100
    };
    entry * load * U256::from(cost_rate) / U256::from(100) / six_decimal_unit()
}

#[test]
fn settlement_cost_reads_entry_price_from_the_bucket_body() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let other_entry = U256::from(3u64) * six_decimal_unit();
    with_storage(Some(rate), |storage, world| {
        let heavy = U256::from(10u64) * six_decimal_unit();
        let light = U256::from(4u64) * six_decimal_unit();
        let heavy_id =
            issue_at_live_rate(storage, world, ALICE, GemTypes::Wallet, heavy, 840, 840).unwrap();
        let light_id =
            issue_at_live_rate(storage, world, BOB, GemTypes::Wallet, light, 840, 840).unwrap();
        // Same load as the heavy gem, so the id needs another owner. A different
        // entry price opens a second bucket. The gem body still has none.
        let other_owner = address!("0x3333333333333333333333333333333333333333");
        let other_id = runtime::issue_gem(
            storage,
            &world.scope,
            &world.parent,
            GemIssueParams {
                owner: other_owner,
                gem_type: GemTypes::Wallet,
                promis_load: heavy,
                issuance_currency: 840,
                reference_currency: 840,
                entry_price: other_entry,
            },
        )
        .unwrap();

        let heavy_view = gem_view(storage, world, heavy_id);
        let light_view = gem_view(storage, world, light_id);
        let other_view = gem_view(storage, world, other_id);
        assert_eq!(heavy_view.item.bucket_key, light_view.item.bucket_key);
        assert_eq!(heavy_view.bucket.entry_price_minor, rate);
        assert_eq!(light_view.bucket.entry_price_minor, rate);
        assert_ne!(other_view.item.bucket_key, heavy_view.item.bucket_key);
        assert_eq!(other_view.bucket.entry_price_minor, other_entry);
        assert_eq!(
            other_view.item.promis_load_minor,
            heavy_view.item.promis_load_minor
        );

        let quoted = |gem_id| {
            runtime::quote_settlement(storage, &world.scope, &world.parent, gem_id, STABLE)
                .unwrap()
                .1
        };
        let expected = |view: &GemView| {
            let (gem_type, load) = gem_body_cost_inputs(&view.item);
            six_decimal_settlement_cost(view.bucket.entry_price_minor, load, gem_type)
        };
        assert_eq!(quoted(heavy_id), expected(&heavy_view));
        assert_eq!(quoted(light_id), expected(&light_view));
        assert_ne!(quoted(heavy_id), quoted(light_id));
        assert_eq!(quoted(other_id), expected(&other_view));
        assert_ne!(quoted(other_id), quoted(heavy_id));
    });
}

#[test]
fn two_merchants_sending_one_series_in_a_block_get_separate_positions() {
    let series = SeriesId::pack(WorldwideDay::new(7), *b"USD", b'U').unwrap();
    let block = 1u64;
    assert_ne!(
        GemFactoryContract::generate_position_id(ALICE, series, block),
        GemFactoryContract::generate_position_id(BOB, series, block),
        "a series has many owners and any of them may send units to the Gem Factory"
    );
}
