use super::*;

fn entry_price() -> U256 {
    U256::from(500_000u64)
}

fn load_minor() -> U256 {
    U256::from(100_000u64) * U256::from(1_000_000u64)
}

fn product() -> U256 {
    entry_price() * load_minor()
}

fn one_unit(decimals: u8) -> U256 {
    runtime::settlement_units(product(), U256::ONE, None, decimals).unwrap()
}

#[test]
fn cost_amount_six_decimals() {
    assert_eq!(one_unit(6), U256::from(50_000_000_000u64));
}

#[test]
fn cost_amount_eighteen_decimals_is_1e12_larger() {
    assert_eq!(
        one_unit(18),
        one_unit(6) * U256::from(10u64).pow(U256::from(12u64))
    );
}

#[test]
fn cost_amount_zero_decimals() {
    assert_eq!(one_unit(0), U256::from(50_000u64));
}

#[test]
fn cost_amount_twelve_decimals() {
    assert_eq!(one_unit(12), U256::from(50_000_000_000_000_000u64));
}

#[test]
fn a_minimum_the_payment_asset_cannot_express_is_refused() {
    let err = runtime::settlement_units(U256::ONE, U256::ONE, None, 0).unwrap_err();
    assert!(err.to_string().contains("rounds to zero"), "{err}");
}

#[test]
fn a_subunit_purchase_charges_one_minor_unit_once() {
    for (amount, expected) in [(1u64, 1u64), (3, 1), (1_000, 1), (2_500_000, 2)] {
        assert_eq!(
            runtime::settlement_units(U256::ONE, U256::from(amount), None, 6).unwrap(),
            U256::from(expected)
        );
    }
}

#[test]
fn the_unit_minimum_precedes_asset_and_currency_conversion() {
    let fx = |to: u64, from: u64| Some((U256::from(to), U256::from(from)));
    for (rate, decimals, expected) in [
        (None, 8, 100u64),
        (None, 18, 1_000_000_000_000),
        (fx(2, 1), 6, 2),
        (fx(1, 2), 18, 500_000_000_000),
    ] {
        assert_eq!(
            runtime::settlement_units(U256::ONE, U256::ONE, rate, decimals).unwrap(),
            U256::from(expected)
        );
    }
    let err = runtime::settlement_units(U256::ONE, U256::ONE, fx(1, 2), 6).unwrap_err();
    assert!(err.to_string().contains("rounds to zero"), "{err}");
}

#[test]
fn a_priceless_series_keeps_its_zero_cost() {
    assert_eq!(
        runtime::settlement_units(U256::ZERO, U256::from(1_000u64), None, 6).unwrap(),
        U256::ZERO
    );
    assert_eq!(
        runtime::settlement_units(product(), U256::ZERO, None, 6).unwrap(),
        U256::ZERO
    );
}

#[test]
fn the_selected_units_are_floored_once_not_one_by_one() {
    // 1.5 payment units each: three units cost 4.5, floored once to 4, where
    // flooring every unit to 1 first would have charged 3.
    let product = U256::from(3u64) * U256::from(500_000_000_000u64);
    assert_eq!(
        runtime::settlement_units(product, U256::from(3u64), None, 0).unwrap(),
        U256::from(4u64)
    );
}

#[test]
fn the_fx_leg_is_floored_together_with_the_units() {
    // One reference unit at COEN/target 1 and COEN/reference 3: a third, floored.
    let rate = Some((U256::from(1_000_000u64), U256::from(3_000_000u64)));
    assert_eq!(
        runtime::settlement_units(U256::from(1_000_000_000_000u64), U256::ONE, rate, 6).unwrap(),
        U256::from(333_333u64)
    );
}

