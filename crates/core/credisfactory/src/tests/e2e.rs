//! End-to-end flow: mine → pledge → requestCredis → latch → settle → void.
//!
//! The confidential Gratis path runs against the in-process enclave engine
//! (`outbe_gratis::enclave_client::test_enclave`); balances/pledged amounts are
//! asserted by decrypting the ciphertext with the account's view key, exactly as
//! a client would. `HashMapStorageProvider` does not run a real EVM, so the
//! runtime's Rust → Solidity sub-calls into `IVaultRouter` / `IERC20` are
//! stubbed via `enable_sub_call_stub` (returns `default_success()`).
//!
//! There is no price-scan hook yet, so the tests that need a CALLED position
//! drive `mark_called` directly — the daily reference-price scan that fires it in
//! production is a separate change.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;

use crate::precompile::ICredisFactory;
use outbe_credis::{CredisContract, CredisState};
use outbe_fidelity::enclave_client::test_enclave as fidelity_enclave;
use outbe_gratis::enclave_client::test_enclave;
use outbe_gratisfactory::runtime as gf;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::SCALE_1E6_U256;
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

const DAY: u64 = 86_400;

/// Policy rate seeded for USD in these e2e tests (4.30 %, scale 1e6).
fn policy_rate() -> U256 {
    U256::from(43_000u64)
}

/// COEN/840 rate these tests seed: 2.0 at scale 1e6. This is the entry price of
/// every position opened here, so floor = 2.16 and call = 2.64.
fn oracle_rate() -> U256 {
    U256::from(2u64) * SCALE_1E6_U256
}

/// A price above every position's floor (2.16), so the settlement latch trips.
fn above_floor() -> U256 {
    U256::from(2_200_000u64)
}

/// Credit a pledge asks for: $2.00 in 6-decimal minor units. At [`oracle_rate`] that
/// costs exactly [`pledge_cost`] gratis.
fn pledge_stables() -> U256 {
    U256::from(2_000_000u64)
}

/// Gratis collateral [`pledge_stables`] costs: `2e6 * 1e6 / 2e6 = 1e6`.
fn pledge_cost() -> U256 {
    SCALE_1E6_U256
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

/// Pledge and open a position for alice, originated by [`cca`].
fn open(storage: &StorageHandle<'_>, nonce: u64) -> U256 {
    let handle = pledge(storage, alice(), nonce);
    let spend = credis_spend_auth(alice(), handle, alice());
    let (position_id, _) =
        runtime::request_credis(storage.clone(), cca(), alice(), handle, spend).unwrap();
    position_id
}

fn chain_b256() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}

fn seed_oracle(storage: StorageHandle<'_>, coen_iso_rate: U256) {
    outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR).unwrap();
    set_coen_rate(&storage, coen_iso_rate);
    let oracle = OracleContract::new(storage);
    oracle
        .reference_currency_rate
        .write(&ISSUANCE_ISO, policy_rate())
        .unwrap();
}

/// Re-publishes the COEN/840 rate — how these tests move the price across a floor.
fn set_coen_rate(storage: &StorageHandle<'_>, coen_iso_rate: U256) {
    outbe_oracle::api::set_exchange_rate(
        storage.clone(),
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        coen_iso_rate,
        0,
        0,
    )
    .unwrap();
}

fn now_of(storage: &StorageHandle<'_>) -> u64 {
    storage.timestamp().unwrap().to::<u64>()
}

fn advance_to(storage: &StorageHandle<'_>, timestamp: u64) {
    storage.set_block_timestamp(U256::from(timestamp)).unwrap();
}

