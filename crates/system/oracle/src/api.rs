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

/// The pair whose WorldwideDay VWAP drives the GREEN/RED day-type decision.
pub const DAY_TYPE_PAIR: (&str, &str) = ("COEN", "0xUSD");

/// Stored WorldwideDay VWAP for the [`DAY_TYPE_PAIR`] (`COEN/0xUSD`), or `None`
/// when the pair is not registered or the day has no snapshot for it.
///
/// This is the single entry point for the day-rate decision: pair resolution and
/// the snapshot lookup live here, behind one typed interface, so callers never
/// touch the oracle's internal `pair_hash_to_id` map. Genuine storage faults
/// propagate as `Err`, keeping "no data yet" (`Ok(None)` → caller's RED fallback)
/// distinct from "oracle broken".
pub fn day_type_pair_vwap(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
) -> Result<Option<U256>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let (base, quote) = DAY_TYPE_PAIR;
    let pair_id = oracle.get_pair_id(base, quote)?;
    if pair_id == 0 {
        return Ok(None);
    }
    oracle.get_worldwide_day_vwap_for_pair_id(worldwide_day, pair_id)
}

/// Computes and stores the WorldwideDay VWAP snapshot for `[start_time,
/// end_time)`. Returns `true` if a snapshot was written, `false` if the window
/// held no oracle data (a deterministic no-op, not an error).
///
/// Owns the legacy `"no VWAP data"` revert string so callers route off the typed
/// `bool` instead of matching oracle error text across the module seam.
pub fn store_worldwide_day_vwap_snapshot(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
    start_time: u64,
    end_time: u64,
) -> Result<bool> {
    let mut oracle: OracleContract<'_> = OracleContract::new(storage);
    match oracle.store_worldwide_day_vwap_snapshot(worldwide_day, start_time, end_time) {
        Ok(()) => Ok(true),
        Err(PrecompileError::Revert(msg)) if msg.contains("no VWAP data") => Ok(false),
        Err(err) => Err(err),
    }
}

/// Finalized per-UTC-day VWAP for the [`DAY_TYPE_PAIR`] (`COEN/0xUSD`), or
/// `None` when the pair is not registered or the day has no finalized value.
pub fn day_type_pair_utc_vwap(storage: StorageHandle, utc_day: u32) -> Result<Option<U256>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let (base, quote) = DAY_TYPE_PAIR;
    let pair_id = oracle.get_pair_id(base, quote)?;
    if pair_id == 0 {
        return Ok(None);
    }
    oracle.get_utc_day_vwap_for_pair_id(utc_day, pair_id)
}

/// Returns the finalized VWAP for `pair_id` on the given UTC calendar day
/// (`utc_day` is a yyyymmdd UTC date key, e.g. `20260625`), or `None` if the
/// day is not finalized or had no oracle data for that pair. Distinguishing
/// "not finalized yet" from "finalized, no data" requires comparing `utc_day`
/// against the oracle's `utc_day_vwap_last_finalized` watermark.
pub fn get_utc_day_vwap(
    storage: StorageHandle,
    utc_day: u32,
    pair_id: u32,
) -> Result<Option<U256>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_utc_day_vwap_for_pair_id(utc_day, pair_id)
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

/// Annualized refinancing rate (1e18 scaled) for an ISO 4217 code, read from the
/// reference-currency collection. Reverts when the code is not a registered
/// reference currency or carries no (non-zero) rate. Called by the Credis
/// Factory at issuance to pin the Anadosis refinancing rate.
pub fn get_refinancing_rate(storage: StorageHandle, iso_code: u16) -> Result<U256> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_refinancing_rate(iso_code)
}
