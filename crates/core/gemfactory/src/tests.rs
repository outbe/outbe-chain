use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_gem::{api as gem_api, GemContract, GemState};
use outbe_intex::SeriesId;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key, WorldwideDay};
use outbe_promisfactory::api::ModifyAuth;
use outbe_tee::protocol::PromisOp;
use outbe_tee_enclave::promis::{decrypt_balance, derive_modify_key, derive_view_key, modify_mac};

use outbe_primitives::block::{BlockContext, BlockRuntimeContext};

const POSITION_VALIDITY_SECONDS: u64 = outbe_gem::GemParams::PROD.position_validity;
use crate::expired;
use crate::runtime;
use crate::schema::{GemFactoryContract, GemPosition, GemTypes};
use crate::sol_ext::{IReferenceCurrency, IERC20};
use alloy_sol_types::SolCall;
use outbe_vaultrouter::api::IVaultRouter;

const T_NOW: u64 = 1_700_000_000;
const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
const BOB: Address = address!("0x2222222222222222222222222222222222222222");
/// Mock settlement stablecoin passed to `settle_gem` in tests. Its `isoCode()` is
/// stubbed to 840 (USD), matching the default test currency.
const STABLE: Address = address!("0x00000000000000000000000000000000000000AA");
/// Mock stablecoin whose `isoCode()` is 978 (EUR): a currency mismatch for a
/// USD-denominated gem.
const STABLE_EUR: Address = address!("0x00000000000000000000000000000000000000BB");
/// Mock USD stablecoin carrying eighteen decimals instead of six.
const STABLE_18: Address = address!("0x00000000000000000000000000000000000000CC");

/// A no-op authorization for mine paths that reject before reaching the (enclave)
/// Promis mint (ownership/state/PoW failures).
fn no_auth() -> ModifyAuth {
    ModifyAuth {
        mac: [0u8; 32],
        op_nonce: 0,
    }
}

/// The Promis modify authorization for `account`'s first mint of `amount`. Requires
/// the in-process Promis enclave to be installed (chain id 1 matches the harness).
fn promis_auth(account: Address, amount: U256, nonce: u64) -> ModifyAuth {
    let sk = outbe_promis::enclave_client::test_enclave::state_key();
    let mk = derive_modify_key(&sk, account).unwrap();
    ModifyAuth {
        mac: modify_mac(
            &mk,
            account,
            PromisOp::Mint,
            amount,
            nonce,
            B256::from(U256::from(1u64)),
        ),
        op_nonce: nonce,
    }
}

/// Units the stubbed `sendToGemFactory` reports as burned (its `uint256` return).
const SENT_UNITS: u64 = 100;

fn word(value: u64) -> alloy_primitives::Bytes {
    alloy_primitives::Bytes::from(U256::from(value).to_be_bytes::<32>().to_vec())
}

/// Stubs one settlement stablecoin's `isoCode()` and `decimals()`.
fn stub_stablecoin(
    storage: &mut HashMapStorageProvider,
    asset: Address,
    iso_code: u64,
    decimals: u64,
) {
    storage.stub_sub_call_at_selector(
        asset,
        IReferenceCurrency::isoCodeCall::SELECTOR,
        word(iso_code),
    );
    storage.stub_sub_call_at_selector(asset, IERC20::decimalsCall::SELECTOR, word(decimals));
    // The deposit path pulls and approves before handing over to the router.
    storage.stub_sub_call_at_selector(asset, IERC20::transferFromCall::SELECTOR, word(1));
    storage.stub_sub_call_at_selector(asset, IERC20::approveCall::SELECTOR, word(1));
    // A fixed stub cannot vary between the two reads, so the delta is zero here.
    storage.stub_sub_call_at_selector(asset, IERC20::balanceOfCall::SELECTOR, word(0));
}

fn test_storage(rate: Option<U256>) -> HashMapStorageProvider {
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    // Stub IntexNFT1155: `sendToGemFactory` returns SENT_UNITS (32-byte uint256).
    storage.stub_sub_call_at(
        outbe_primitives::addresses::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(U256::from(SENT_UNITS).to_be_bytes::<32>().to_vec()),
    );
    // Answered per selector, so the settlement path can tell USD from EUR.
    stub_stablecoin(&mut storage, STABLE, 840, 6);
    stub_stablecoin(&mut storage, STABLE_EUR, 978, 6);
    stub_stablecoin(&mut storage, STABLE_18, 840, 18);
    // Every asset the tests pass in has a registered vault.
    storage.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall::SELECTOR,
        word(1),
    );
    // `deposit` reports minted shares.
    storage.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::depositCall::SELECTOR,
        word(1),
    );
    StorageHandle::enter(&mut storage, |handle| {
        // These cases assert the PROD gem terms. An unset profile would resolve
        // by chain id, and the test chain is not mainnet.
        outbe_gem::schema::GemContract::new(handle.clone())
            .config_profile
            .write(outbe_gem::config::PROFILE_PROD)
            .unwrap();
        // Registry membership is independent of whether a price exists: 840 is a
        // reference currency in every fixture, priced or not.
        let oracle = OracleContract::new(handle.clone());
        oracle.reference_currencies.push(840u16).unwrap();
        oracle.config_lookback_duration.write(86_400).unwrap();
        if let Some(rate) = rate {
            outbe_oracle::api::register_pair(handle.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
                .unwrap();
            outbe_oracle::api::set_exchange_rate(
                handle.clone(),
                Address::ZERO,
                outbe_oracle::api::DAY_TYPE_PAIR,
                rate,
                1,
                T_NOW,
            )
            .unwrap();
        }
    });
    storage
}

