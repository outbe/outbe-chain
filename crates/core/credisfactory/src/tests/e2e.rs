//! End-to-end flow: mine → pledge → requestCredis → payAnadosis → unlock.
//!
//! The confidential Gratis path runs against the in-process enclave engine
//! (`outbe_gratis::enclave_client::test_enclave`); balances/pledged amounts are
//! asserted by decrypting the ciphertext with the account's view key, exactly as
//! a client would. `HashMapStorageProvider` does not run a real EVM, so the
//! runtime's Rust → Solidity sub-calls into `IVaultRouter` / `IERC20` are
//! stubbed via `enable_sub_call_stub` (returns `default_success()`).

use alloy_primitives::{Address, Bytes, B256, U256};

use outbe_credis::{CredisContract, NUMBER_OF_ANADOSIS, SECONDS_PER_MONTH};
use outbe_fidelity::enclave_client::test_enclave as fidelity_enclave;
use outbe_gratis::enclave_client::test_enclave;
use outbe_gratisfactory::runtime as gf;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::PromisLimitContract;
use outbe_tee::protocol::{GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{
    decrypt_balance, decrypt_pledged, derive_modify_key, derive_view_key, modify_mac,
    pledge_secret, spend_auth_mac,
};

use crate::runtime;
use crate::tests::common::*;

/// Issuance currency (ISO 4217) reported by `asset()`'s stubbed `isoCode()`.
const ISSUANCE_ISO: u16 = 840;

/// currency rate seeded for USD in these e2e tests (4.30 %, 1e18 scaled).
fn refi_rate() -> U256 {
    U256::from(43_000_000_000_000_000u128)
}

fn one_e18() -> U256 {
    U256::from(10u64).pow(U256::from(18u64))
}

/// COEN/0xUSD rate these tests seed: 2.0, 1e18-scaled.
fn oracle_rate() -> U256 {
    U256::from(2u64) * one_e18()
}

/// Credit a pledge asks for: $2.00 in 6-decimal minor units. At [`oracle_rate`] that
/// costs exactly [`pledge_cost`] gratis.
fn pledge_stables() -> U256 {
    U256::from(2_000_000u64)
}

/// Gratis collateral [`pledge_stables`] costs: `2e6 * 1e12 * 1e18 / 2e18 = 1e18`.
fn pledge_cost() -> U256 {
    one_e18()
}

/// Pledge [`pledge_stables`] of credit for `who` at op-nonce `nonce` (uncapped), and
/// return the resulting handle. The gratis it costs is derived from the seeded rate.
fn pledge(storage: &StorageHandle<'_>, who: Address, nonce: u64) -> B256 {
    let (handle, gratis_cost) = gf::pledge_gratis(
        storage.clone(),
        who,
        pledge_stables(),
        asset(),
        U256::MAX,
        auth(GratisOp::Pledge, who, pledge_stables(), nonce),
    )
    .unwrap();
    assert_eq!(gratis_cost, pledge_cost(), "seeded rate drifted");
    handle
}

fn chain_b256() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}

fn seed_oracle(storage: StorageHandle<'_>, rate_1e18: U256) {
    let mut oracle = OracleContract::new(storage);
    oracle.register_pair("COEN", "0xUSD").unwrap();
    oracle
        .set_exchange_rate(Address::ZERO, "COEN", "0xUSD", rate_1e18, 0, 0)
        .unwrap();
    oracle
        .reference_currency_rate
        .write(&ISSUANCE_ISO, refi_rate())
        .unwrap();
}

/// Pays exactly what the position's next installment still owes, and returns the
/// amount actually pulled.
fn pay_next_installment(storage: &StorageHandle<'_>, caller: Address, position_id: U256) -> U256 {
    let owed = CredisContract::new(storage.clone())
        .get_next_anadosis(position_id)
        .unwrap()
        .unwrap()
        .unpaid_amount;
    runtime::pay_anadosis(storage.clone(), caller, position_id, owed).unwrap()
}

