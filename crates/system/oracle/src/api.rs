//! Public cross-module API surface for the Oracle module.
//!
//! Exposes read-only helpers that other modules call to validate
//! currency support, without going through the precompile dispatch.

use crate::errors::{OracleError, OracleOcompError};
use crate::schema::{OracleContract, PairIndex};
use crate::scurve;

pub use crate::constants::{DAY_TYPE_ISO, DAY_TYPE_PAIR};
pub use crate::types::{currency_address, AddressPair, AssetType, COEN_ASSET};

use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    storage::StorageHandle,
    time::{previous_date_key, timestamp_to_date_key},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum OcompAuctionEntryPriceSource {
    LastClosedDayVwap = 1,
    CurrentVwapFallback = 2,
}

/// Bounded Oracle projection captured before the terminal OCOMP request.
///
/// `oracle_state_version` reuses the authoritative monotonic snapshot stream
/// index: every exchange-rate snapshot advances it, while the WWD and S-curve
/// counters identify the exact derived collections read for this day.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OcompOraclePreAdmissionProjection {
    pub profile_ready: bool,
    pub auction_entry_price: U256,
    pub auction_entry_price_source: OcompAuctionEntryPriceSource,
    pub auction_entry_price_source_day: u32,
    pub oracle_state_version: u64,
    pub wwd_pair_entries: u32,
    pub active_scurve_entries: u32,
}

/// Validates that `iso_code` is registered as a reference currency.
///
/// Returns `Ok(())` if the code is present in `reference_currencies`, or
/// [`OracleError::NotReferenceCurrency`] otherwise.
///
/// Reference currencies are the ISO 4217 numeric codes considered valid
/// for off-chain pricing references. The list is pre-filled at genesis
/// with `[840]` (USD) and may be extended via future protocol upgrades.
pub fn check_reference_currency(ctx: &BlockRuntimeContext, iso_code: u16) -> Result<()> {
    check_reference_currency_with_storage(ctx.storage.clone(), iso_code)
}

pub fn get_all_reference_currencies(ctx: &BlockRuntimeContext) -> Result<Vec<u16>> {
    let oracle: OracleContract<'_> = OracleContract::new(ctx.storage.clone());
    oracle.reference_currencies.read_all()
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
    Err(OracleError::NotReferenceCurrency { iso_code }.into())
}

/// Current COEN price in currency `iso_code`, 1e18 scaled.
///
/// Reverts when `COEN/<iso_code>` is not a registered pair, and returns
/// `U256::ZERO` when it is registered but carries no published rate. Callers
/// that must not halt a block gate on [`registered_coen_pair`] first and treat
/// zero as "not ready". This is the single definition of the
/// `iso_code -> exchange_rate` lookup.
pub fn coen_rate_for(storage: StorageHandle, iso_code: u16) -> Result<U256> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_exchange_rate(COEN_ASSET, currency_address(iso_code))
}

pub fn get_exchange_rate(storage: StorageHandle, base: Address, quote: Address) -> Result<U256> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_exchange_rate(base, quote)
}

/// The `COEN/<iso_code>` pair, or `None` when it is not registered.
///
/// The key itself is a pure function of the ISO code; the storage read only
/// answers "is it registered", which is what callers actually branch on.
pub fn registered_coen_pair(storage: StorageHandle, iso_code: u16) -> Result<Option<AddressPair>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let pair = AddressPair::new_coen_to(iso_code);
    Ok((oracle.pair_index_of(pair)? != 0).then_some(pair))
}

/// [`registered_coen_pair`] with the "not registered" revert callers repeat.
pub fn require_coen_pair(storage: StorageHandle, iso_code: u16) -> Result<AddressPair> {
    registered_coen_pair(storage, iso_code)?.ok_or_else(|| OracleError::PairNotRegistered.into())
}

pub fn register_pair(storage: StorageHandle, pair: AddressPair) -> Result<PairIndex> {
    let mut oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.register_pair(pair)
}

pub fn set_exchange_rate(
    storage: StorageHandle,
    caller: Address,
    pair: AddressPair,
    rate: U256,
    block_number: u64,
    timestamp: u64,
) -> Result<()> {
    let mut oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.set_exchange_rate(caller, pair, rate, block_number, timestamp)
}

pub fn get_worldwide_day_vwap_for_pair(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
    pair: AddressPair,
) -> Result<Option<U256>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_worldwide_day_vwap_for_pair(worldwide_day, pair)
}