fn with_storage<R>(rate: Option<U256>, f: impl FnOnce(&StorageHandle) -> R) -> R {
    let mut storage = test_storage(rate);
    StorageHandle::enter(&mut storage, |handle| f(&handle))
}

fn six_decimal_unit() -> U256 {
    U256::from(1_000_000u64)
}

fn err_msg<T>(r: outbe_primitives::error::Result<T>) -> String {
    format!("{:?}", r.err().unwrap())
}

/// Brute-force the lowest nonce that satisfies `validate_pow(gem_id, owner, _)`
/// for the current `POW_DIFFICULTY`. With difficulty=1 the expected loop length
/// is ~256 iterations.
fn find_valid_nonce(gem_id: U256, owner: Address) -> u64 {
    for nonce in 0u64..u64::MAX {
        if runtime::validate_pow(gem_id, owner, nonce).is_ok() {
            return nonce;
        }
    }
    panic!("no valid nonce found")
}

/// Issues at the fixture's live COEN/reference rate. The production caller
/// resolves the price for the gem's own day; these tests only need a price that
/// matches the rate the fixture published.
fn issue_at_live_rate(
    storage: &StorageHandle<'_>,
    owner: Address,
    gem_type: GemTypes,
    promis_load: U256,
    issuance_currency: u16,
    reference_currency: u16,
) -> outbe_primitives::error::Result<U256> {
    let price = outbe_oracle::api::fresh_coen_rate_for(storage.clone(), reference_currency)?;
    runtime::issue_gem(
        storage,
        owner,
        gem_type,
        promis_load,
        issuance_currency,
        reference_currency,
        price,
    )
}

/// Pays `gem_id` by ERC20 at `asset`'s quote and returns what it quoted. The
/// stubbed token moves no balance, so an admitted payment stops at the delta check.
fn admitted_at_quote(storage: &StorageHandle<'_>, gem_id: U256, asset: Address) -> (u16, U256) {
    let (currency, amount, snapshot) = runtime::quote_settlement(storage, gem_id, asset).unwrap();
    let res = runtime::settle_gem(storage, BOB, gem_id, asset, snapshot);
    assert!(err_msg(res).contains("unexpected amount"));
    assert_eq!(
        gem_api::get_gem(storage, gem_id).unwrap().unwrap().state,
        GemState::Issued as u8
    );
    (currency, amount)
}

/// Close the gem's first full day above its floor, which qualifies it.
fn seed_qualifying_day(storage: &StorageHandle<'_>, gem_id: U256) {
    let item = gem_api::get_gem(storage, gem_id).unwrap().unwrap();
    let oracle = OracleContract::new(storage.clone());
    let pair = oracle
        .pair_index_of(outbe_oracle::api::AddressPair::new_coen_to(
            item.reference_currency,
        ))
        .unwrap();
    let day = outbe_primitives::time::first_full_day(item.issued_at);
    oracle
        .record_utc_day_vwap(day, pair, item.floor_price_minor + U256::ONE)
        .unwrap();
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < day {
        oracle.utc_day_vwap_last_finalized.write(day).unwrap();
    }
}

/// Registers and prices `COEN/<iso>` and adds `iso` to the reference registry.
/// Registers `COEN/<iso>`, prices it at `rate` both live and for the last closed
/// day, and adds `iso` to the reference registry.
fn register_currency(storage: &StorageHandle<'_>, iso: u16, rate: U256) {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(iso);
    outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
    outbe_oracle::api::set_exchange_rate(storage.clone(), Address::ZERO, pair, rate, 1, T_NOW)
        .unwrap();
    OracleContract::new(storage.clone())
        .reference_currencies
        .push(iso)
        .unwrap();
    seed_day_vwap(storage, iso, rate);
}

// TODO(reserve-config): the paid `settle_gem` path (Reserve vault deposit)
// is not exercisable in the storage-only harness for ANY gem type now that
// Genesis also carries a non-zero cost. Unit coverage forces `Settled` via
// `gem_api::set_state` to reach the mine path. Localnet covers the real paid
// settle with a configured `RESERVE_ASSET` / `RESERVE_VAULT`.

// --- Merchant gems ---

fn source_intex_id() -> SeriesId {
    SeriesId::pack(WorldwideDay::new(20_260_212), *b"USD", b'U').unwrap()
}

fn six_decimal_u128() -> u128 {
    1_000_000
}