/// ABI-encoded `uint16` return for the asset's `isoCode()` static sub-call.
fn iso_word(iso: u16) -> Bytes {
    let mut b = vec![0u8; 32];
    b[30..32].copy_from_slice(&iso.to_be_bytes());
    Bytes::from(b)
}

/// 32-byte zero word — the stubbed `uint256` return for the vault sub-calls.
fn zero_word() -> Bytes {
    Bytes::from(vec![0u8; 32])
}

/// Positive Fidelity so `gratisfactory::pledge_gratis` clears the eligibility gate.
fn seed_fidelity(storage: StorageHandle<'_>, account: Address) {
    const ONE_YEAR_SECS: u64 = 365 * 86_400;
    outbe_fidelity::api::cohort_in(
        storage,
        account,
        U256::from(100u64),
        CREATED_AT - ONE_YEAR_SECS,
    )
    .unwrap();
}

fn auth(op: GratisOp, owner: Address, amount: U256, op_nonce: u64) -> ModifyAuth {
    let mk = derive_modify_key(&test_enclave::state_key(), owner).unwrap();
    ModifyAuth {
        mac: modify_mac(&mk, owner, op, amount, op_nonce, chain_b256()),
        op_nonce,
    }
}

fn view_balance(s: &StorageHandle<'_>, a: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), a).unwrap();
    let blob = outbe_gratis::api::balance_ct(s.clone(), a).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_balance(&vk, a, &blob).unwrap()
}

fn view_pledged(s: &StorageHandle<'_>, a: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), a).unwrap();
    let blob = outbe_gratis::api::pledged_ct(s.clone(), a).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_pledged(&vk, a, &blob).unwrap()
}

/// The spend authorization the pledger EOA hands to the CCA to bind a pledge to a
/// destination smart account (`HMAC(pledgeSecret, "credis-bind" || bundle)`).
fn credis_spend_auth(eoa: Address, handle: B256, bundle: Address) -> [u8; 32] {
    let mk = derive_modify_key(&test_enclave::state_key(), eoa).unwrap();
    spend_auth_mac(&pledge_secret(&mk, handle), bundle)
}

/// Storage set up with the block time, sub-call stubs, and the enclave installed.
fn env() -> HashMapStorageProvider {
    test_enclave::install();
    fidelity_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    storage.set_block_number(BLOCK_NUMBER);
    storage.enable_sub_call_stub();
    storage.stub_sub_call_at(VAULT_ROUTER_ADDRESS, zero_word());
    storage.stub_sub_call_at(asset(), iso_word(ISSUANCE_ISO));
    storage
}