/// Selects the already-stored auction entry price and returns only O(1)
/// authenticated collection counts. This path never invokes calculation or
/// enumerates Oracle records.
pub fn ocomp_pre_admission_projection(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
    current_vwap: U256,
    block_timestamp: u64,
) -> Result<OcompOraclePreAdmissionProjection> {
    let oracle = OracleContract::new(storage);
    let profile_ready = oracle.ocomp_profile_ready.read()?;
    let current_utc_day = timestamp_to_date_key(block_timestamp);
    let last_closed_day = previous_date_key(current_utc_day);
    let last_closed_vwap = profile_ready
        .then(|| oracle.ocomp_day_type_vwap_by_utc_day.read(&last_closed_day))
        .transpose()?
        .filter(|value| !value.is_zero());
    let (auction_entry_price, auction_entry_price_source, source_day) =
        if let Some(vwap) = last_closed_vwap {
            (
                vwap,
                OcompAuctionEntryPriceSource::LastClosedDayVwap,
                last_closed_day,
            )
        } else {
            (
                current_vwap,
                OcompAuctionEntryPriceSource::CurrentVwapFallback,
                worldwide_day.value(),
            )
        };
    let scurve_count = oracle.scurve_count.read()?;
    let scurve_oldest = oracle.scurve_oldest_idx.read()?;
    let active_scurve_entries = scurve_count
        .checked_sub(scurve_oldest)
        .ok_or(OracleOcompError::ScurveOldestExceedsWriteCount)?;

    Ok(OcompOraclePreAdmissionProjection {
        profile_ready,
        auction_entry_price,
        auction_entry_price_source,
        auction_entry_price_source_day: source_day,
        oracle_state_version: oracle.ocomp_state_version.read()?,
        wwd_pair_entries: oracle.worldwide_day_vwap_pair_count.read(&worldwide_day)?,
        active_scurve_entries,
    })
}

/// Initializes the fixed Oracle projection on the fresh-devnet OCOMP fork.
///
/// The genesis-bound OCOMP lifecycle is the sole production caller. The
/// initialization is idempotent only when the configured pair and version
/// already match exactly.
pub fn initialize_fresh_ocomp_profile(storage: StorageHandle) -> Result<()> {
    let mut oracle = OracleContract::new(storage);
    oracle.initialize_fresh_ocomp_profile()
}

/// Stored WorldwideDay VWAP for the [`DAY_TYPE_PAIR`] (`COEN/840`), or `None`
/// when the pair is not registered or the day has no snapshot for it.
///
/// This is the single entry point for the day-rate decision: pair resolution and
/// the snapshot lookup live here, behind one typed interface, so callers never
/// touch the oracle's internal `pair_index` map. Genuine storage faults
/// propagate as `Err`, keeping "no data yet" (`Ok(None)` → caller's RED fallback)
/// distinct from "oracle broken".
pub fn day_type_pair_vwap(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
) -> Result<Option<U256>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    if oracle.pair_index_of(DAY_TYPE_PAIR)? == 0 {
        return Ok(None);
    }
    oracle.get_worldwide_day_vwap_for_pair(worldwide_day, DAY_TYPE_PAIR)
}

/// Computes and stores the WorldwideDay VWAP snapshot for `[start_time,
/// end_time)`. Returns `true` if a snapshot was written, `false` if the window
/// held no oracle data (a deterministic no-op, not an error).
pub fn store_worldwide_day_vwap_snapshot(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
    start_time: u64,
    end_time: u64,
) -> Result<bool> {
    let mut oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.store_worldwide_day_vwap_snapshot(worldwide_day, start_time, end_time)
}

/// Finalized per-UTC-day VWAP for the [`DAY_TYPE_PAIR`] (`COEN/840`), or
/// `None` when the pair is not registered or the day has no finalized value.
pub fn day_type_pair_utc_vwap(storage: StorageHandle, utc_day: u32) -> Result<Option<U256>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    if oracle.pair_index_of(DAY_TYPE_PAIR)? == 0 {
        return Ok(None);
    }
    oracle.get_utc_day_vwap_for_pair(utc_day, DAY_TYPE_PAIR)
}

/// Returns the finalized VWAP for `pair` on the given UTC calendar day
/// (`utc_day` is a yyyymmdd UTC date key, e.g. `20260625`), or `None` if the
/// day is not finalized or had no oracle data for that pair. Distinguishing
/// "not finalized yet" from "finalized, no data" requires comparing `utc_day`
/// against the oracle's `utc_day_vwap_last_finalized` watermark.
pub fn get_utc_day_vwap(
    storage: StorageHandle,
    utc_day: u32,
    pair: AddressPair,
) -> Result<Option<U256>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_utc_day_vwap_for_pair(utc_day, pair)
}

pub fn get_max_active_scurve_value(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
    pair: AddressPair,
) -> Result<U256> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let scurve_timestamp = worldwide_day.to_timestamp_utc();
    scurve::get_max_active_scurve_value(&oracle, pair, scurve_timestamp)
}

/// Annualized currency rate (1e18 scaled) for an ISO 4217 code, read from the
/// reference-currency collection. Reverts when the code is not a registered
/// reference currency or carries no (non-zero) rate. Called by the Credis
/// Factory at issuance to pin the Anadosis currency rate.
pub fn get_currency_rate(storage: StorageHandle, iso_code: u16) -> Result<U256> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_currency_rate(iso_code)
}
