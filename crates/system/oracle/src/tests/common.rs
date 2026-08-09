//! Shared fixtures and helpers for the Oracle test suite.

use crate::schema::OracleContract;
pub(super) use crate::types::{AddressPair, AssetType};
use alloy_primitives::{address, Address, U256};
use outbe_primitives::error::Result as PrecompileResult;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::Units;

/// No shared mutable state between tests; contracts read/write through the
/// scoped `StorageHandle` passed into the closure.
pub(super) fn with_storage<F: FnOnce(StorageHandle)>(f: F) {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, f);
}

pub(super) fn with_storage_at<F: FnOnce(StorageHandle)>(timestamp: u64, f: F) {
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(timestamp));
    StorageHandle::enter(&mut storage, f);
}

// Assets the suite quotes pairs in. `COEN` is the native asset and `USD`/`EUR`
// are ISO 4217 codes, so both encode to reserved addresses; the ERC20 stand-ins
// use their real mainnet addresses, which sit well outside the ISO range.
pub(super) const COEN: Address = Address::ZERO;
pub(super) const USDT: Address = address!("0xdac17f958d2ee523a2206206994597c13d831ec7");
pub(super) const USDC: Address = address!("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
pub(super) const ETH: Address = address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
pub(super) const BTC: Address = address!("0x2260fac5e5542a773aa44fbcfedf7c193bc2c599");

/// ISO 840 (USD) as an asset address.
pub(super) fn usd() -> Address {
    AssetType::IsoCurrency(840).into()
}

/// A pair in the given quoting direction. Storage lookup sorts, so this doubles
/// as the key for either direction.
pub(super) fn pair_key(base: Address, quote: Address) -> AddressPair {
    AddressPair::from_addresses(base, quote)
}

/// Test currency rate (4.30 %, 1e18 scaled) used when building
/// `ReferenceCurrency` genesis entries.
pub(super) const TEST_RATE: U256 = U256::from_limbs([43_000_000_000_000_000u64, 0, 0, 0]);

/// Builds a `ReferenceCurrency` with the test currency rate.
pub(super) fn ref_cur(iso_code: u16) -> crate::genesis::ReferenceCurrency {
    crate::genesis::ReferenceCurrency {
        iso_code,
        currency_rate: TEST_RATE,
    }
}

pub(super) const ATOMIC_DAY_START: u64 = 1_753_228_800;

pub(super) type OracleFixture = fn(&mut HashMapStorageProvider);
pub(super) type OracleMutation = for<'a> fn(StorageHandle<'a>) -> PrecompileResult<()>;

pub(super) fn seed_ocomp_oracle(provider: &mut HashMapStorageProvider) {
    StorageHandle::enter(provider, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        crate::api::initialize_fresh_ocomp_profile(storage).unwrap();
    });
}

pub(super) fn seed_ocomp_oracle_with_snapshot(provider: &mut HashMapStorageProvider) {
    seed_ocomp_oracle(provider);
    StorageHandle::enter(provider, |storage| {
        OracleContract::new(storage)
            .write_snapshot(
                ATOMIC_DAY_START + 100,
                &[(pair_key(COEN, usd()), U256::from(125), U256::from(2))],
            )
            .unwrap();
    });
}

pub(super) fn seed_ocomp_oracle_with_scurve(provider: &mut HashMapStorageProvider) {
    seed_ocomp_oracle(provider);
    StorageHandle::enter(provider, |storage| {
        crate::scurve::store_scurve_entry(
            &mut OracleContract::new(storage),
            pair_key(COEN, usd()),
            ATOMIC_DAY_START,
            U256::from(125),
        )
        .unwrap();
    });
}

pub(super) const SCURVE_CURRENT_DAY: u64 = ATOMIC_DAY_START + 3 * crate::scurve::DAY_SECONDS;

pub(super) fn seed_oracle_with_peak_history(
    provider: &mut HashMapStorageProvider,
    initialize_ocomp: bool,
) {
    StorageHandle::enter(provider, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        if initialize_ocomp {
            crate::api::initialize_fresh_ocomp_profile(storage).unwrap();
        }
        for (day, price) in [
            (SCURVE_CURRENT_DAY - 3 * crate::scurve::DAY_SECONDS, 100_u64),
            (SCURVE_CURRENT_DAY - 2 * crate::scurve::DAY_SECONDS, 150_u64),
            (SCURVE_CURRENT_DAY - crate::scurve::DAY_SECONDS, 120_u64),
        ] {
            oracle
                .write_snapshot(
                    day + 100,
                    &[(pair_key(COEN, usd()), U256::from(price), U256::from(2))],
                )
                .unwrap();
        }
    });
}

pub(super) fn seed_ocomp_oracle_with_peak_history(provider: &mut HashMapStorageProvider) {
    seed_oracle_with_peak_history(provider, true);
}

pub(super) fn seed_prefork_oracle_with_peak_history(provider: &mut HashMapStorageProvider) {
    seed_oracle_with_peak_history(provider, false);
}

pub(super) fn seed_prefork_oracle_with_snapshot(provider: &mut HashMapStorageProvider) {
    StorageHandle::enter(provider, |storage| {
        let mut oracle = OracleContract::new(storage);
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        oracle
            .write_snapshot(
                ATOMIC_DAY_START + 100,
                &[(pair_key(COEN, usd()), U256::from(125), U256::from(2))],
            )
            .unwrap();
    });
}

pub(super) fn write_snapshot_mutation(storage: StorageHandle<'_>) -> PrecompileResult<()> {
    OracleContract::new(storage).write_snapshot(
        ATOMIC_DAY_START + 100,
        &[(pair_key(COEN, usd()), U256::from(125), U256::from(2))],
    )
}