#[test]
fn full_pledge_request_pay_unlock_flow() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let pledge_amount = one_e18();
        let installment = pledge_amount / U256::from(NUMBER_OF_ANADOSIS);

        // Mine + pledge. Alice is both the pledger EOA and the smart account here.
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            pledge_amount,
            auth(GratisOp::Mint, alice(), pledge_amount, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());
        seed_oracle(storage.clone(), oracle_rate());
        let handle = pledge(&storage, alice(), 1);
        // Pledge parks the amount in the ticket: balance drained, pledged ledger 0.
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);

        // requestCredis bound to alice's smart account, with alice as the pledger
        // EOA. The collateral is credited into alice's OWN pledged ledger.
        let spend = credis_spend_auth(alice(), handle, alice());
        let (position_id, amount_stables) =
            runtime::request_credis(storage.clone(), alice(), alice(), handle, spend).unwrap();
        assert_eq!(view_pledged(&storage, alice()), pledge_amount);

        // The loan is exactly what the pledger asked for — credis does not re-price it.
        assert_eq!(amount_stables, pledge_stables());

        let credis = CredisContract::new(storage.clone());
        let position = credis.get_position(position_id).unwrap();
        assert_eq!(position.smart_account, alice());
        // The pledger EOA is stored sealed (ciphertext), never as a plaintext address,
        // and the enclave opens it back to alice via RevealOwner.
        assert!(!position.eoa_ct.is_empty(), "eoa stored as ciphertext");
        assert_eq!(
            outbe_gratis::api::reveal_owner(storage.clone(), &position.eoa_ct).unwrap(),
            alice()
        );
        assert_eq!(position.credis_principal, amount_stables);
        // The entry price is the rate the PLEDGE was quoted at, not a later read.
        assert_eq!(position.entry_price_minor, oracle_rate());
        assert_eq!(position.currency_rate, refi_rate());
        assert_eq!(position.issuance_currency, ISSUANCE_ISO);
        let multiplier =
            one_e18() + refi_rate() * U256::from(NUMBER_OF_ANADOSIS) / U256::from(12u64);
        assert_eq!(
            position.total_anadosis_amount,
            amount_stables * multiplier / one_e18()
        );
        assert_eq!(position.total_gratis_amount, pledge_amount);

        // Pay each installment; each releases 1/10 of the collateral from alice's own
        // pledged ledger back to her balance, one installment at a time.
        for n in 1..=NUMBER_OF_ANADOSIS {
            storage
                .set_block_timestamp(U256::from(CREATED_AT + n as u64 * SECONDS_PER_MONTH))
                .unwrap();
            pay_next_installment(&storage, alice(), position_id);

            let unlocked = U256::from(n) * installment;
            assert_eq!(view_balance(&storage, alice()), unlocked, "installment {n}");
            assert_eq!(
                view_pledged(&storage, alice()),
                pledge_amount - unlocked,
                "pledged {n}"
            );
        }

        // Fully drained: alice holds the whole pledge again; pledged ledger empty.
        assert_eq!(view_balance(&storage, alice()), pledge_amount);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
    });
    fidelity_enclave::uninstall();
    test_enclave::uninstall();
}

#[test]
fn pay_anadosis_unlocks_one_installment() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let pledge_amount = one_e18();
        let installment = pledge_amount / U256::from(NUMBER_OF_ANADOSIS);

        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            pledge_amount,
            auth(GratisOp::Mint, alice(), pledge_amount, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());
        seed_oracle(storage.clone(), oracle_rate());
        let handle = pledge(&storage, alice(), 1);

        let spend = credis_spend_auth(alice(), handle, alice());
        let (position_id, _) =
            runtime::request_credis(storage.clone(), alice(), alice(), handle, spend).unwrap();

        // Pay a single installment: releases exactly pledge/10 from alice's pledged
        // ledger right away, without waiting for the loan to complete.
        storage
            .set_block_timestamp(U256::from(CREATED_AT + SECONDS_PER_MONTH))
            .unwrap();
        pay_next_installment(&storage, alice(), position_id);

        assert_eq!(view_balance(&storage, alice()), installment);
        assert_eq!(view_pledged(&storage, alice()), pledge_amount - installment);
    });
    fidelity_enclave::uninstall();
    test_enclave::uninstall();
}

