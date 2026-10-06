use super::*;

/// The surplus of an over-spend reaches the reserve vault with nothing left to
/// return it, so a note that overpays settles nothing.
#[test]
fn a_paynote_spending_more_than_the_cost_is_refused() {
    let (mut provider, gem_id, proof) = gem_paid_off_the_quote(|cost| cost + U256::ONE);
    StorageHandle::enter(&mut provider, |storage| {
        let res = runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof);
        assert!(err_msg(res).contains("PayNote spends"));
        assert_eq!(
            gem_api::get_gem(&storage, gem_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn a_paynote_spending_less_than_the_cost_is_refused() {
    let (mut provider, gem_id, proof) = gem_paid_off_the_quote(|cost| cost - U256::ONE);
    StorageHandle::enter(&mut provider, |storage| {
        let res = runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof);
        assert!(err_msg(res).contains("PayNote spends"));
        assert_eq!(
            gem_api::get_gem(&storage, gem_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn settlement_event_reports_the_rail_the_asset_matched() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(rate));
    let (gem_id, proof) = note_for_quoted_cost(&mut provider, STABLE, |storage| {
        let gem_id = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            949,
            840,
        )
        .unwrap();
        seed_qualifying_day(storage, gem_id);
        gem_id
    });
    StorageHandle::enter(&mut provider, |storage| {
        runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof).unwrap();
    });

    let event = provider
        .get_ordered_events()
        .iter()
        .filter_map(|log| crate::precompile::IGemFactory::GemSettled::decode_log(log).ok())
        .next()
        .expect("settlement emits GemSettled");
    assert_eq!(event.gemId, gem_id);
    assert_eq!(event.settlementCurrency, 840);
}

/// The stubbed token answers `true` without moving a balance, so the direct path
/// must refuse the payment and leave the gem unsettled.
#[test]
fn erc20_settle_refuses_a_transfer_that_moves_nothing() {
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
        seed_qualifying_day(storage, gem_id);

        let call = crate::precompile::IGemFactory::settleGemCall {
            gemId: gem_id,
            asset: STABLE,
            snapshotId: U256::ZERO,
        };
        let res = crate::precompile::dispatch(storage.clone(), &call.abi_encode(), BOB, U256::ZERO);
        assert!(err_msg(res).contains("unexpected amount"));
        assert_eq!(
            gem_api::get_gem(storage, gem_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn erc20_settle_rejects_a_foreign_currency_asset_before_paying() {
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
        seed_qualifying_day(storage, gem_id);

        let res = runtime::settle_gem(storage, ALICE, gem_id, STABLE_EUR, U256::ZERO);
        assert!(err_msg(res).contains("does not match"));
        assert_eq!(
            gem_api::get_gem(storage, gem_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn the_issuance_currency_settles_through_the_coen_pivot() {
    // COEN/USD 2.0, COEN/EUR 1.0: the same cost converts to half as many EUR units.
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(usd_rate));
    let (gem_id, proof) = note_for_quoted_cost(&mut provider, STABLE_EUR, |storage| {
        register_currency(storage, 978, six_decimal_unit());
        seed_day_vwap(storage, 840, usd_rate);
        let gem_id = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        seed_qualifying_day(storage, gem_id);
        gem_id
    });
    StorageHandle::enter(&mut provider, |storage| {
        // Paying with the EUR asset picks the issuance rail.
        runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof).unwrap();
    });

    let event = settled_event(&provider);
    assert_eq!(event.asset, STABLE_EUR);
    assert_eq!(event.settlementCurrency, 978);
    assert_eq!(event.paymentMinor, U256::from(10u64) * six_decimal_unit());
}

#[test]
fn the_issuance_rail_floors_the_whole_obligation_in_the_payers_favour() {
    // Exact obligation 2.5 EUR units: flooring charges 2, rounding up charged 3.
    let mut provider = test_storage(Some(U256::from(3_000_000u64)));
    let (gem_id, proof) = note_for_quoted_cost(&mut provider, STABLE_EUR, |storage| {
        register_currency(storage, 978, U256::from(2_500_000u64));
        seed_day_vwap(storage, 840, U256::from(3_000_000u64));
        let gem_id =
            issue_at_live_rate(storage, ALICE, GemTypes::Wallet, U256::ONE, 978, 840).unwrap();
        seed_qualifying_day(storage, gem_id);
        gem_id
    });
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            runtime::gem_cost_minor(&gem_api::get_gem(&storage, gem_id).unwrap().unwrap()).unwrap(),
            U256::from(3u64)
        );
        runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof).unwrap();
    });

    assert_eq!(settled_event(&provider).paymentMinor, U256::from(2u64));
}

#[test]
fn a_wider_asset_keeps_what_the_six_decimal_cost_dropped() {
    // The reference cost floors to 1. The obligation is 1.500001. An
    // eighteen-decimal asset carries all of it. Scaling the floored 1 charged 1e12.
    let mut provider = test_storage(Some(U256::from(1_500_001u64)));
    let (gem_id, proof) = note_for_quoted_cost(&mut provider, STABLE_18, |storage| {
        let gem_id =
            issue_at_live_rate(storage, ALICE, GemTypes::Wallet, U256::ONE, 840, 840).unwrap();
        seed_qualifying_day(storage, gem_id);
        gem_id
    });
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            runtime::gem_cost_minor(&gem_api::get_gem(&storage, gem_id).unwrap().unwrap()).unwrap(),
            U256::ONE
        );
        runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof).unwrap();
    });

    assert_eq!(
        settled_event(&provider).paymentMinor,
        U256::from(1_500_001_000_000u64)
    );
}