/// Settles exactly the accrued interest plus `principal` of the outstanding
/// balance, and returns the `(principal, interest)` the settlement reported.
fn settle_principal(
    storage: &StorageHandle<'_>,
    payer: Address,
    position_id: U256,
    principal: U256,
) -> (U256, U256) {
    let position = CredisContract::new(storage.clone())
        .get_position(position_id)
        .unwrap();
    let interest = CredisContract::accrued_interest(&position, now_of(storage)).unwrap();
    runtime::settle(storage.clone(), payer, position_id, interest + principal).unwrap()
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

/// Mints `amount` gratis to alice and seeds the fidelity + oracle state a pledge needs.
fn bootstrap(storage: &StorageHandle<'_>, amount: U256) {
    outbe_gratis::api::mint(
        storage.clone(),
        alice(),
        amount,
        auth(GratisOp::Mint, alice(), amount, 0),
    )
    .unwrap();
    seed_fidelity(storage.clone(), alice());
    seed_oracle(storage.clone(), oracle_rate());
}

fn teardown() {
    fidelity_enclave::uninstall();
    test_enclave::uninstall();
}

#[test]
fn request_credis_seals_the_position_geometry_from_the_pledge_quote() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let handle = pledge(&storage, alice(), 1);
        // Pledge parks the amount in the ticket: balance drained, pledged ledger 0.
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);

        let spend = credis_spend_auth(alice(), handle, alice());
        let (position_id, amount_stables) =
            runtime::request_credis(storage.clone(), cca(), alice(), handle, spend).unwrap();

        // The collateral moved into alice's OWN pledged ledger.
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
        // The loan is exactly what the pledger asked for — credis does not re-price it.
        assert_eq!(amount_stables, pledge_stables());

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.smart_account, alice());
        assert_eq!(position.cca, cca(), "the caller is the originating agent");
        // The pledger EOA is stored sealed (ciphertext), never as a plaintext address,
        // and the enclave opens it back to alice via RevealOwner.
        assert!(!position.eoa_ct.is_empty(), "eoa stored as ciphertext");
        assert_eq!(
            outbe_gratis::api::reveal_owner(storage.clone(), &position.eoa_ct).unwrap(),
            alice()
        );

        assert_eq!(position.principal, amount_stables);
        assert_eq!(position.outstanding, amount_stables);
        assert_eq!(position.collateral, pledge_cost());
        assert_eq!(position.collateral_locked, pledge_cost());
        // The entry price is the rate the PLEDGE was quoted at, not a later read;
        // floor and call derive from it, so the whole geometry follows the quote.
        assert_eq!(position.entry_price, oracle_rate());
        assert_eq!(position.floor_price, U256::from(2_160_000u64));
        assert_eq!(position.call_price, U256::from(2_640_000u64));
        assert_eq!(position.policy_rate, policy_rate());
        assert_eq!(position.issuance_currency, ISSUANCE_ISO);
        assert_eq!(position.lifecycle_state().unwrap(), CredisState::Open);
        assert_eq!(position.last_settled_at, position.originated_at);
    });
    teardown();
}

#[test]
fn settle_is_rejected_until_the_price_crosses_the_floor() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);

        // The seeded price (2.0) sits below the floor (2.16).
        let err = runtime::settle(
            storage.clone(),
            alice(),
            position_id,
            U256::from(1_000_000u64),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not settleable"), "got: {err}");

        // Cross the floor: the settlement latches the position on the way through.
        set_coen_rate(&storage, above_floor());
        settle_principal(&storage, alice(), position_id, U256::from(500_000u64));
        assert_eq!(
            CredisContract::new(storage.clone())
                .get_position(position_id)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            CredisState::Settleable
        );

        // The latch is one-way: the price falling back below the floor does not
        // re-lock the position.
        set_coen_rate(&storage, oracle_rate());
        settle_principal(&storage, alice(), position_id, U256::from(500_000u64));
        assert_eq!(
            CredisContract::new(storage.clone())
                .get_position(position_id)
                .unwrap()
                .outstanding,
            U256::from(1_000_000u64)
        );
    });
    teardown();
}

#[test]
fn settlement_releases_collateral_proportionally_and_closes_without_dust() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        set_coen_rate(&storage, above_floor());

        // Half the principal, 30 days in → half the collateral back.
        advance_to(&storage, CREATED_AT + 30 * DAY);
        let half = pledge_stables() / U256::from(2u64);
        let (principal_paid, interest_paid) =
            settle_principal(&storage, alice(), position_id, half);

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.outstanding, half);
        assert_eq!(position.collateral_locked, pledge_cost() / U256::from(2u64));
        // The two components are reported separately: the principal is exactly
        // what was asked for, and the interest rode on top of it.
        assert_eq!(principal_paid, half);
        assert!(
            !interest_paid.is_zero(),
            "interest was collected on top of the principal"
        );

        assert_eq!(
            view_balance(&storage, alice()),
            pledge_cost() / U256::from(2u64)
        );
        assert_eq!(
            view_pledged(&storage, alice()),
            pledge_cost() / U256::from(2u64)
        );

        // Settling the rest returns the whole pledge and closes the position with
        // nothing stranded in the pledged ledger.
        advance_to(&storage, CREATED_AT + 60 * DAY);
        settle_principal(&storage, alice(), position_id, half);

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.lifecycle_state().unwrap(), CredisState::Settled);
        assert!(position.collateral_locked.is_zero());
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
    });
    teardown();
}

