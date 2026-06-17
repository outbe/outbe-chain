//! Public cross-module API surface for the Oracle module.
//!
//! Exposes read-only helpers that other modules call to validate
//! currency support, without going through the precompile dispatch.

use crate::contract::OracleContract;
use crate::scurve;
use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

/// Validates that `iso_code` is registered as a reference currency.
///
/// Returns `Ok(())` if the code is present in `reference_currencies`,
/// or `PrecompileError::Revert` with a descriptive message otherwise.
///
/// Reference currencies are the ISO 4217 numeric codes considered valid
/// for off-chain pricing references. The list is pre-filled at genesis
/// with `[840]` (USD) and may be extended via future protocol upgrades.
pub fn check_reference_currency(ctx: &BlockRuntimeContext, iso_code: u16) -> Result<()> {
    check_reference_currency_with_storage(ctx.storage.clone(), iso_code)
}

/// Same validation as [`check_reference_currency`] but takes a bare
/// [`StorageHandle`] for callers (e.g. precompile dispatch) that do not have
/// a [`BlockRuntimeContext`] in scope.
pub fn check_reference_currency_with_storage(storage: StorageHandle, iso_code: u16) -> Result<()> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let len = oracle.reference_currencies.len()?;
    for i in 0..len {
        if let Some(code) = oracle.reference_currencies.get(i)? {
            if code == iso_code {
                return Ok(());
            }
        }
    }
    Err(PrecompileError::Revert(format!(
        "iso_code {iso_code} is not a registered reference currency"
    )))
}

pub fn get_pair_id(storage: StorageHandle, iso_code: u16) -> Result<u32> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let pair = oracle.settlement_iso_to_pair.read(&iso_code)?;
    let id = oracle.pair_hash_to_id.read(&pair)?;
    if id == 0 {
        return Err(PrecompileError::Revert("pair not registered".into()));
    }
    Ok(id)
}

pub fn get_worldwide_day_vwap_for_pair_id(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
    pair_id: u32,
) -> Result<Option<U256>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_worldwide_day_vwap_for_pair_id(worldwide_day, pair_id)
}

pub fn get_max_active_scurve_value(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
    pair_id: u32,
) -> Result<U256> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let scurve_timestamp = worldwide_day.to_timestamp_utc();
    scurve::get_max_active_scurve_value(&oracle, pair_id, scurve_timestamp)
}

pub fn get_exchange_rate(storage: StorageHandle, base: &str, quote: &str) -> Result<U256> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let (rate, _, _) = oracle.get_exchange_rate(base, quote)?;
    Ok(rate)
}