#[test]
fn the_settlement_minimum_precedes_asset_and_currency_conversion() {
    with_storage(Some(U256::ONE), |storage| {
        let gem_id =
            issue_at_live_rate(storage, ALICE, GemTypes::Wallet, U256::ONE, 840, 840).unwrap();
        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
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
    let mut provider = test_storage(Some(U256::ONE));
    let (gem_id, proof) = note_for_quoted_cost(&mut provider, STABLE, |storage| {
        let gem_id =
            issue_at_live_rate(storage, ALICE, GemTypes::Wallet, U256::ONE, 840, 840).unwrap();
        seed_qualifying_day(storage, gem_id);
        gem_id
    });
    StorageHandle::enter(&mut provider, |storage| {
        runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof).unwrap();
    });

    assert_eq!(settled_event(&provider).paymentMinor, U256::ONE);
}

#[test]
fn the_settlement_minimum_ignores_the_sra_discount() {
    with_storage(Some(U256::ONE), |storage| {
        let gem_id =
            issue_at_live_rate(storage, ALICE, GemTypes::Sra, U256::ONE, 840, 840).unwrap();
        let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();

        assert_eq!(
            runtime::settlement_units(&item, None, 6).unwrap(),
            U256::ONE
        );
    });
}

#[test]
fn settling_on_an_unregistered_issuance_leg_is_refused() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    with_storage_paying(Some(usd_rate), STABLE_EUR, |storage, proof| {
        // The EUR asset is a valid vault asset, but COEN/978 was never registered,
        // so the pivot has no leg to convert through.
        seed_day_vwap(storage, 840, usd_rate);
        let gem_id = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        seed_qualifying_day(storage, gem_id);
        let res = runtime::settle_gem_with_paynote(storage, ALICE, gem_id, proof);
        assert!(err_msg(res).contains("oracle nominal unavailable"));
        assert_eq!(
            gem_api::get_gem(storage, gem_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn the_reference_currency_settles_without_reading_any_issuance_rate() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(usd_rate));
    let (gem_id, proof) = note_for_quoted_cost(&mut provider, STABLE, |storage| {
        // Issuance 978 is never registered, so it carries no rate at all.
        let gem_id = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        seed_qualifying_day(storage, gem_id);
        gem_id
    });
    StorageHandle::enter(&mut provider, |storage| {
        runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof).unwrap();
    });

    let event = settled_event(&provider);
    assert_eq!(event.settlementCurrency, 840);
    assert_eq!(event.paymentMinor, U256::from(20u64) * six_decimal_unit());
}

#[test]
fn settle_rejects_an_asset_with_no_registered_vault() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(rate));
    let proof = note_proof(
        &mut provider,
        STABLE,
        unbound_context(),
        U256::from(NOTE_AMOUNT),
    );
    // Override the blanket vault count: this asset has none.
    provider.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall::SELECTOR,
        word(0),
    );
    StorageHandle::enter(&mut provider, |storage| {
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
        let res = runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof);
        assert!(err_msg(res).contains("no registered vault"));
    });
}

