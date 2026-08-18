//! Shared test harness for the credisfactory crate.
//!
//! The confidential Gratis path runs against the in-process enclave engine
//! (`outbe_gratis::enclave_client::test_enclave`); balances/pledged amounts are
//! asserted by decrypting the ciphertext with the account's view key, exactly as
//! a client would. `HashMapStorageProvider` does not run a real EVM, so the
//! runtime's Rust → Solidity sub-calls into `IVaultRouter` / `IERC20` are
//! stubbed via `enable_sub_call_stub` (returns `default_success()`).

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;

use outbe_credis::CredisContract;
use outbe_fidelity::enclave_client::test_enclave as fidelity_enclave;
use outbe_gratis::enclave_client::test_enclave;
use outbe_gratisfactory::runtime as gf;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};
use outbe_tee::protocol::{GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{
    decrypt_balance, decrypt_pledged, derive_modify_key, derive_view_key, modify_mac,
    pledge_secret, spend_auth_mac,
};

use crate::runtime;

pub const CHAIN_ID: u64 = 1;
pub const CREATED_AT: u64 = 1_700_000_000;
pub const BLOCK_NUMBER: u64 = 42;

/// Issuance currency (ISO 4217) reported by `asset()`'s stubbed `isoCode()`.
pub const ISSUANCE_ISO: u16 = 840;

/// A second ISO currency, for the multi-currency scan tests.
pub const EUR: u16 = 978;

pub const DAY: u64 = 86_400;

pub fn alice() -> Address {
    address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
}

pub fn bob() -> Address {
    address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB")
}

pub fn asset() -> Address {
    address!("0x0000000000000000000000000000000000000888")
}

/// The originating agent. `requestCredis`'s caller is recorded on the position.
pub fn cca() -> Address {
    address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC")
}

/// Policy rate seeded for USD in these tests (4.30 %, 1e18 scaled).
pub fn policy_rate() -> U256 {
    U256::from(43_000_000_000_000_000u128)
}

pub fn one_e18() -> U256 {
    U256::from(10u64).pow(U256::from(18u64))
}

/// COEN/840 rate these tests seed: 2.0, 1e18-scaled. This is the entry price of
/// every position opened here, so floor = 2.16 and call = 2.64.
pub fn oracle_rate() -> U256 {
    U256::from(2u64) * one_e18()
}

/// A price above every position's floor (2.16), so the settlement latch trips.
pub fn above_floor() -> U256 {
    U256::from(2_200_000_000_000_000_000u64)
}

/// A price at or above every position's call price (2.64).
pub fn at_call() -> U256 {
    U256::from(2_640_000_000_000_000_000u64)
}

/// Credit a pledge asks for: $2.00 in 6-decimal minor units. At [`oracle_rate`] that
/// costs exactly [`pledge_cost`] gratis.
pub fn pledge_stables() -> U256 {
    U256::from(2_000_000u64)
}

/// Gratis collateral [`pledge_stables`] costs: `2e6 * 1e12 * 1e18 / 2e18 = 1e18`.
pub fn pledge_cost() -> U256 {
    one_e18()
}

/// Pledge [`pledge_stables`] of credit for `who` at op-nonce `nonce` (uncapped), and
/// return the resulting handle. The gratis it costs is derived from the seeded rate.
pub fn pledge(storage: &StorageHandle<'_>, who: Address, nonce: u64) -> B256 {
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

/// Pledge `amount_stables` of credit for `who`. Unlike [`pledge`] this does not
/// pin the gratis cost, so a test can ask for an amount above the §8.3 dust
/// guard — the standard [`pledge_stables`] fixture is only $2.00.
pub fn pledge_amount(
    storage: &StorageHandle<'_>,
    who: Address,
    amount_stables: U256,
    nonce: u64,
) -> B256 {
    let (handle, _) = gf::pledge_gratis(
        storage.clone(),
        who,
        amount_stables,
        asset(),
        U256::MAX,
        auth(GratisOp::Pledge, who, amount_stables, nonce),
    )
    .unwrap();
    handle
}

/// Gratis that `amount_stables` of credit costs at the seeded rate:
/// `stables × 1e12 × 1e18 / 2e18`.
pub fn gratis_for(amount_stables: U256) -> U256 {
    amount_stables * U256::from(500_000_000_000u64)
}

/// Pledge and open a position for alice, originated by [`cca`].
pub fn open(storage: &StorageHandle<'_>, nonce: u64) -> U256 {
    open_for(storage, alice(), nonce)
}

/// Pledge and open a position for `who`, originated by [`cca`].
pub fn open_for(storage: &StorageHandle<'_>, who: Address, nonce: u64) -> U256 {
    let handle = pledge(storage, who, nonce);
    let spend = credis_spend_auth(who, handle, who);
    let (position_id, _) =
        runtime::request_credis(storage.clone(), cca(), who, handle, spend).unwrap();
    position_id
}

pub fn chain_b256() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}

/// Idempotent, like genesis: the COEN/840 pair is registered once and re-seeding
/// only republishes the rate.
pub fn seed_oracle(storage: StorageHandle<'_>, rate_1e18: U256) {
    if outbe_oracle::api::coen_pair_index_opt(storage.clone(), ISSUANCE_ISO)
        .unwrap()
        .is_none()
    {
        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
    }
    set_coen_rate(&storage, rate_1e18);
    let oracle = OracleContract::new(storage);
    oracle
        .reference_currency_rate
        .write(&ISSUANCE_ISO, policy_rate())
        .unwrap();
}

/// Re-publishes the live COEN/840 spot rate — how these tests move the price
/// across a floor for the on-demand latch inside `settle`.
pub fn set_coen_rate(storage: &StorageHandle<'_>, rate_1e18: U256) {
    outbe_oracle::api::set_exchange_rate(
        storage.clone(),
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        rate_1e18,
        0,
        0,
    )
    .unwrap();
}

// -------------------------------------------------------------------------
// Finalized daily reference series — what the daily scan actually reads
// -------------------------------------------------------------------------

/// The most recent fully-closed UTC day at `timestamp`, i.e. the day the scan
/// treats as the newest data point.
pub fn last_closed_day(timestamp: u64) -> u32 {
    previous_date_key(timestamp_to_date_key(timestamp))
}

/// Advances the finalization watermark to cover the day closed at `timestamp`
/// without publishing a price for any day. The scan then runs but finds no
/// reference series, so only price-independent transitions (the void) apply.
pub fn finalize_through(storage: &StorageHandle<'_>, timestamp: u64) {
    bump_watermark(storage, last_closed_day(timestamp));
}

/// Publishes a finalized daily reference price for one UTC day, in USD.
pub fn set_vwap(storage: &StorageHandle<'_>, utc_day: u32, value: U256) {
    set_vwap_for(storage, ISSUANCE_ISO, utc_day, value);
}

/// Publishes a finalized daily reference price for one UTC day in the COEN/`iso`
/// series. The pair must already be registered.
pub fn set_vwap_for(storage: &StorageHandle<'_>, iso: u16, utc_day: u32, value: U256) {
    let oracle = OracleContract::new(storage.clone());
    let index = oracle
        .pair_index_of(outbe_oracle::api::AddressPair::new_coen_to(iso))
        .unwrap();
    oracle
        .utc_day_vwap_value
        .get_nested(&utc_day)
        .write(&index, value)
        .unwrap();
    bump_watermark(storage, utc_day);
}

/// Registers the COEN/`iso` pair so that currency has its own daily series.
pub fn register_currency(storage: &StorageHandle<'_>, iso: u16) {
    outbe_oracle::api::register_pair(
        storage.clone(),
        outbe_oracle::api::AddressPair::new_coen_to(iso),
    )
    .unwrap();
}

/// Sets `days` consecutive closed UTC days ending at `latest` to `value`.
pub fn fill_days(storage: &StorageHandle<'_>, latest: u32, days: u32, value: U256) {
    let mut day = latest;
    for _ in 0..days {
        set_vwap(storage, day, value);
        day = previous_date_key(day);
    }
}

/// Mirrors the oracle begin-block hook: the watermark covers every seeded day.
fn bump_watermark(storage: &StorageHandle<'_>, utc_day: u32) {
    let oracle = OracleContract::new(storage.clone());
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < utc_day {
        oracle.utc_day_vwap_last_finalized.write(utc_day).unwrap();
    }
}

/// Runs the daily price-path scan at `timestamp`, returning how many positions
/// it moved.
pub fn scan(storage: &StorageHandle<'_>, timestamp: u64) -> u32 {
    let ctx = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(BLOCK_NUMBER, timestamp, CHAIN_ID),
        storage.clone(),
    );
    crate::called::scan_and_call(&ctx).unwrap()
}