pub(super) fn store_wwd_snapshot_mutation(storage: StorageHandle<'_>) -> PrecompileResult<()> {
    OracleContract::new(storage).store_worldwide_day_vwap_snapshot(
        outbe_common::WorldwideDay::from_timestamp(ATOMIC_DAY_START),
        ATOMIC_DAY_START,
        ATOMIC_DAY_START + outbe_primitives::time::SECONDS_PER_DAY,
    )
}

pub(super) fn finalize_utc_day_mutation(storage: StorageHandle<'_>) -> PrecompileResult<()> {
    OracleContract::new(storage).finalize_utc_day_vwap(
        outbe_primitives::time::timestamp_to_date_key(ATOMIC_DAY_START),
    )
}

pub(super) fn store_scurve_mutation(storage: StorageHandle<'_>) -> PrecompileResult<()> {
    crate::scurve::store_scurve_entry(
        &mut OracleContract::new(storage),
        pair_key(COEN, usd()),
        ATOMIC_DAY_START,
        U256::from(125),
    )
}

pub(super) fn evict_scurve_mutation(storage: StorageHandle<'_>) -> PrecompileResult<()> {
    crate::scurve::evict_expired_scurves(
        &mut OracleContract::new(storage),
        ATOMIC_DAY_START
            + (u64::try_from(crate::scurve::PERIOD).unwrap() + 1) * crate::scurve::DAY_SECONDS,
    )
}

pub(super) fn process_scurve_mutation(storage: StorageHandle<'_>) -> PrecompileResult<()> {
    crate::scurve::process_daily_scurve(
        &mut OracleContract::new(storage),
        pair_key(COEN, usd()),
        SCURVE_CURRENT_DAY,
    )
}

pub(super) fn assert_oracle_mutation_is_atomic(
    label: &str,
    fixture: OracleFixture,
    mutation: OracleMutation,
) {
    let mutation_count = {
        let mut provider = HashMapStorageProvider::new(1);
        fixture(&mut provider);
        provider.clear_mutation_failure();
        StorageHandle::enter(&mut provider, mutation).unwrap();
        provider.clear_mutation_failure()
    };
    assert!(
        mutation_count > 1,
        "{label} fixture must cross partial-write boundaries"
    );

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(1);
        fixture(&mut provider);
        provider.clear_mutation_failure();
        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        provider.fail_after_mutation_at(operation);

        let result = StorageHandle::enter(&mut provider, mutation);
        assert!(
            result.is_err(),
            "{label}: fault after mutation {operation} must propagate"
        );
        assert_eq!(
            provider.clear_mutation_failure(),
            operation + 1,
            "{label}: unexpected write boundary"
        );
        assert_eq!(
            provider.storage, storage_before,
            "{label}: persistent state changed after mutation {operation}"
        );
        assert_eq!(
            provider.events, events_before,
            "{label}: events changed after mutation {operation}"
        );
    }
}

pub(super) fn run_prefork_with_last_mutation_failure(
    fixture: OracleFixture,
    mutation: OracleMutation,
) -> HashMapStorageProvider {
    let mutation_count = {
        let mut provider = HashMapStorageProvider::new(1);
        fixture(&mut provider);
        provider.clear_mutation_failure();
        StorageHandle::enter(&mut provider, mutation).unwrap();
        provider.clear_mutation_failure()
    };
    assert!(mutation_count > 1);

    let mut provider = HashMapStorageProvider::new(1);
    fixture(&mut provider);
    provider.clear_mutation_failure();
    let events_before = provider.events.clone();
    provider.fail_mutation_at(mutation_count - 1);
    StorageHandle::enter(&mut provider, mutation).unwrap();
    assert_eq!(provider.clear_mutation_failure(), mutation_count - 1);
    assert_eq!(
        provider.events, events_before,
        "pre-fork best-effort event failure must not synthesize an event"
    );
    provider
}

// -----------------------------------------------------------------------
// Phase 2: Tally integration tests
// -----------------------------------------------------------------------

/// Helper: initialize oracle config and register a pair.
pub(super) fn init_oracle(oracle: &mut OracleContract) {
    oracle.config_vote_period.write(2).unwrap();
    oracle
        .config_reward_band
        .write(U256::from(20_000_000_000_000_000u128))
        .unwrap(); // 0.02
    oracle.config_slash_window.write(96).unwrap();
    oracle
        .config_min_valid_per_window
        .write(U256::from(50_000_000_000_000_000u128))
        .unwrap(); // 0.05
    oracle.config_slash_fraction.write(U256::ZERO).unwrap();
    oracle.config_lookback_duration.write(86400).unwrap();
    oracle.config_enabled.write(true).unwrap();
    oracle.config_is_initialized.write(true).unwrap();
}

/// Helper: register a validator in the ValidatorSet with given stake.
/// Uses the first byte of addr as the pubkey seed to avoid BLS pubkey collision.
pub(super) fn register_validator(storage: StorageHandle, addr: Address, stake: U256) {
    use outbe_validatorset::logic::status;

    let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
    // Only write config once (if not already initialized)
    if !vs.config_is_initialized.read().unwrap() {
        vs.config_is_initialized.write(true).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_min_stake.write(U256::in_units(1u64)).unwrap();
        vs.config_epoch_length_blocks.write(3600).unwrap();
        vs.config_owner.write(Address::ZERO).unwrap();
    }

    // Generate unique pubkey from address
    let mut pubkey = [0u8; 48];
    pubkey[..20].copy_from_slice(addr.as_slice());
    vs.register_validator(Address::ZERO, addr, &pubkey).unwrap();
    // Set stake and status to ACTIVE
    vs.val_stake.write(&addr, stake).unwrap();
    vs.val_status.write(&addr, status::ACTIVE).unwrap();
    vs.val_has_bls_share.write(&addr, true).unwrap();
}