#[test]
fn settlement_scales_the_cost_to_the_asset_decimals() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(usd_rate));
    let (gem_id, proof) = note_for_quoted_cost(&mut provider, STABLE_18, |storage| {
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
        // An eighteen-decimal asset was a hard revert before. Now it scales.
        runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof).unwrap();
    });

    let event = settled_event(&provider);
    assert_eq!(
        event.paymentMinor,
        U256::from(20u64) * six_decimal_unit() * U256::from(1_000_000_000_000u64)
    );
}

#[test]
fn an_unassigned_issuance_code_mints_and_settles_on_the_reference_rail() {
    // 899 is inside the three-digit range but is not an assigned ISO 4217 code.
    // Gem no longer refuses it. Nothing prices against it, and no settlement
    // asset can ever report it. So it is inert, exactly as it is for a bid.
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(rate));
    let (gem_id, proof) = note_for_quoted_cost(&mut provider, STABLE, |storage| {
        let gem_id = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            899,
            840,
        )
        .unwrap();
        seed_qualifying_day(storage, gem_id);
        gem_id
    });
    StorageHandle::enter(&mut provider, |storage| {
        runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof).unwrap();
    });

    let event = settled_event(&provider);
    assert_eq!(event.settlementCurrency, 840);
    assert_eq!(event.paymentMinor, U256::from(20u64) * six_decimal_unit());
}