#[test]
fn the_settle_abi_returns_the_principal_and_interest_split() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        set_coen_rate(&storage, above_floor());
        advance_to(&storage, CREATED_AT + 30 * DAY);

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        let interest = CredisContract::accrued_interest(&position, now_of(&storage)).unwrap();
        assert!(!interest.is_zero(), "30 days must have accrued something");
        let principal = pledge_stables() / U256::from(4u64);

        // Drive the real ABI path, so the two-field return is exercised through
        // encoding and decoding rather than only as a Rust tuple.
        let data = ICredisFactory::settleCall {
            positionId: position_id,
            amount: interest + principal,
        }
        .abi_encode();
        let out = crate::precompile::dispatch(storage.clone(), &data, alice(), U256::ZERO).unwrap();
        let decoded = ICredisFactory::settleCall::abi_decode_returns(&out).unwrap();

        // Order matters: principal first, interest second.
        assert_eq!(decoded.principal, principal);
        assert_eq!(decoded.interest, interest);

        let after = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(
            after.outstanding,
            pledge_stables() - principal,
            "only the principal component reduces the balance"
        );
    });
    teardown();
}

#[test]
fn settle_takes_only_what_the_position_needs() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        set_coen_rate(&storage, above_floor());
        advance_to(&storage, CREATED_AT + 30 * DAY);

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        let interest = CredisContract::accrued_interest(&position, now_of(&storage)).unwrap();

        let (principal_paid, interest_paid) = runtime::settle(
            storage.clone(),
            alice(),
            position_id,
            pledge_stables() * U256::from(1_000u64),
        )
        .unwrap();
        // The split is reported separately, and only what the position needed
        // was pulled — the vast over-payment is not.
        assert_eq!(principal_paid, pledge_stables());
        assert_eq!(interest_paid, interest);
        assert_eq!(
            principal_paid + interest_paid,
            interest + pledge_stables(),
            "only interest + outstanding principal is pulled"
        );
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
    });
    teardown();
}

#[test]
fn settle_accepts_a_third_party_payer() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        set_coen_rate(&storage, above_floor());

        // bob is neither the pledger nor the smart account, but anyone may settle.
        let half = pledge_stables() / U256::from(2u64);
        settle_principal(&storage, bob(), position_id, half);

        // The freed collateral goes to the ORIGINAL pledger, never to the payer — this
        // is what makes an open payer safe without an access check.
        assert_eq!(
            view_balance(&storage, alice()),
            pledge_cost() / U256::from(2u64)
        );
        assert_eq!(
            view_pledged(&storage, alice()),
            pledge_cost() / U256::from(2u64)
        );
        assert_eq!(view_balance(&storage, bob()), U256::ZERO);
        assert_eq!(view_pledged(&storage, bob()), U256::ZERO);
    });
    teardown();
}

#[test]
fn request_credis_rejects_an_owner_with_an_unresolved_call() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        // Both pledges are quoted at the seeded rate, before the price moves.
        let first_handle = pledge(&storage, alice(), 1);
        let second_handle = pledge(&storage, alice(), 2);

        let first_spend = credis_spend_auth(alice(), first_handle, alice());
        let (first, _) =
            runtime::request_credis(storage.clone(), cca(), alice(), first_handle, first_spend)
                .unwrap();

        // Latch and call the first position.
        set_coen_rate(&storage, above_floor());
        {
            let mut credis = CredisContract::new(storage.clone());
            assert!(credis.mark_settleable(first).unwrap());
            assert!(credis.mark_called(first, now_of(&storage)).unwrap());
        }

        let spend = credis_spend_auth(alice(), second_handle, alice());
        let err = runtime::request_credis(storage.clone(), cca(), alice(), second_handle, spend)
            .unwrap_err();
        assert!(err.to_string().contains("called position"), "got: {err}");

        // Settling the call in full clears the block.
        settle_principal(&storage, alice(), first, pledge_stables());
        runtime::request_credis(storage.clone(), cca(), alice(), second_handle, spend).unwrap();
    });
    teardown();
}