#[test]
fn a_hundred_dollars_converts_to_ninety_euros_at_every_asset_scale() {
    // 100 USD = entry 2.00 x load 50; R = 2.00 USD/COEN, I = 1.80 EUR/COEN.
    let product = U256::from(2_000_000u64) * U256::from(50_000_000u64);
    let rate = Some((U256::from(1_800_000u64), U256::from(2_000_000u64)));
    for (decimals, expected) in [
        (6, U256::from(90_000_000u64)),
        (8, U256::from(9_000_000_000u64)),
        (
            18,
            U256::from(90u64) * U256::from(10u64).pow(U256::from(18u64)),
        ),
    ] {
        assert_eq!(
            runtime::settlement_units(product, U256::ONE, rate, decimals).unwrap(),
            expected,
            "{decimals} decimals"
        );
    }
}

#[test]
fn three_units_convert_as_one_operation_before_the_floor() {
    // One dollar a unit at I/R = 1/3: 1,000,000 for all three, not 3 x 333,333.
    let rate = Some((U256::from(1_000_000u64), U256::from(3_000_000u64)));
    assert_eq!(
        runtime::settlement_units(U256::from(1_000_000_000_000u64), U256::from(3u64), rate, 6)
            .unwrap(),
        U256::from(1_000_000u64)
    );
}

#[test]
fn cost_amount_rejects_unsupported_payment_decimals() {
    let err = runtime::settlement_units(product(), U256::ONE, None, 19).unwrap_err();
    assert!(err.to_string().contains("unsupported decimals"), "{err}");
}

#[test]
fn cost_amount_rejects_scaling_overflow() {
    let err = runtime::settlement_units(U256::MAX, U256::ONE, None, 18).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("overflow"), "{err}");
}

/// Storage with an issued series 7 and a payment token the router reports
/// `vaults` vaults for, reporting `iso` and `decimals`.
fn with_payment_token<R>(
    vaults: u64,
    iso: u64,
    decimals: u64,
    f: impl FnOnce(StorageHandle) -> R,
) -> R {
    use crate::sol_ext::{IReferenceCurrency, IERC20};
    use outbe_vaultrouter::api::IVaultRouter;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(crate::constants::INTEX_NFT1155_ADDRESS, word(0));
    storage.stub_sub_call_at(crate::constants::ORIGIN_ROUTER_ADDRESS, word(0));
    storage.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall::SELECTOR,
        word(vaults),
    );
    storage.stub_sub_call_at_selector(
        payment_token(),
        IReferenceCurrency::isoCodeCall::SELECTOR,
        word(iso),
    );
    storage.stub_sub_call_at_selector(
        payment_token(),
        IERC20::decimalsCall::SELECTOR,
        word(decimals),
    );

    StorageHandle::enter(&mut storage, |s| {
        runtime::issue(&s, sample(7)).unwrap();
        f(s)
    })
}

#[test]
fn settlement_quote_prices_an_accepted_token() {
    for (decimals, expected) in [
        (0, U256::ONE),
        (6, U256::from(1_000_000u64)),
        (12, U256::from(1_000_000_000_000u64)),
        (18, U256::from(1_000_000_000_000_000_000u64)),
    ] {
        with_payment_token(1, 840, decimals, |s| {
            let (_, cost, _) =
                runtime::quote_settlement(&s, sid(7), payment_token(), U256::ONE).unwrap();
            assert_eq!(cost, expected, "payment token decimals {decimals}");
        });
    }
}

#[test]
fn settlement_quote_rejects_an_unregistered_token() {
    with_payment_token(0, 840, 18, |s| {
        let err = runtime::quote_settlement(&s, sid(7), payment_token(), U256::ONE).unwrap_err();
        assert!(err.to_string().contains("no registered vault"), "{err}");
    });
}

#[test]
fn settlement_quote_rejects_a_foreign_currency() {
    with_payment_token(1, 978, 18, |s| {
        let err = runtime::quote_settlement(&s, sid(7), payment_token(), U256::ONE).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
    });
}