#[test]
fn pay_anadosis_spans_installments_and_caps_at_outstanding() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let pledge_amount = one_e18();

        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            pledge_amount,
            auth(GratisOp::Mint, alice(), pledge_amount, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());
        seed_oracle(storage.clone(), oracle_rate());
        let handle = pledge(&storage, alice(), 1);

        let spend = credis_spend_auth(alice(), handle, alice());
        let (position_id, _) =
            runtime::request_credis(storage.clone(), alice(), alice(), handle, spend).unwrap();

        let total_debt = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap()
            .total_anadosis_amount;
        let per_installment = total_debt / U256::from(NUMBER_OF_ANADOSIS);

        storage
            .set_block_timestamp(U256::from(CREATED_AT + SECONDS_PER_MONTH))
            .unwrap();

        // 2.5 installments in one call: #1 and #2 settle, #3 keeps half owed.
        let amount = per_installment * U256::from(2u64) + per_installment / U256::from(2u64);
        let paid = runtime::pay_anadosis(storage.clone(), alice(), position_id, amount).unwrap();
        assert_eq!(paid, amount, "whole budget consumed — schedule had room");

        let credis = CredisContract::new(storage.clone());
        assert_eq!(
            credis
                .get_position(position_id)
                .unwrap()
                .next_anadosis_number,
            3
        );
        let third = credis.get_anadosis(position_id, 3).unwrap();
        assert_eq!(
            third.unpaid_amount,
            third.anadosis_amount - per_installment / U256::from(2u64)
        );
        assert_eq!(third.paid_at, 0, "partially paid installment stays open");

        // Collateral released tracks the debt paid down, and the two ledgers stay
        // complementary (nothing minted, nothing stranded).
        let released = view_balance(&storage, alice());
        assert_eq!(view_pledged(&storage, alice()), pledge_amount - released);
        assert!(released > U256::ZERO && released < pledge_amount);

        // Overpaying the rest takes only what is outstanding and closes the position,
        // returning the full pledge to alice's liquid balance.
        let outstanding = credis
            .get_position(position_id)
            .unwrap()
            .outstanding_anadosis_amount;
        let rest = runtime::pay_anadosis(
            storage.clone(),
            alice(),
            position_id,
            outstanding * U256::from(1_000u64),
        )
        .unwrap();
        assert_eq!(rest, outstanding, "only the required amount is used");
        assert_eq!(paid + rest, total_debt, "never more than the total debt");

        assert_eq!(view_balance(&storage, alice()), pledge_amount);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
    });
    fidelity_enclave::uninstall();
    test_enclave::uninstall();
}

#[test]
fn request_credis_rejects_overdue_anadosis() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let amount = one_e18();
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            amount * U256::from(2u64),
            auth(GratisOp::Mint, alice(), amount * U256::from(2u64), 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());
        seed_oracle(storage.clone(), oracle_rate());

        // First pledge + request.
        let h1 = pledge(&storage, alice(), 1);
        let spend1 = credis_spend_auth(alice(), h1, alice());
        runtime::request_credis(storage.clone(), alice(), alice(), h1, spend1).unwrap();

        // Second pledge — then attempt a second request once anadosis-1 is overdue
        // on the first position.
        let h2 = pledge(&storage, alice(), 2);
        let spend2 = credis_spend_auth(alice(), h2, alice());
        storage
            .set_block_timestamp(U256::from(CREATED_AT + SECONDS_PER_MONTH + 1))
            .unwrap();
        let err =
            runtime::request_credis(storage.clone(), alice(), alice(), h2, spend2).unwrap_err();
        assert!(err.to_string().contains("overdue"), "got: {err}");
    });
    fidelity_enclave::uninstall();
    test_enclave::uninstall();
}

// A zero asset can no longer reach requestCredis — it is rejected at pledge time now
// that the asset travels in the ticket. See
// `outbe_gratisfactory::tests::pledge_rejects_zero_asset`.

#[test]
fn request_credis_rejects_zero_smart_account() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let err = runtime::request_credis(
            storage.clone(),
            alice(),
            Address::ZERO,
            B256::ZERO,
            [0u8; 32],
        )
        .unwrap_err();
        assert!(err.to_string().contains("smart account"), "got: {err}");
    });
}

#[test]
fn pay_anadosis_accepts_third_party_payer() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let pledge_amount = one_e18();
        let installment = pledge_amount / U256::from(NUMBER_OF_ANADOSIS);

        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            pledge_amount,
            auth(GratisOp::Mint, alice(), pledge_amount, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());
        seed_oracle(storage.clone(), oracle_rate());
        let handle = pledge(&storage, alice(), 1);
        let spend = credis_spend_auth(alice(), handle, alice());
        let (position_id, _) =
            runtime::request_credis(storage.clone(), alice(), alice(), handle, spend).unwrap();

        // bob is not the position's smart account, but anyone may pay it down.
        let owed = CredisContract::new(storage.clone())
            .get_next_anadosis(position_id)
            .unwrap()
            .unwrap()
            .unpaid_amount;
        let paid = runtime::pay_anadosis(storage.clone(), bob(), position_id, owed).unwrap();
        assert_eq!(paid, owed);
        assert_eq!(
            CredisContract::new(storage.clone())
                .get_position(position_id)
                .unwrap()
                .next_anadosis_number,
            2,
            "third-party payment advances the schedule"
        );

        // The freed collateral goes to the ORIGINAL pledger, never to the payer — this
        // is what makes an open payer safe without an access check.
        assert_eq!(view_balance(&storage, alice()), installment);
        assert_eq!(view_pledged(&storage, alice()), pledge_amount - installment);
        assert_eq!(view_balance(&storage, bob()), U256::ZERO);
        assert_eq!(view_pledged(&storage, bob()), U256::ZERO);
    });
    fidelity_enclave::uninstall();
    test_enclave::uninstall();
}

