//! End-to-end flow: mine → pledge → requestCredis → payAnadosis → unlock.
//!
//! The confidential Gratis path runs against the in-process enclave engine
//! (`outbe_gratis::enclave_client::test_enclave`); balances/pledged amounts are
//! asserted by decrypting the ciphertext with the account's view key, exactly as
//! a client would. `HashMapStorageProvider` does not run a real EVM, so the
//! runtime's Rust → Solidity sub-calls into `IVaultProvider` / `IERC20` are
//! stubbed via `enable_sub_call_stub` (returns `default_success()`).

use alloy_primitives::{Address, Bytes, B256, U256};

use outbe_credis::{CredisContract, NUMBER_OF_ANADOSIS, SECONDS_PER_MONTH};
use outbe_gratis::enclave_client::test_enclave;
use outbe_gratisfactory::runtime as gf;
use outbe_oracle::contract::OracleContract;
use outbe_primitives::addresses::VAULT_PROVIDER_ADDRESS;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_tee::protocol::{GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{
    decrypt_balance, decrypt_pledged, derive_modify_key, derive_view_key, modify_mac,
    pledge_secret, spend_auth_mac,
};

use crate::runtime;
use crate::tests::common::*;

/// Issuance currency (ISO 4217) reported by `asset()`'s stubbed `isoCode()`.
const ISSUANCE_ISO: u16 = 840;

/// Refinancing rate seeded for USD in these e2e tests (4.30 %, 1e18 scaled).
fn refi_rate() -> U256 {
    U256::from(43_000_000_000_000_000u128)
}

fn one_e18() -> U256 {
    U256::from(10u64).pow(U256::from(18u64))
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
        .reference_refinancing_rate
        .write(&ISSUANCE_ISO, refi_rate())
        .unwrap();
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
/// destination bundle account (`HMAC(pledgeSecret, "credis-bind" || bundle)`).
fn credis_spend_auth(eoa: Address, handle: B256, bundle: Address) -> [u8; 32] {
    let mk = derive_modify_key(&test_enclave::state_key(), eoa).unwrap();
    spend_auth_mac(&pledge_secret(&mk, handle), bundle)
}

/// Storage set up with the block time, sub-call stubs, and the enclave installed.
fn env() -> HashMapStorageProvider {
    test_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    storage.set_block_number(BLOCK_NUMBER);
    storage.enable_sub_call_stub();
    storage.stub_sub_call_at(VAULT_PROVIDER_ADDRESS, zero_word());
    storage.stub_sub_call_at(asset(), iso_word(ISSUANCE_ISO));
    storage
}

#[test]
fn full_pledge_request_pay_unlock_flow() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let pledge_amount = one_e18();
        let installment = pledge_amount / U256::from(NUMBER_OF_ANADOSIS);

        // Mine + pledge. Alice is both the pledger EOA and the bundle account here.
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            pledge_amount,
            auth(GratisOp::Mint, alice(), pledge_amount, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());
        seed_oracle(storage.clone(), U256::from(2u64) * one_e18());
        let handle = gf::pledge_gratis(
            storage.clone(),
            alice(),
            pledge_amount,
            auth(GratisOp::Pledge, alice(), pledge_amount, 1),
        )
        .unwrap();
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(view_pledged(&storage, alice()), pledge_amount);

        // requestCredis bound to alice's bundle account.
        let spend = credis_spend_auth(alice(), handle, alice());
        let (position_id, amount_stables) =
            runtime::request_credis(storage.clone(), alice(), asset(), alice(), handle, spend)
                .unwrap();

        // amount_stables = pledge_amount * 2e18 / (1e12 * 1e18) for rate 2.0.
        let expected_stables = pledge_amount * U256::from(2u64) * one_e18()
            / (U256::from(1_000_000_000_000u128) * one_e18());
        assert_eq!(amount_stables, expected_stables);

        let credis = CredisContract::new(storage.clone());
        let position = credis.get_position(position_id).unwrap();
        assert_eq!(position.bundle_account, alice());
        assert_eq!(position.credis_principal, amount_stables);
        assert_eq!(position.refinancing_rate, refi_rate());
        assert_eq!(position.issuance_currency, ISSUANCE_ISO);
        let multiplier =
            one_e18() + refi_rate() * U256::from(NUMBER_OF_ANADOSIS) / U256::from(12u64);
        assert_eq!(
            position.total_anadosis_amount,
            amount_stables * multiplier / one_e18()
        );
        assert_eq!(position.total_gratis_amount, pledge_amount);

        // Pay each installment; each releases 1/10 of the collateral back to
        // alice's encrypted balance, one installment at a time.
        for n in 1..=NUMBER_OF_ANADOSIS {
            storage
                .set_block_timestamp(U256::from(CREATED_AT + n as u64 * SECONDS_PER_MONTH))
                .unwrap();
            runtime::pay_anadosis(storage.clone(), alice(), position_id).unwrap();

            let unlocked = U256::from(n) * installment;
            assert_eq!(view_balance(&storage, alice()), unlocked, "installment {n}");
            assert_eq!(view_pledged(&storage, alice()), pledge_amount - unlocked);
        }

        // Fully drained: alice holds the whole pledge again.
        assert_eq!(view_balance(&storage, alice()), pledge_amount);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
    });
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
        seed_oracle(storage.clone(), U256::from(2u64) * one_e18());
        let handle = gf::pledge_gratis(
            storage.clone(),
            alice(),
            pledge_amount,
            auth(GratisOp::Pledge, alice(), pledge_amount, 1),
        )
        .unwrap();

        let spend = credis_spend_auth(alice(), handle, alice());
        let (position_id, _) =
            runtime::request_credis(storage.clone(), alice(), asset(), alice(), handle, spend)
                .unwrap();

        // Pay a single installment: unlocks exactly pledge/10 right away, without
        // waiting for the loan to complete.
        storage
            .set_block_timestamp(U256::from(CREATED_AT + SECONDS_PER_MONTH))
            .unwrap();
        runtime::pay_anadosis(storage.clone(), alice(), position_id).unwrap();

        assert_eq!(view_balance(&storage, alice()), installment);
        assert_eq!(view_pledged(&storage, alice()), pledge_amount - installment);
    });
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
        seed_oracle(storage.clone(), U256::from(2u64) * one_e18());

        // First pledge + request.
        let h1 = gf::pledge_gratis(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Pledge, alice(), amount, 1),
        )
        .unwrap();
        let spend1 = credis_spend_auth(alice(), h1, alice());
        runtime::request_credis(storage.clone(), alice(), asset(), alice(), h1, spend1).unwrap();

        // Second pledge — then attempt a second request once anadosis-1 is overdue
        // on the first position.
        let h2 = gf::pledge_gratis(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Pledge, alice(), amount, 2),
        )
        .unwrap();
        let spend2 = credis_spend_auth(alice(), h2, alice());
        storage
            .set_block_timestamp(U256::from(CREATED_AT + SECONDS_PER_MONTH + 1))
            .unwrap();
        let err = runtime::request_credis(storage.clone(), alice(), asset(), alice(), h2, spend2)
            .unwrap_err();
        assert!(err.to_string().contains("overdue"), "got: {err}");
    });
    test_enclave::uninstall();
}

