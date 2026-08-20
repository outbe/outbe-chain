use super::*;

fn entry_price() -> U256 {
    U256::from(500_000u64)
}

fn load_minor() -> U256 {
    U256::from(100_000u64) * U256::from(1_000_000u64)
}

#[test]
fn cost_amount_six_decimals() {
    let cost = runtime::derived_cost_amount(entry_price(), load_minor(), 6).unwrap();
    assert_eq!(cost, U256::from(50_000_000_000u64));
}

#[test]
fn cost_amount_eighteen_decimals_is_1e12_larger() {
    let six = runtime::derived_cost_amount(entry_price(), load_minor(), 6).unwrap();
    let eighteen = runtime::derived_cost_amount(entry_price(), load_minor(), 18).unwrap();
    assert_eq!(eighteen, six * U256::from(10u64).pow(U256::from(12u64)));
}

#[test]
fn cost_amount_zero_decimals() {
    let cost = runtime::derived_cost_amount(entry_price(), load_minor(), 0).unwrap();
    assert_eq!(cost, U256::from(50_000u64));
}

#[test]
fn cost_amount_twelve_decimals() {
    let cost = runtime::derived_cost_amount(entry_price(), load_minor(), 12).unwrap();
    assert_eq!(cost, U256::from(50_000_000_000_000_000u64));
}

#[test]
fn cost_amount_rounds_positive_subunit_payment_up_to_one() {
    let cost = runtime::derived_cost_amount(U256::ONE, U256::ONE, 0).unwrap();
    assert_eq!(cost, U256::ONE);
}

#[test]
fn cost_amount_rejects_unsupported_payment_decimals() {
    let err = runtime::derived_cost_amount(entry_price(), load_minor(), 19).unwrap_err();
    assert!(err.to_string().contains("unsupported decimals"), "{err}");
}

#[test]
fn cost_amount_rejects_product_overflow() {
    let err = runtime::derived_cost_amount(U256::MAX, U256::from(2u64), 6).unwrap_err();
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
fn cost_amount_prices_an_accepted_token() {
    for (decimals, expected) in [
        (0, U256::ONE),
        (6, U256::from(1_000_000u64)),
        (12, U256::from(1_000_000_000_000u64)),
        (18, U256::from(1_000_000_000_000_000_000u64)),
    ] {
        with_payment_token(1, 840, decimals, |s| {
            let cost = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap();
            assert_eq!(cost, expected, "payment token decimals {decimals}");
        });
    }
}

#[test]
fn cost_amount_rejects_an_unregistered_token() {
    with_payment_token(0, 840, 18, |s| {
        let err = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap_err();
        assert!(err.to_string().contains("no registered vault"), "{err}");
    });
}

#[test]
fn cost_amount_rejects_a_foreign_currency() {
    with_payment_token(1, 978, 18, |s| {
        let err = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
    });
}

#[test]
fn cost_amount_rejects_missing_series() {
    with_factory(|s| {
        assert!(runtime::quote_cost_amount(&s, sid(7), payment_token()).is_err());
    });
}

#[test]
fn cost_amount_dispatch() {
    with_payment_token(1, 840, 18, |s| {
        let out = precompile::dispatch(
            s.clone(),
            &IIntexFactory::quoteCostAmountCall {
                seriesId: sid(7).into(),
                paymentToken: payment_token(),
            }
            .abi_encode(),
            holder(),
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IIntexFactory::quoteCostAmountCall::abi_decode_returns(&out).unwrap(),
            U256::from(1_000_000_000_000_000_000u64)
        );
    });
}

// ---------------------------------------------------------------------
// settle gating (value movement is localnet-exercised, not unit tested)
// ---------------------------------------------------------------------

#[test]
fn settle_rejects_zero_amount() {
    with_factory(|s| {
        assert!(
            runtime::settle(&s, sid(7), holder(), holder(), U256::ZERO, payment_token()).is_err()
        );
    });
}

#[test]
fn settle_rejects_missing_series() {
    with_factory(|s| {
        assert!(runtime::settle(
            &s,
            sid(7),
            holder(),
            holder(),
            U256::from(1),
            payment_token()
        )
        .is_err());
    });
}

#[test]
fn settle_rejects_wrong_state_issued() {
    with_factory(|s| {
        // Born Issued; settlement is only valid in Qualified/Called.
        runtime::issue(&s, sample(7)).unwrap();
        let err = runtime::settle(
            &s,
            sid(7),
            holder(),
            holder(),
            U256::from(1),
            payment_token(),
        )
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
        let err = runtime::settle(
            &s,
            sid(7),
            holder(),
            holder(),
            U256::from(1),
            payment_token(),
        )
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("deadline"));
    });
}

#[test]
fn set_authorized_settler_round_trip() {
    with_factory(|s| {
        let settler = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
        runtime::set_authorized_settler(&s, holder(), sid(7), settler).unwrap();
        let f = IntexFactoryContract::new(s.clone());
        assert_eq!(
            f.read_authorized_settler(holder(), sid(7)).unwrap(),
            settler
        );
    });
}