#[test]
fn expiry_sweep_burns_outstanding_collateral() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let pledge_amount = one_e18();
        let installment = pledge_amount / U256::from(NUMBER_OF_ANADOSIS);

        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            pledge_amount,
            auth(GratisOp::Mint, alice(), pledge_amount, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());
        seed_oracle(storage.clone(), oracle_rate());
        let handle = pledge(&storage, alice(), 1);
        let spend = credis_spend_auth(alice(), handle, alice());
        let (position_id, _) =
            runtime::request_credis(storage.clone(), alice(), alice(), handle, spend).unwrap();

        // Pay 3 of 10 installments, leaving 7/10 of the collateral outstanding.
        for n in 1..=3u64 {
            storage
                .set_block_timestamp(U256::from(CREATED_AT + n * SECONDS_PER_MONTH))
                .unwrap();
            pay_next_installment(&storage, alice(), position_id);
        }
        let outstanding_gratis = pledge_amount - U256::from(3u64) * installment;
        assert_eq!(view_pledged(&storage, alice()), outstanding_gratis);

        // Encrypted cohort ledger before the burn — the burn records a sale
        // cohort, so the ciphertext must change afterwards.
        let expiry_ts = CREATED_AT + NUMBER_OF_ANADOSIS as u64 * SECONDS_PER_MONTH;
        let cohorts_before = outbe_fidelity::FidelityContract::new(storage.clone())
            .cohorts_ct_of(alice())
            .unwrap();
        assert!(!cohorts_before.is_empty(), "alice has a seeded cohort");

        // Advance past the 10-month term and run the begin-block expiry sweep.
        let sweep_ts = expiry_ts + 1;
        storage.set_block_timestamp(U256::from(sweep_ts)).unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(BLOCK_NUMBER, sweep_ts, CHAIN_ID),
            storage.clone(),
        );
        assert_eq!(crate::lifecycle::scan_and_expire(&ctx).unwrap(), 1);

        // Collateral burned from alice's own pledged ledger; supply reduced by it.
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            pledge_amount - outstanding_gratis
        );
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );

        // The equivalent value was deposited 1:1 into the Promis Reserve.
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            outstanding_gratis
        );

        // Position closed: outstanding balances zeroed.
        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert!(position.outstanding_anadosis_amount.is_zero());
        assert!(position.outstanding_gratis_amount.is_zero());

        // A sale cohort was recorded for the burned collateral, so alice's
        // encrypted cohort ledger changed (the RCFI-drop semantics itself is
        // covered by the fidelity + enclave tests).
        let cohorts_after = outbe_fidelity::FidelityContract::new(storage.clone())
            .cohorts_ct_of(alice())
            .unwrap();
        assert_ne!(
            cohorts_before, cohorts_after,
            "expiry burn should record a sale cohort"
        );

        // Idempotent: a second sweep at the same height finds nothing to burn.
        let ctx2 = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(BLOCK_NUMBER, sweep_ts, CHAIN_ID),
            storage.clone(),
        );
        assert_eq!(crate::lifecycle::scan_and_expire(&ctx2).unwrap(), 0);
    });
    fidelity_enclave::uninstall();
    test_enclave::uninstall();
}