pub fn now_of(storage: &StorageHandle<'_>) -> u64 {
    storage.timestamp().unwrap().to::<u64>()
}

pub fn advance_to(storage: &StorageHandle<'_>, timestamp: u64) {
    storage.set_block_timestamp(U256::from(timestamp)).unwrap();
}

/// Settles exactly the accrued interest plus `principal` of the outstanding
/// balance, and returns what was actually pulled from `payer`.
pub fn settle_principal(
    storage: &StorageHandle<'_>,
    payer: Address,
    position_id: U256,
    principal: U256,
) -> U256 {
    let position = CredisContract::new(storage.clone())
        .get_position(position_id)
        .unwrap();
    let interest = CredisContract::accrued_interest(&position, now_of(storage)).unwrap();
    runtime::settle(storage.clone(), payer, position_id, interest + principal).unwrap()
}

/// ABI-encoded `uint16` return for the asset's `isoCode()` static sub-call.
pub fn iso_word(iso: u16) -> Bytes {
    let mut b = vec![0u8; 32];
    b[30..32].copy_from_slice(&iso.to_be_bytes());
    Bytes::from(b)
}

/// 32-byte zero word — the stubbed `uint256` return for the vault sub-calls.
pub fn zero_word() -> Bytes {
    Bytes::from(vec![0u8; 32])
}