/// Whole-position capacity for a series with `promis_load` per unit: the stubbed
/// `sendToGemFactory` burns `SENT_UNITS`, so capacity = `promis_load x SENT_UNITS`.
fn sent_capacity(promis_load: u128) -> U256 {
    U256::from(promis_load) * U256::from(SENT_UNITS)
}

fn seed_source_series(
    storage: &StorageHandle,
    entry: U256,
    floor: U256,
    promis_load: u128,
    call_trigger: outbe_intex::IntexCallTrigger,
) {
    outbe_intex::api::create_series(
        storage,
        outbe_intex::CreateSeriesParams {
            series_id: source_intex_id(),
            worldwide_day: WorldwideDay::new(0),
            issued_units: SENT_UNITS as u32,
            promis_load_minor: promis_load,
            entry_price_minor: entry,
            floor_price_minor: floor,
            call_price_minor: U256::ZERO,
            call_trigger,
            issued_at: T_NOW as u32,
            issuance_currency: 840,
            reference_currency: 840,
        },
    )
    .unwrap();
}

/// Seed an Intex series and send the merchant's whole holding into a GemPosition
/// NFT (burn stubbed via `with_storage`). Returns the `position_id`.
fn seed_and_send(storage: &StorageHandle, entry: U256, floor: U256, promis_load: u128) -> U256 {
    seed_source_series(
        storage,
        entry,
        floor,
        promis_load,
        outbe_intex::IntexCallTrigger::default(),
    );
    runtime::issue_gem_position(storage, ALICE, source_intex_id(), U256::from(SENT_UNITS)).unwrap()
}

const SOURCE_NOTICE_SECONDS: u32 = 3_600;

/// Seeds a source series with a notice period and calls it at `called_at`.
fn seed_called_source(storage: &StorageHandle, called_at: u64) {
    seed_source_series(
        storage,
        six_decimal_unit(),
        six_decimal_unit(),
        six_decimal_u128(),
        outbe_intex::IntexCallTrigger {
            call_notice_period_seconds: SOURCE_NOTICE_SECONDS,
            ..Default::default()
        },
    );
    outbe_intex::api::mark_called(storage, source_intex_id(), called_at as u32).unwrap();
}

fn send_whole_holding(storage: &StorageHandle) -> outbe_primitives::error::Result<U256> {
    runtime::issue_gem_position(storage, ALICE, source_intex_id(), U256::from(SENT_UNITS))
}

/// The refusal leaves storage and events as they were, before any burn.
fn assert_called_source_cannot_be_sent(called_at: u64) {
    let mut provider = test_storage(None);
    // An empty burn answer fails to decode, so a burn before the state check would show.
    provider.stub_sub_call_at(
        outbe_primitives::addresses::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::new(),
    );
    StorageHandle::enter(&mut provider, |storage| {
        seed_called_source(&storage, called_at)
    });
    let before = provider.storage.clone();
    let events = provider.get_ordered_events().to_vec();
    StorageHandle::enter(&mut provider, |storage| {
        assert!(err_msg(send_whole_holding(&storage)).contains("source intex is not issued"));
    });
    assert_eq!(provider.storage, before);
    assert_eq!(provider.get_ordered_events(), events.as_slice());
}

fn block_ctx<'a>(storage: &StorageHandle<'a>, now: u64) -> BlockRuntimeContext<'a> {
    BlockRuntimeContext::new(BlockContext::empty_for_tests(1, now, 1), storage.clone())
}

fn unallocated(storage: &StorageHandle) -> U256 {
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .get_total_unallocated()
        .unwrap()
}

/// Publishes `vwap` as the finalized COEN/`iso` VWAP of the UTC day before `T_NOW`
/// and as the only observation of the trailing window settlement converts at.
fn seed_day_vwap(storage: &StorageHandle, iso: u16, vwap: U256) {
    let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), iso)
        .unwrap()
        .expect("COEN pair registered");
    let day = previous_date_key(timestamp_to_date_key(T_NOW));
    let mut oracle = OracleContract::new(storage.clone());
    oracle.record_utc_day_vwap(day, index, vwap).unwrap();
    let snapshot = outbe_oracle::api::current_vwap_snapshot(storage.clone()).unwrap();
    oracle
        .write_snapshot(
            snapshot.cutoff() - 1,
            &[(
                outbe_oracle::api::AddressPair::new_coen_to(iso),
                vwap,
                six_decimal_unit(),
            )],
        )
        .unwrap();
}

fn merchant_entry_price(storage: &StorageHandle, source_entry: U256) -> U256 {
    let id = seed_and_send(storage, source_entry, source_entry, six_decimal_u128());
    let gem_id = runtime::issue_merchant_gem(storage, ALICE, id, BOB, six_decimal_unit()).unwrap();
    gem_api::get_gem(storage, gem_id)
        .unwrap()
        .unwrap()
        .entry_price_minor
}

mod capacity_conservation;
mod direct_fx_admission;
mod eligibility;
mod issuance;
mod merchant;
mod mining;
mod mining_after_deadline;
mod mining_atomicity;
mod settlement;
