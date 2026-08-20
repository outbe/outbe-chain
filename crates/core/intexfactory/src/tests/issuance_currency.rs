use super::*;

/// Issue a series whose issuance currency differs from its reference, with a
/// payment token reporting `iso` and 18 decimals and a registered vault.
fn with_dual_currency_series<R>(iso: u64, f: impl FnOnce(StorageHandle) -> R) -> R {
    use crate::sol_ext::{IReferenceCurrency, IERC20};
    use outbe_vaultrouter::api::IVaultRouter;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(crate::constants::INTEX_NFT1155_ADDRESS, word(0));
    storage.stub_sub_call_at(crate::constants::ORIGIN_ROUTER_ADDRESS, word(0));
    storage.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall::SELECTOR,
        word(1),
    );
    storage.stub_sub_call_at_selector(
        payment_token(),
        IReferenceCurrency::isoCodeCall::SELECTOR,
        word(iso),
    );
    storage.stub_sub_call_at_selector(payment_token(), IERC20::decimalsCall::SELECTOR, word(18));

    StorageHandle::enter(&mut storage, |s| {
        let params = IssuanceParams {
            issuance_currency: EUR_ISO,
            ..sample(7)
        };
        runtime::issue(&s, params).unwrap();
        f(s)
    })
}

/// Every stablecoin-backed COEN/ISO Oracle rate uses six decimals.
const COEN_ISO_RATE_SCALE: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);

/// Publish a COEN rate for `iso_code`, stamped `age` seconds ago.
fn publish_rate(oracle: &OracleContract, iso_code: u16, pair_id: u32, rate: U256, age: u64) {
    write_rate(oracle, iso_code, pair_id, rate);
    oracle
        .exchange_rate_timestamp
        .write(&pair_id, ISSUED_AT as u64 - age)
        .unwrap();
}

#[test]
fn the_issuance_currency_settles_through_the_coen_pivot() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        // COEN buys twice as many dollars as euros, so the euro price of one
        // Intex is half its dollar price.
        publish_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
            0,
        );
        publish_rate(&oracle, EUR_ISO, EUR_PAIR_ID, COEN_ISO_RATE_SCALE, 0);

        let cost = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap();
        assert_eq!(cost, U256::from(500_000_000_000_000_000u64));
    });
}

#[test]
fn issuance_currency_settlement_rounds_a_non_divisible_fx_result_up_once() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        publish_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(3u64) * COEN_ISO_RATE_SCALE,
            0,
        );
        publish_rate(&oracle, EUR_ISO, EUR_PAIR_ID, COEN_ISO_RATE_SCALE, 0);

        let cost = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap();
        assert_eq!(cost, U256::from(333_333_333_333_333_334u64));
    });
}

#[test]
fn an_unpriced_issuance_currency_cannot_be_settled_in() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        publish_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
            0,
        );
        // No euro pair at all.
        let err = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap_err();
        assert!(err.to_string().contains("no COEN rate published"), "{err}");
    });
}

#[test]
fn a_stale_rate_cannot_be_settled_in() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        publish_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
            0,
        );
        publish_rate(
            &oracle,
            EUR_ISO,
            EUR_PAIR_ID,
            COEN_ISO_RATE_SCALE,
            crate::constants::FX_RATE_MAX_AGE_SECONDS + 1,
        );

        let err = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap_err();
        assert!(err.to_string().contains("too old"), "{err}");
    });
}

#[test]
fn issuance_currency_settlement_rejects_fx_overflow() {
    with_dual_currency_series(EUR_ISO as u64, |s| {
        let oracle = OracleContract::new(s.clone());
        publish_rate(&oracle, REFERENCE_ISO, PAIR_ID, COEN_ISO_RATE_SCALE, 0);
        publish_rate(&oracle, EUR_ISO, EUR_PAIR_ID, U256::MAX, 0);

        let err = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("overflow"), "{err}");
    });
}

#[test]
fn the_reference_currency_settles_without_reading_any_rate() {
    // No rate is published at all, yet the reference currency still settles.
    with_dual_currency_series(REFERENCE_ISO as u64, |s| {
        let cost = runtime::quote_cost_amount(&s, sid(7), payment_token()).unwrap();
        assert_eq!(cost, U256::from(1_000_000_000_000_000_000u64));
    });
}