/// ABI-encoded `uint256` return, for the asset's `balanceOf()` static sub-call.
pub fn u256_word(value: U256) -> Bytes {
    Bytes::from(value.to_be_bytes::<32>().to_vec())
}

/// `IERC20.balanceOf(address)` selector.
pub const BALANCE_OF_SELECTOR: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];

/// Sets what the asset reports as `smart_account`'s stablecoin balance, which is
/// what the §7 matched-funding check reads.
pub fn set_matched_funds(storage: &mut HashMapStorageProvider, amount: U256) {
    storage.stub_sub_call_at_selector(asset(), BALANCE_OF_SELECTOR, u256_word(amount));
}

/// Positive Fidelity so `gratisfactory::pledge_gratis` clears the eligibility gate.
pub fn seed_fidelity(storage: StorageHandle<'_>, account: Address) {
    const ONE_YEAR_SECS: u64 = 365 * 86_400;
    outbe_fidelity::api::cohort_in(
        storage,
        account,
        U256::from(100u64),
        CREATED_AT - ONE_YEAR_SECS,
    )
    .unwrap();
}

pub fn auth(op: GratisOp, owner: Address, amount: U256, op_nonce: u64) -> ModifyAuth {
    let mk = derive_modify_key(&test_enclave::state_key(), owner).unwrap();
    ModifyAuth {
        mac: modify_mac(&mk, owner, op, amount, op_nonce, chain_b256()),
        op_nonce,
    }
}

pub fn view_balance(s: &StorageHandle<'_>, a: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), a).unwrap();
    let blob = outbe_gratis::api::balance_ct(s.clone(), a).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_balance(&vk, a, &blob).unwrap()
}

pub fn view_pledged(s: &StorageHandle<'_>, a: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), a).unwrap();
    let blob = outbe_gratis::api::pledged_ct(s.clone(), a).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_pledged(&vk, a, &blob).unwrap()
}

/// The spend authorization the pledger EOA hands to the CCA to bind a pledge to a
/// destination smart account (`HMAC(pledgeSecret, "credis-bind" || bundle)`).
pub fn credis_spend_auth(eoa: Address, handle: B256, bundle: Address) -> [u8; 32] {
    let mk = derive_modify_key(&test_enclave::state_key(), eoa).unwrap();
    spend_auth_mac(&pledge_secret(&mk, handle), bundle)
}

/// Storage set up with the block time, sub-call stubs, and the enclave installed.
pub fn env() -> HashMapStorageProvider {
    test_enclave::install();
    fidelity_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    storage.set_block_number(BLOCK_NUMBER);
    storage.enable_sub_call_stub();
    storage.stub_sub_call_at(VAULT_ROUTER_ADDRESS, zero_word());
    // A funded reserve vault. `pledge_gratis` claims its credit up front and
    // `request_credis` delivers that claim, so both legs must answer; the
    // address-wide zero word above is a valid `uint256` for either.
    for selector in [
        outbe_vaultrouter::api::IVaultRouter::reserveCall::SELECTOR,
        outbe_vaultrouter::api::IVaultRouter::releaseReservationCall::SELECTOR,
        outbe_vaultrouter::api::IVaultRouter::returnReservationCall::SELECTOR,
    ] {
        storage.stub_sub_call_at_selector(
            VAULT_ROUTER_ADDRESS,
            selector,
            u256_word(pledge_stables()),
        );
    }
    storage.stub_sub_call_at(asset(), iso_word(ISSUANCE_ISO));
    // Matched funding defaults to satisfied; the tests that exercise §7 override
    // it. The selector stub takes priority over the address-wide `isoCode()` one.
    set_matched_funds(&mut storage, pledge_stables());
    storage
}

/// Mints `amount` gratis to alice and seeds the fidelity + oracle state a pledge needs.
pub fn bootstrap(storage: &StorageHandle<'_>, amount: U256) {
    bootstrap_for(storage, alice(), amount);
}

/// Mints `amount` gratis to `who` and seeds the fidelity + oracle state a pledge
/// needs, plus the CCA registration `requestCredis` gates on.
pub fn bootstrap_for(storage: &StorageHandle<'_>, who: Address, amount: U256) {
    outbe_gratis::api::mint(
        storage.clone(),
        who,
        amount,
        auth(GratisOp::Mint, who, amount, 0),
    )
    .unwrap();
    seed_fidelity(storage.clone(), who);
    seed_oracle(storage.clone(), oracle_rate());
    register_cca(storage, cca());
}

/// Registers `agent` so it clears the §8.4 origination gate. Idempotent.
pub fn register_cca(storage: &StorageHandle<'_>, agent: Address) {
    let mut registry = outbe_cca::CcaContract::new(storage.clone());
    if !registry.can_originate(agent).unwrap() {
        registry.register(agent, now_of(storage)).unwrap();
    }
}

/// The originating agent's performance multiplier, 1e18 scaled.
pub fn cca_multiplier(storage: &StorageHandle<'_>, agent: Address) -> U256 {
    outbe_cca::api::multiplier_of(storage.clone(), agent).unwrap()
}

pub fn teardown() {
    fidelity_enclave::uninstall();
    test_enclave::uninstall();
}