#[test]
fn issue_rejects_an_issuance_code_outside_the_three_digit_range() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage| {
        for iso in [0u16, 1000u16] {
            let res = issue_at_live_rate(
                storage,
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
    with_storage(Some(rate), |storage| {
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
fn the_quote_agrees_with_what_settling_charges_on_both_rails() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(usd_rate));
    let (gem_id, proof) = note_for_quoted_cost(&mut provider, STABLE_EUR, |storage| {
        register_currency(storage, 978, six_decimal_unit());
        seed_day_vwap(storage, 840, usd_rate);
        let gem_id = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        seed_qualifying_day(storage, gem_id);
        gem_id
    });
    let quoted = StorageHandle::enter(&mut provider, |storage| {
        // Both rails quote, and neither quote moves anything.
        let (ref_iso, ref_amount, _) = runtime::quote_settlement(&storage, gem_id, STABLE).unwrap();
        let (iss_iso, iss_amount, _) =
            runtime::quote_settlement(&storage, gem_id, STABLE_EUR).unwrap();
        assert_eq!(ref_iso, 840);
        assert_eq!(iss_iso, 978);
        assert_eq!(iss_amount, ref_amount / U256::from(2u64));

        runtime::settle_gem_with_paynote(&storage, ALICE, gem_id, &proof).unwrap();
        iss_amount
    });

    assert_eq!(settled_event(&provider).paymentMinor, quoted);
}

#[test]
fn an_issuance_payment_must_name_the_snapshot_required_at_execution() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(usd_rate));
    let (gem_id, amount, quoted) = StorageHandle::enter(&mut provider, |storage| {
        register_currency(&storage, 978, six_decimal_unit());
        seed_day_vwap(&storage, 840, usd_rate);
        let gem_id = issue_at_live_rate(
            &storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        seed_qualifying_day(&storage, gem_id);
        let (_, amount, quoted) = runtime::quote_settlement(&storage, gem_id, STABLE_EUR).unwrap();
        (gem_id, amount, quoted)
    });
    let cutoff = outbe_oracle::api::VwapSnapshotId::from_u256(quoted)
        .unwrap()
        .cutoff();

    provider.set_timestamp(U256::from(cutoff + 3_599));
    StorageHandle::enter(&mut provider, |storage| {
        let res = runtime::settle_gem(&storage, ALICE, gem_id, STABLE_EUR, quoted);
        assert!(err_msg(res).contains("unexpected amount"));
    });

    provider.set_timestamp(U256::from(cutoff + 3_600));
    StorageHandle::enter(&mut provider, |storage| {
        let (_, next_amount, required) =
            runtime::quote_settlement(&storage, gem_id, STABLE_EUR).unwrap();
        assert_eq!(next_amount, amount, "the next window holds the same price");
        let res = runtime::settle_gem(&storage, ALICE, gem_id, STABLE_EUR, quoted);
        assert_eq!(
            err_msg(res),
            format!(
                "{:?}",
                outbe_primitives::error::PrecompileError::from(
                    crate::errors::GemFactoryError::VwapSnapshotMismatch {
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
        let res = runtime::settle_gem(&storage, ALICE, gem_id, STABLE_EUR, other_policy.to_u256());
        assert_eq!(
            err_msg(res),
            format!(
                "{:?}",
                outbe_primitives::error::PrecompileError::from(
                    crate::errors::GemFactoryError::VwapSnapshotMismatch {
                        authorized: other_policy.to_u256(),
                        required,
                    }
                )
            )
        );
        let reference = runtime::settle_gem(&storage, ALICE, gem_id, STABLE, quoted);
        assert!(err_msg(reference).contains("unexpected amount"));
        assert_eq!(
            gem_api::get_gem(&storage, gem_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}

#[test]
fn a_paynote_bound_to_an_earlier_snapshot_cannot_settle_after_rollover() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(usd_rate));
    let quote = |provider: &mut HashMapStorageProvider, gem_id| {
        StorageHandle::enter(provider, |storage| {
            runtime::quote_settlement(&storage, gem_id, STABLE_EUR).unwrap()
        })
    };
    let gem_id = StorageHandle::enter(&mut provider, |storage| {
        register_currency(&storage, 978, six_decimal_unit());
        seed_day_vwap(&storage, 840, usd_rate);
        let gem_id = issue_at_live_rate(
            &storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        seed_qualifying_day(&storage, gem_id);
        gem_id
    });
    let (_, amount, quoted) = quote(&mut provider, gem_id);
    let stale = outbe_paynote::test_support::note_and_spend_proof(
        1,
        STABLE_EUR,
        gem_context(gem_id, quoted),
        amount,
        amount,
    );
    outbe_paynote::test_support::seed_pool(&mut provider, 1, &[stale.commitment]);

    let cutoff = outbe_oracle::api::VwapSnapshotId::from_u256(quoted)
        .unwrap()
        .cutoff();
    provider.set_timestamp(U256::from(cutoff + 3_600));
    let (_, next_amount, required) = quote(&mut provider, gem_id);
    assert_eq!(next_amount, amount, "the next window holds the same price");
    let before = provider.storage.clone();
    let rejected = StorageHandle::enter(&mut provider, |storage| {
        runtime::settle_gem_with_paynote(&storage, BOB, gem_id, &stale.proof)
    });
    assert_eq!(
        err_msg(rejected),
        format!(
            "{:?}",
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GemFactoryError::PayNoteContextMismatch {
                    expected: gem_context(gem_id, required),
                    actual: gem_context(gem_id, quoted),
                }
            )
        )
    );
    assert_eq!(
        provider.storage, before,
        "the note and the gem are untouched"
    );

    let current = outbe_paynote::test_support::note_and_spend_proof(
        1,
        STABLE_EUR,
        gem_context(gem_id, required),
        amount,
        amount,
    );
    StorageHandle::enter(&mut provider, |storage| {
        runtime::settle_gem_with_paynote(&storage, BOB, gem_id, &current.proof).unwrap();
    });
    assert_eq!(settled_event(&provider).paymentMinor, amount);
}

#[test]
fn a_hundred_dollars_converts_to_ninety_euros_at_every_asset_scale() {
    let eur_8 = address!("0x00000000000000000000000000000000000000E8");
    let eur_18 = address!("0x00000000000000000000000000000000000000E9");
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(usd_rate));
    stub_stablecoin(&mut provider, eur_8, 978, 8);
    stub_stablecoin(&mut provider, eur_18, 978, 18);
    StorageHandle::enter(&mut provider, |storage| {
        register_currency(&storage, 978, U256::from(1_800_000u64));
        seed_day_vwap(&storage, 840, usd_rate);
        let load = U256::from(50u64) * six_decimal_unit();
        let gem_id = issue_at_live_rate(&storage, ALICE, GemTypes::Wallet, load, 978, 840).unwrap();
        for (asset, expected) in [
            (STABLE_EUR, U256::from(90_000_000u64)),
            (eur_8, U256::from(9_000_000_000u64)),
            (
                eur_18,
                U256::from(90u64) * U256::from(10u64).pow(U256::from(18)),
            ),
        ] {
            let (_, cost, _) = runtime::quote_settlement(&storage, gem_id, asset).unwrap();
            assert_eq!(cost, expected, "{asset}");
        }
    });
}

#[test]
fn an_sra_gem_keeps_its_sixty_four_percent_through_the_currency_conversion() {
    // 100 USD standard basis (entry 2.00 x load 50) at R = 2.00, I = 1.80.
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(usd_rate), |storage| {
        register_currency(storage, 978, U256::from(1_800_000u64));
        seed_day_vwap(storage, 840, usd_rate);
        let load = U256::from(50u64) * six_decimal_unit();
        let sra = issue_at_live_rate(storage, ALICE, GemTypes::Sra, load, 978, 840).unwrap();
        let wallet = issue_at_live_rate(storage, BOB, GemTypes::Wallet, load, 978, 840).unwrap();
        let quote = |gem_id| {
            runtime::quote_settlement(storage, gem_id, STABLE_EUR)
                .unwrap()
                .1
        };
        assert_eq!(quote(wallet), U256::from(90_000_000u64));
        assert_eq!(quote(sra), U256::from(57_600_000u64));
        let item = gem_api::get_gem(storage, sra).unwrap().unwrap();
        assert_eq!(
            (item.entry_price_minor, item.promis_load_minor),
            (usd_rate, load)
        );
    });
}

#[test]
fn the_issuance_rail_converts_at_the_trailing_window_not_the_closed_day() {
    let usd_rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(usd_rate), |storage| {
        register_currency(storage, 978, six_decimal_unit());
        seed_day_vwap(storage, 840, usd_rate);
        let gem_id = issue_at_live_rate(
            storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            978,
            840,
        )
        .unwrap();
        let (_, before, _) = runtime::quote_settlement(storage, gem_id, STABLE_EUR).unwrap();
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
        let (_, after, _) = runtime::quote_settlement(storage, gem_id, STABLE_EUR).unwrap();
        let (_, reference, _) = runtime::quote_settlement(storage, gem_id, STABLE).unwrap();
        assert_eq!(after, before);
        assert_eq!(after, reference / U256::from(2u64));
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