#[test]
fn request_credis_rejects_zero_smart_account() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let err =
            runtime::request_credis(storage.clone(), cca(), Address::ZERO, B256::ZERO, [0u8; 32])
                .unwrap_err();
        assert!(err.to_string().contains("smart account"), "got: {err}");
    });
}

#[test]
fn void_sweep_burns_only_the_unpaid_share() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        set_coen_rate(&storage, above_floor());

        // Settle half the principal, reclaiming half the collateral.
        advance_to(&storage, CREATED_AT + 30 * DAY);
        settle_principal(
            &storage,
            alice(),
            position_id,
            pledge_stables() / U256::from(2u64),
        );
        let unpaid_collateral = pledge_cost() / U256::from(2u64);
        assert_eq!(view_pledged(&storage, alice()), unpaid_collateral);

        // Call it, then let the settlement window lapse.
        let called_at = now_of(&storage);
        {
            let mut credis = CredisContract::new(storage.clone());
            assert!(credis.mark_called(position_id, called_at).unwrap());
        }

        // Encrypted cohort ledger before the burn — the burn records a sale
        // cohort, so the ciphertext must change afterwards.
        let cohorts_before = outbe_fidelity::FidelityContract::new(storage.clone())
            .cohorts_ct_of(alice())
            .unwrap();
        assert!(!cohorts_before.is_empty(), "alice has a seeded cohort");

        // One second inside the window the sweep must find nothing.
        let deadline = called_at + 14 * DAY;
        advance_to(&storage, deadline - 1);
        assert_eq!(scan(&storage, deadline - 1), 0, "window still open");

        advance_to(&storage, deadline);
        assert_eq!(scan(&storage, deadline), 1);

        // Only the unpaid share was burned; the settled half stays with alice.
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(view_balance(&storage, alice()), unpaid_collateral);
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            pledge_cost() - unpaid_collateral
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
            unpaid_collateral
        );

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.lifecycle_state().unwrap(), CredisState::Void);
        assert!(position.outstanding.is_zero());
        assert!(position.collateral_locked.is_zero());

        // A sale cohort was recorded for the burned collateral, so alice's
        // encrypted cohort ledger changed (the RCFI-drop semantics itself is
        // covered by the fidelity + enclave tests).
        let cohorts_after = outbe_fidelity::FidelityContract::new(storage.clone())
            .cohorts_ct_of(alice())
            .unwrap();
        assert_ne!(
            cohorts_before, cohorts_after,
            "the void burn should record a sale cohort"
        );

        // Idempotent: a second sweep at the same height finds nothing to burn.
        assert_eq!(scan(&storage, deadline), 0);
    });
    teardown();
}

#[test]
fn a_position_settled_inside_the_window_is_never_voided() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        set_coen_rate(&storage, above_floor());

        let called_at = now_of(&storage);
        {
            let mut credis = CredisContract::new(storage.clone());
            assert!(credis.mark_settleable(position_id).unwrap());
            assert!(credis.mark_called(position_id, called_at).unwrap());
        }

        // Settle in full inside the window.
        advance_to(&storage, called_at + 7 * DAY);
        settle_principal(&storage, alice(), position_id, pledge_stables());

        let deadline = called_at + 14 * DAY;
        advance_to(&storage, deadline + DAY);
        assert_eq!(scan(&storage, deadline + DAY), 0);

        // Nothing burned: the whole pledge is back with alice.
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            pledge_cost()
        );
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            U256::ZERO
        );
    });
    teardown();
}

/// Runs the begin-block void sweep at `timestamp`, returning how many positions
/// it voided.
fn scan(storage: &StorageHandle<'_>, timestamp: u64) -> u32 {
    let ctx = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(BLOCK_NUMBER, timestamp, CHAIN_ID),
        storage.clone(),
    );
    crate::lifecycle::scan_and_void(&ctx).unwrap()
}