#[test]
fn request_credis_rejects_zero_asset() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let err = runtime::request_credis(
            storage.clone(),
            alice(),
            Address::ZERO,
            alice(),
            B256::ZERO,
            [0u8; 32],
        )
        .unwrap_err();
        assert!(err.to_string().contains("asset"), "got: {err}");
    });
}

#[test]
fn request_credis_rejects_zero_bundle_account() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let err = runtime::request_credis(
            storage.clone(),
            alice(),
            asset(),
            Address::ZERO,
            B256::ZERO,
            [0u8; 32],
        )
        .unwrap_err();
        assert!(err.to_string().contains("bundle account"), "got: {err}");
    });
}

#[test]
fn pay_anadosis_rejects_non_owner_caller() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let amount = one_e18();
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());
        seed_oracle(storage.clone(), U256::from(2u64) * one_e18());
        let handle = gf::pledge_gratis(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Pledge, alice(), amount, 1),
        )
        .unwrap();
        let spend = credis_spend_auth(alice(), handle, alice());
        let (position_id, _) =
            runtime::request_credis(storage.clone(), alice(), asset(), alice(), handle, spend)
                .unwrap();

        // bob is not the position's bundle account.
        let err = runtime::pay_anadosis(storage.clone(), bob(), position_id).unwrap_err();
        assert!(err.to_string().contains("bundleAccount"), "got: {err}");
    });
    test_enclave::uninstall();
}
