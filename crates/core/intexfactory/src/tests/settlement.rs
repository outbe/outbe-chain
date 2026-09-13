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
fn a_subunit_cost_is_refused_rather_than_rounded_up() {
    let err = runtime::settlement_units(U256::ONE, U256::ONE, None, 0).unwrap_err();
    assert!(err.to_string().contains("rounds to zero"), "{err}");
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
            let (_, cost) =
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
            holder(),
            U256::ZERO,
        )
        .unwrap();
        let ret = IIntexFactory::quoteSettlementCall::abi_decode_returns(&out).unwrap();
        assert_eq!(ret.settlementCurrency, 840);
        assert_eq!(ret.payableUnits, U256::from(2_000_000_000_000_000_000u64));
    });
}

/// A settle from a stranger must succeed and book the units to the holder.
#[test]
fn anyone_may_settle_and_the_units_stay_with_the_holder() {
    use crate::sol_ext::{IReferenceCurrency, IERC1155, IERC20};
    use alloy_sol_types::SolEvent;
    use outbe_vaultrouter::api::IVaultRouter;

    let payer = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
    let units = U256::from(2u64);
    // Two units of `sample(7)` at six decimals; the note covers exactly that.
    let cost = U256::from(2_000_000u64);

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

    let fixture = outbe_paynote::test_support::note_and_spend_proof(
        CHAIN_ID,
        payment_token(),
        payer,
        cost,
        cost,
    );
    outbe_paynote::test_support::seed_pool(&mut storage, CHAIN_ID, &[fixture.commitment]);

    StorageHandle::enter(&mut storage, |s| {
        runtime::issue(&s, sample(7)).unwrap();
        outbe_intex::api::mark_qualified(&s, sid(7)).unwrap();
        runtime::settle(&s, sid(7), holder(), payer, units, &fixture.proof).unwrap();

        assert_eq!(
            outbe_intex::api::settled_units(&s, sid(7)).unwrap(),
            2,
            "the units are booked settled"
        );
    });

    let sig = IIntexFactory::Settled::SIGNATURE_HASH;
    let settled: Vec<_> = storage
        .get_events(INTEX_FACTORY_ADDRESS)
        .iter()
        .filter(|log| log.topics().first() == Some(&sig))
        .map(|log| IIntexFactory::Settled::decode_log_data(log).unwrap())
        .collect();
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0].intexHolder, holder(), "the payer keeps nothing");
    assert_eq!(settled[0].amount, units);
}

// ---------------------------------------------------------------------
// settle gating (value movement is localnet-exercised, not unit tested)
// ---------------------------------------------------------------------

#[test]
fn settle_rejects_zero_amount() {
    with_factory(|s| {
        assert!(runtime::settle(&s, sid(7), holder(), holder(), U256::ZERO, &[]).is_err());
    });
}

#[test]
fn settle_rejects_missing_series() {
    with_factory(|s| {
        assert!(runtime::settle(&s, sid(7), holder(), holder(), U256::from(1), &[]).is_err());
    });
}

#[test]
fn settle_rejects_wrong_state_issued() {
    with_factory(|s| {
        // Born Issued; settlement is only valid in Qualified/Called.
        runtime::issue(&s, sample(7)).unwrap();
        let err = runtime::settle(&s, sid(7), holder(), holder(), U256::from(1), &[]).unwrap_err();
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
        let err = runtime::settle(&s, sid(7), holder(), holder(), U256::from(1), &[]).unwrap_err();
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
    // SHA256(holder ++ promisAmount_be32 ++ seriesId ++ seq_be4 ++ nonce_be8)
    let promis_amount = U256::from(1_000u64);
    let (series_id, seq, nonce) = (sid(7), 3u32, 42u64);
    let got = runtime::compute_pow_hash(holder(), promis_amount, series_id, seq, nonce);

    let mut data = holder().as_slice().to_vec();
    data.extend_from_slice(&promis_amount.to_be_bytes::<32>());
    data.extend_from_slice(series_id.as_bytes());
    data.extend_from_slice(&seq.to_be_bytes());
    data.extend_from_slice(&nonce.to_be_bytes());
    let expected = ring::digest::digest(&ring::digest::SHA256, &data);
    assert_eq!(got.as_slice(), expected.as_ref());
}

#[test]
fn validate_pow_accepts_valid_and_rejects_invalid_nonce() {
    let pa = U256::from(1_000u64);
    let (series_id, seq) = (sid(7), 0u32);
    // Difficulty 1: ~1/256 of nonces pass; brute-force a valid and an invalid one.
    let mut good = None;
    let mut bad = None;
    for n in 0u64..100_000 {
        let ok = runtime::validate_pow(holder(), pa, series_id, seq, n).is_ok();
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
        runtime::validate_pow(holder(), pa, series_id, seq, good.expect("a valid nonce")).is_ok()
    );
    assert!(
        runtime::validate_pow(holder(), pa, series_id, seq, bad.expect("an invalid nonce"))
            .is_err()
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
        assert!(runtime::mine_promis(&s, sid(7), holder(), U256::ZERO, 0, no_auth()).is_err());
    });
}

#[test]
fn mine_promis_rejects_missing_series() {
    with_factory(|s| {
        assert!(runtime::mine_promis(&s, sid(7), holder(), U256::from(1), 0, no_auth()).is_err());
    });
}