#[test]
fn settlement_quote_rejects_missing_series() {
    with_factory(|s| {
        assert!(runtime::quote_settlement(&s, sid(7), payment_token(), U256::ONE).is_err());
    });
}

#[test]
fn settlement_quote_dispatch() {
    with_payment_token(1, 840, 18, |s| {
        let out = precompile::dispatch(
            s.clone(),
            &IIntexFactory::quoteSettlementCall {
                seriesId: sid(7).into(),
                paymentToken: payment_token(),
                amount: U256::from(2u64),
            }
            .abi_encode(),
            owner(),
            U256::ZERO,
        )
        .unwrap();
        let ret = IIntexFactory::quoteSettlementCall::abi_decode_returns(&out).unwrap();
        assert_eq!(ret.settlementCurrency, 840);
        assert_eq!(ret.payableUnits, U256::from(2_000_000_000_000_000_000u64));
    });
}

/// Two units of `sample(7)` cost this at six decimals.
const TWO_UNIT_COST: U256 = U256::from_limbs([2_000_000, 0, 0, 0]);

fn intex_context(series: SeriesId, holder: Address, units: U256) -> B256 {
    outbe_paynote::api::settlement_context(
        outbe_paynote::api::SettlementDomain::Intex,
        outbe_paynote::api::intex_holding_target(series.as_bytes(), holder),
        units,
        U256::ZERO,
    )
    .unwrap()
}

/// Series 7 qualified with two units on its owner, settled by a stranger whose
/// note spends `spend` and is bound to those two units.
fn settle_two_units_spending(
    spend: U256,
) -> (
    HashMapStorageProvider,
    outbe_primitives::error::Result<U256>,
) {
    settle_two_units_spending_from(spend, false)
}

fn settle_two_units_spending_from(
    spend: U256,
    merged: bool,
) -> (
    HashMapStorageProvider,
    outbe_primitives::error::Result<U256>,
) {
    let (storage, outcome, _) = settle_bound(
        spend,
        merged,
        sid(7),
        owner(),
        U256::from(2u64),
        U256::from(2u64),
    );
    (storage, outcome)
}

fn settle_bound(
    spend: U256,
    merged: bool,
    bound_series: SeriesId,
    bound_holder: Address,
    bound_units: U256,
    settle_units: U256,
) -> (
    HashMapStorageProvider,
    outbe_primitives::error::Result<U256>,
    B256,
) {
    use crate::sol_ext::{IReferenceCurrency, IERC1155, IERC20};
    use outbe_vaultrouter::api::IVaultRouter;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(crate::constants::INTEX_NFT1155_ADDRESS, word(0));
    storage.stub_sub_call_at_selector(
        crate::constants::INTEX_NFT1155_ADDRESS,
        IERC1155::balanceOfCall::SELECTOR,
        word(2),
    );
    storage.stub_sub_call_at(crate::constants::ORIGIN_ROUTER_ADDRESS, word(0));
    storage.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall::SELECTOR,
        word(1),
    );
    storage.stub_sub_call_at_selector(
        payment_token(),
        IReferenceCurrency::isoCodeCall::SELECTOR,
        word(840),
    );
    storage.stub_sub_call_at_selector(payment_token(), IERC20::decimalsCall::SELECTOR, word(6));

    let context = intex_context(bound_series, bound_holder, bound_units);
    let (proof, nullifier) = if merged {
        let (proof, nullifier) = outbe_paynote::test_support::merged_note_spend_proof(
            &mut storage,
            CHAIN_ID,
            payment_token(),
            context,
            spend,
        );
        (
            proof,
            outbe_protocol::codec::field_to_b256(&nullifier).unwrap(),
        )
    } else {
        let fixture = outbe_paynote::test_support::note_and_spend_proof(
            CHAIN_ID,
            payment_token(),
            context,
            spend,
            spend,
        );
        outbe_paynote::test_support::seed_pool(&mut storage, CHAIN_ID, &[fixture.commitment]);
        (
            fixture.proof,
            outbe_protocol::codec::field_to_b256(&fixture.public.nullifier).unwrap(),
        )
    };

    let outcome = StorageHandle::enter(&mut storage, |s| {
        runtime::issue(&s, sample(7)).unwrap();
        seed_qualifying_day(&s);
        runtime::settle_intex_with_paynote(&s, sid(7), owner(), PAYER, settle_units, &proof)
            .map(|_| U256::from(outbe_intex::api::settled_units(&s, sid(7)).unwrap()))
    });
    (storage, outcome, nullifier)
}

const PAYER: Address = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");

/// A settle from a stranger must succeed and book the units to the owner.
#[test]
fn anyone_may_settle_and_the_units_stay_with_the_owner() {
    use alloy_sol_types::SolEvent;

    let (storage, outcome) = settle_two_units_spending(TWO_UNIT_COST);
    assert_eq!(
        outcome.unwrap(),
        U256::from(2u64),
        "the units are booked settled"
    );

    let sig = IIntexFactory::Settled::SIGNATURE_HASH;
    let settled: Vec<_> = storage
        .get_events(INTEX_FACTORY_ADDRESS)
        .iter()
        .filter(|log| log.topics().first() == Some(&sig))
        .map(|log| IIntexFactory::Settled::decode_log_data(log).unwrap())
        .collect();
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0].intexOwner, owner(), "the payer keeps nothing");
    assert_eq!(settled[0].amount, U256::from(2u64));
}

#[test]
fn a_proof_bound_to_two_units_cannot_settle_one() {
    let (mut storage, outcome, nullifier) = settle_bound(
        TWO_UNIT_COST,
        false,
        sid(7),
        owner(),
        U256::from(2u64),
        U256::from(1u64),
    );
    let error = outcome.unwrap_err().to_string();
    assert!(error.contains("does not match settlement"), "{error}");
    StorageHandle::enter(&mut storage, |s| {
        assert_eq!(outbe_intex::api::settled_units(&s, sid(7)).unwrap(), 0);
        assert!(!outbe_paynote::api::is_spent(&s, nullifier).unwrap());
    });
}

#[test]
fn a_proof_bound_to_another_series_cannot_settle_this_one() {
    let (mut storage, outcome, nullifier) = settle_bound(
        TWO_UNIT_COST,
        false,
        sid(8),
        owner(),
        U256::from(2u64),
        U256::from(2u64),
    );
    let error = outcome.unwrap_err().to_string();
    assert!(error.contains("does not match settlement"), "{error}");
    StorageHandle::enter(&mut storage, |s| {
        assert_eq!(outbe_intex::api::settled_units(&s, sid(7)).unwrap(), 0);
        assert!(!outbe_paynote::api::is_spent(&s, nullifier).unwrap());
    });
}

/// Another holder of the same series cannot spend a note bound to someone else's units.
#[test]
fn a_proof_bound_to_one_holder_cannot_settle_another() {
    let (mut storage, outcome, nullifier) = settle_bound(
        TWO_UNIT_COST,
        false,
        sid(7),
        PAYER,
        U256::from(2u64),
        U256::from(2u64),
    );
    let error = outcome.unwrap_err().to_string();
    assert!(error.contains("does not match settlement"), "{error}");
    StorageHandle::enter(&mut storage, |s| {
        assert_eq!(outbe_intex::api::settled_units(&s, sid(7)).unwrap(), 0);
        assert!(!outbe_paynote::api::is_spent(&s, nullifier).unwrap());
    });
}

/// The surplus of an over-spend reaches the reserve vault with nothing left to
/// return it, so settlement takes the cost or nothing.
#[test]
fn a_paynote_that_does_not_spend_the_cost_settles_nothing() {
    for spend in [TWO_UNIT_COST + U256::ONE, TWO_UNIT_COST - U256::ONE] {
        let (mut storage, outcome) = settle_two_units_spending(spend);
        let error = outcome.unwrap_err().to_string();
        assert!(error.contains("PayNote spends"), "{error}");
        StorageHandle::enter(&mut storage, |s| {
            assert_eq!(outbe_intex::api::settled_units(&s, sid(7)).unwrap(), 0);
        });
    }
}

/// A qualified series 7 whose owner holds two units, and a registered six-decimal
/// payment token whose transfers answer `transfer_ret`.
fn with_erc20_series<R>(
    transfer_ret: alloy_primitives::Bytes,
    f: impl FnOnce(StorageHandle) -> R,
) -> R {
    use crate::sol_ext::{IReferenceCurrency, IERC1155, IERC20};
    use outbe_vaultrouter::api::IVaultRouter;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(crate::constants::INTEX_NFT1155_ADDRESS, word(0));
    storage.stub_sub_call_at_selector(
        crate::constants::INTEX_NFT1155_ADDRESS,
        IERC1155::balanceOfCall::SELECTOR,
        word(2),
    );
    storage.stub_sub_call_at(crate::constants::ORIGIN_ROUTER_ADDRESS, word(0));
    storage.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall::SELECTOR,
        word(1),
    );
    storage.stub_sub_call_at(payment_token(), transfer_ret);
    storage.stub_sub_call_at_selector(
        payment_token(),
        IReferenceCurrency::isoCodeCall::SELECTOR,
        word(840),
    );
    storage.stub_sub_call_at_selector(payment_token(), IERC20::decimalsCall::SELECTOR, word(6));
    storage.stub_sub_call_at_selector(payment_token(), IERC20::balanceOfCall::SELECTOR, word(0));

    StorageHandle::enter(&mut storage, |s| {
        runtime::issue(&s, sample(7)).unwrap();
        seed_qualifying_day(&s);
        f(s)
    })
}

#[test]
fn erc20_settle_refuses_a_transfer_that_moves_nothing_and_books_no_units() {
    with_erc20_series(word(1), |s| {
        let err = runtime::settle_intex(
            &s,
            sid(7),
            owner(),
            owner(),
            U256::from(2),
            payment_token(),
            U256::ZERO,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unexpected amount"), "{err}");
        assert_eq!(outbe_intex::api::settled_units(&s, sid(7)).unwrap(), 0);
    });
}

#[test]
fn erc20_settle_refuses_a_token_answering_false() {
    with_erc20_series(word(0), |s| {
        let err = runtime::settle_intex(
            &s,
            sid(7),
            owner(),
            owner(),
            U256::from(2),
            payment_token(),
            U256::ZERO,
        )
        .unwrap_err();
        assert!(err.to_string().contains("token call failed"), "{err}");
        assert_eq!(outbe_intex::api::settled_units(&s, sid(7)).unwrap(), 0);
    });
}

#[test]
fn erc20_settle_rejects_an_unaccepted_asset_before_any_transfer() {
    with_erc20_series(word(1), |s| {
        let foreign = address!("0xDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD");
        assert!(runtime::settle_intex(
            &s,
            sid(7),
            owner(),
            owner(),
            U256::from(1),
            foreign,
            U256::ZERO,
        )
        .is_err());
        assert_eq!(outbe_intex::api::settled_units(&s, sid(7)).unwrap(), 0);
    });
}

#[test]
fn only_the_paynote_settle_pays_for_proof_verification() {
    use outbe_primitives::storage::gas::{PRECOMPILE_BASE_GAS, ZK_VERIFY_GAS};

    let erc20 = IIntexFactory::settleIntexCall {
        seriesId: sid(7).into(),
        intexOwner: owner(),
        amount: U256::ONE,
        asset: payment_token(),
        snapshotId: U256::ZERO,
    }
    .abi_encode();
    let paynote = IIntexFactory::settleIntexWithPayNoteCall {
        seriesId: sid(7).into(),
        intexOwner: owner(),
        amount: U256::ONE,
        payNoteProof: Default::default(),
    }
    .abi_encode();
    assert_eq!(precompile::base_gas(&erc20), PRECOMPILE_BASE_GAS);
    assert_eq!(precompile::base_gas(&paynote), ZK_VERIFY_GAS);
}

// ---------------------------------------------------------------------
// settle gating (value movement is localnet-exercised, not unit tested)
// ---------------------------------------------------------------------

#[test]
fn settle_rejects_zero_amount() {
    with_factory(|s| {
        assert!(
            runtime::settle_intex_with_paynote(&s, sid(7), owner(), owner(), U256::ZERO, &[])
                .is_err()
        );
    });
}

#[test]
fn settle_rejects_missing_series() {
    with_factory(|s| {
        assert!(runtime::settle_intex_with_paynote(
            &s,
            sid(7),
            owner(),
            owner(),
            U256::from(1),
            &[]
        )
        .is_err());
    });
}

#[test]
fn settle_rejects_an_unqualified_series() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        // The only closed day ends at the floor, which does not qualify.
        write_day_vwap(
            &OracleContract::new(s.clone()),
            REFERENCE_ISO,
            PAIR_ID,
            ISSUED_AT as u64 + 2 * DAY,
            U256::from(EXPECTED_FLOOR),
        );
        let err =
            runtime::settle_intex_with_paynote(&s, sid(7), owner(), owner(), U256::from(1), &[])
                .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("settleable"));
    });
}

#[test]
fn settle_rejects_expired_deadline() {
    // Late block timestamp so the Called deadline is already in the past.
    let now = (ISSUED_AT as u64) + (CALL_NOTICE_PERIOD as u64) + 1_000;
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(now));
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    // Stub OriginRouter: send* calls return bytes32 sendId (32 bytes); the value is ignored.
    storage.stub_sub_call_at(
        crate::constants::ORIGIN_ROUTER_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, |s| {
        runtime::issue(&s, sample(7)).unwrap();
        // deadline = ISSUED_AT + CALL_NOTICE_PERIOD < now
        outbe_intex::api::mark_called(&s, sid(7), ISSUED_AT).unwrap();
        let err =
            runtime::settle_intex_with_paynote(&s, sid(7), owner(), owner(), U256::from(1), &[])
                .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("deadline"));
    });
}

#[test]
fn a_qualified_series_called_past_its_deadline_stays_closed() {
    let mut storage = factory_provider();
    StorageHandle::enter(&mut storage, |s| {
        runtime::issue(&s, sample(7)).unwrap();
        seed_qualifying_day(&s);
        outbe_intex::api::mark_called(&s, sid(7), ISSUED_AT).unwrap();
    });
    storage.set_timestamp(U256::from(
        (ISSUED_AT as u64) + (CALL_NOTICE_PERIOD as u64) + 1_000,
    ));
    StorageHandle::enter(&mut storage, |s| {
        let series = outbe_intex::api::read_series(&s, sid(7)).unwrap();
        assert!(runtime::is_qualified(&s, &series).unwrap());
        let err =
            runtime::settle_intex_with_paynote(&s, sid(7), owner(), owner(), U256::from(1), &[])
                .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("deadline"));
    });
}

#[test]
fn settled_token_id_tags_the_series_id() {
    let series_id = sid(7);
    let issued = U256::from_be_slice(series_id.as_bytes());
    let settled = runtime::settled_token_id(series_id);
    let tag: U256 = U256::from(1u8) << 112;

    // Solidity derives the same value; the tag sits above the 14-byte series-id space, so the two
    // id classes cannot collide and clearing it recovers the series.
    assert!(issued < tag);
    assert_eq!(settled, issued | tag);
    assert_eq!(settled & !tag, issued);
}

#[test]
fn compute_pow_hash_matches_manual_sha256() {
    // SHA256(owner ++ promisAmount_be32 ++ seriesId ++ seq_be4 ++ nonce_be8)
    let promis_amount = U256::from(1_000u64);
    let (series_id, seq, nonce) = (sid(7), 3u32, 42u64);
    let got = runtime::compute_pow_hash(owner(), promis_amount, series_id, seq, nonce);

    let mut data = owner().as_slice().to_vec();
    data.extend_from_slice(&promis_amount.to_be_bytes::<32>());
    data.extend_from_slice(series_id.as_bytes());
    data.extend_from_slice(&seq.to_be_bytes());
    data.extend_from_slice(&nonce.to_be_bytes());
    let expected = ring::digest::digest(&ring::digest::SHA256, &data);
    assert_eq!(got.as_slice(), expected.as_ref());
}

/// The vectors the MCP miner is tested against, computed outside both implementations.
#[test]
fn compute_pow_hash_matches_the_shared_client_vectors() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../mcp/src/intex/pow.vectors.json"
    ))
    .unwrap();
    assert_eq!(
        fixture["difficulty_bytes"].as_u64(),
        Some(outbe_common::pow::POW_DIFFICULTY as u64)
    );
    let vectors = fixture["vectors"].as_array().unwrap();
    assert!(!vectors.is_empty());
    for vector in vectors {
        let field = |name: &str| vector[name].as_str().unwrap();
        let owner: Address = field("owner").parse().unwrap();
        let promis_amount: U256 = field("promis_amount").parse().unwrap();
        let series_id = SeriesId::from(
            field("series_id")
                .parse::<alloy_primitives::FixedBytes<14>>()
                .unwrap(),
        );
        let seq = u32::try_from(vector["seq"].as_u64().unwrap()).unwrap();
        let hash = |nonce: u64| {
            B256::from(runtime::compute_pow_hash(
                owner,
                promis_amount,
                series_id,
                seq,
                nonce,
            ))
        };

        let nonce: u64 = field("nonce").parse().unwrap();
        assert_eq!(hash(nonce), field("hash").parse::<B256>().unwrap());

        let first_valid: u64 = field("first_valid_nonce").parse().unwrap();
        assert_eq!(
            hash(first_valid),
            field("first_valid_hash").parse::<B256>().unwrap()
        );
        assert!(runtime::validate_pow(owner, promis_amount, series_id, seq, first_valid).is_ok());
        assert!((0..first_valid).all(|nonce| runtime::validate_pow(
            owner,
            promis_amount,
            series_id,
            seq,
            nonce
        )
        .is_err()));
    }
}

#[test]
fn validate_pow_accepts_valid_and_rejects_invalid_nonce() {
    let pa = U256::from(1_000u64);
    let (series_id, seq) = (sid(7), 0u32);
    // Difficulty 1: ~1/256 of nonces pass; brute-force a valid and an invalid one.
    let mut good = None;
    let mut bad = None;
    for n in 0u64..100_000 {
        let ok = runtime::validate_pow(owner(), pa, series_id, seq, n).is_ok();
        if ok && good.is_none() {
            good = Some(n);
        }
        if !ok && bad.is_none() {
            bad = Some(n);
        }
        if good.is_some() && bad.is_some() {
            break;
        }
    }
    assert!(
        runtime::validate_pow(owner(), pa, series_id, seq, good.expect("a valid nonce")).is_ok()
    );
    assert!(
        runtime::validate_pow(owner(), pa, series_id, seq, bad.expect("an invalid nonce")).is_err()
    );
}

/// A dummy authorization for mine_promis paths that reject before the (enclave)
/// Promis mint (zero amount / missing series).
fn no_auth() -> outbe_promisfactory::api::ModifyAuth {
    outbe_promisfactory::api::ModifyAuth {
        mac: [0u8; 32],
        op_nonce: 0,
    }
}

#[test]
fn mine_promis_rejects_zero_amount() {
    with_factory(|s| {
        assert!(runtime::mine_promis(&s, sid(7), owner(), U256::ZERO, 0, no_auth()).is_err());
    });
}

#[test]
fn mine_promis_rejects_missing_series() {
    with_factory(|s| {
        assert!(runtime::mine_promis(&s, sid(7), owner(), U256::from(1), 0, no_auth()).is_err());
    });
}

/// A mining that fails after any of its writes leaves the sequence, the units and every
/// event as they were, so the same nonce and the same paid units can be tried again.
#[test]
fn a_mining_that_fails_after_any_write_changes_nothing() {
    use crate::sol_ext::IERC1155;

    let mut failures = 0;
    for failure_at in 0..16 {
        let mut storage = factory_provider();
        storage.stub_sub_call_at_selector(
            crate::constants::INTEX_NFT1155_ADDRESS,
            IERC1155::balanceOfCall::SELECTOR,
            word(1),
        );
        let nonce = StorageHandle::enter(&mut storage, |s| {
            select_prod_profile(&s);
            runtime::issue(&s, sample(7)).unwrap();
            let promis_amount = outbe_intex::api::read_series(&s, sid(7))
                .unwrap()
                .promis_load_minor;
            (0u64..)
                .find(|nonce| {
                    runtime::validate_pow(owner(), promis_amount, sid(7), 0, *nonce).is_ok()
                })
                .unwrap()
        });
        let slots = storage.storage.clone();
        let events = storage.get_ordered_events().to_vec();

        storage.fail_after_mutation_at(failure_at);
        let mined = StorageHandle::enter(&mut storage, |s| {
            runtime::mine_promis(&s, sid(7), owner(), U256::ONE, nonce, no_auth())
        });
        if mined.is_ok() {
            continue;
        }
        failures += 1;
        assert_eq!(
            storage.storage, slots,
            "a write survived failure {failure_at}"
        );
        assert_eq!(storage.get_ordered_events(), events, "failure {failure_at}");
        let seq = StorageHandle::enter(&mut storage, |s| {
            IntexFactoryContract::new(s).read_mine_seq(sid(7), owner())
        })
        .unwrap();
        assert_eq!(seq, 0, "failure {failure_at}");
    }
    assert!(failures > 0);
}

/// The view hands a reader the disjoint classes, so nobody has to redo the arithmetic
/// against the separate ledgers.
#[test]
fn the_unit_counts_view_reports_the_disjoint_classes() {
    with_factory(|s| {
        runtime::issue(&s, sample(7)).unwrap();
        outbe_intex::api::record_settled_units(&s, sid(7), 40).unwrap();
        outbe_intex::api::record_gem_factory_units(&s, sid(7), owner(), 10).unwrap();
        outbe_intex::api::record_exercised_units(&s, sid(7), owner(), 15).unwrap();

        // Through dispatch, so the selector and the struct encoding are covered too.
        let out = precompile::dispatch(
            s.clone(),
            &IIntexFactory::seriesUnitCountsCall {
                seriesId: sid(7).into(),
            }
            .abi_encode(),
            owner(),
            U256::ZERO,
        )
        .unwrap();
        let counts = IIntexFactory::seriesUnitCountsCall::abi_decode_returns(&out).unwrap();
        assert_eq!(counts.issuedUnits, 100);
        assert_eq!(counts.activeUnits, 50);
        assert_eq!(counts.settledUnits, 25);
        assert_eq!(counts.exercisedUnits, 15);
        assert_eq!(counts.gemFactoryUnits, 10);
        assert_eq!(counts.forfeitedUnits, 0);
    });
}

#[test]
fn merged_paynote_settles_intex_units_without_additional_funding() {
    let (_, outcome) = settle_two_units_spending_from(TWO_UNIT_COST, true);
    assert_eq!(outcome.unwrap(), U256::from(2));
}
