//! Public cross-module API surface for the Oracle module.
//!
//! Exposes read-only helpers that other modules call to validate
//! currency support, without going through the precompile dispatch.

use crate::constants::FX_RATE_MAX_AGE_SECONDS;
use crate::errors::{OracleError, OracleOcompError};
use crate::schema::{OracleContract, PairIndex};
use crate::scurve;

pub use crate::constants::{DAY_TYPE_ISO, DAY_TYPE_PAIR};
pub use crate::types::{currency_address, AddressPair, AssetType, COEN_ASSET};
pub use crate::window::{
    active_vwap_policy, get_vwap_snapshot_id, VwapPolicy, VwapSnapshotId, DEFAULT_VWAP_POLICY,
};

use alloy_primitives::{Address, U256};

use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::StorageHandle};

/// Bounded Oracle projection captured before the terminal OCOMP request.
///
/// `oracle_state_version` is the `ocomp_state_version` counter.
/// It is not the snapshot-stream index.
/// It advances on snapshots, WorldwideDay VWAP writes, UTC-day finalization,
/// and S-curve writes. It advances only after the profile is ready.
/// It does not advance on every snapshot.
/// The WWD and S-curve counters identify the derived collections for this day.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompOraclePreAdmissionProjection {
    pub profile_ready: bool,
    pub oracle_state_version: u64,
    /// Registered pairs, i.e. the upper bound on the day-VWAP entries an
    /// opening proof can be asked to cover. The registry index now keys the
    /// WorldwideDay VWAP column, so the registry size *is* that bound. There is
    /// no separate per-day entry count to read.
    pub wwd_pair_entries: u32,
    pub active_scurve_entries: u32,
}

/// Validates that `iso_code` is registered as a reference currency.
///
/// Returns `Ok(())` if the code is present in `reference_currencies`, or
/// [`OracleError::NotReferenceCurrency`] otherwise.
///
/// Reference currencies are the ISO 4217 numeric codes considered valid
/// for off-chain pricing references. Genesis pre-fills six codes:
/// CNY 156, HKD 344, JPY 392, GBP 826, USD 840, and EUR 978.
/// USD is the mandatory member. Future protocol upgrades may extend it.
pub fn check_reference_currency(ctx: &BlockRuntimeContext, iso_code: u16) -> Result<()> {
    check_reference_currency_with_storage(ctx.storage.clone(), iso_code)
}

pub fn get_all_reference_currencies(ctx: &BlockRuntimeContext) -> Result<Vec<u16>> {
    reference_currencies(ctx.storage.clone())
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

/// Current COEN price to currency `iso_code` in the COEN/ISO six-decimal scale.
///
/// Returns an error when `COEN/<iso_code>` is not a registered pair, or when
/// the pair has no rates.
pub fn coen_rate_for(storage: StorageHandle, iso_code: u16) -> Result<U256> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_exchange_rate(COEN_ASSET, currency_address(iso_code))
}

/// `amount`, denominated in `from_iso`, re-expressed in `to_iso`.
///
/// Both currencies are priced against COEN, so the cross rate is the ratio of
/// the two legs: `amount x rate(COEN/to) / rate(COEN/from)`. The function rounds
/// once, upwards, so a converted charge never undercollects. Equal currencies
/// short-circuit and read no rate at all.
///
/// Reverts when either leg has no registered pair or no published rate.
pub fn currency_cross_rate(
    storage: StorageHandle,
    from_iso: u16,
    to_iso: u16,
    amount: U256,
) -> Result<U256> {
    cross_rate_with(from_iso, to_iso, amount, |iso_code| {
        coen_rate_for(storage.clone(), iso_code)
    })
}

/// `amount x rate(to_iso) / rate(from_iso)`, rounded up once, with each leg read
/// by `coen_rate`: `from_iso` first, then `to_iso`. Equal currencies and zero
/// amounts return `amount` and read no rate.
fn cross_rate_with(
    from_iso: u16,
    to_iso: u16,
    amount: U256,
    coen_rate: impl Fn(u16) -> Result<U256>,
) -> Result<U256> {
    if from_iso == to_iso || amount.is_zero() {
        return Ok(amount);
    }
    let rate_from = coen_rate(from_iso)?;
    let rate_to = coen_rate(to_iso)?;
    let numerator = amount
        .checked_mul(rate_to)
        .ok_or(OracleError::CrossRateOverflow)?;
    Ok(numerator.div_ceil(rate_from))
}

/// Current COEN price to currency `iso_code`, or `None` when the pair is not
/// registered or carries no published rate.
///
/// [`get_all_reference_currencies`] lists currencies independently of whether
/// their `COEN/<iso>` pair is registered and priced. A block hook that walks that
/// registry therefore needs a read that reports "not priceable yet" instead of
/// reverting and halting the block. Storage faults still propagate.
pub fn coen_rate_for_opt(storage: StorageHandle, iso_code: u16) -> Result<Option<U256>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let index = oracle.pair_index_of(AddressPair::new_coen_to(iso_code))?;
    if index == 0 {
        return Ok(None);
    }
    let stored = oracle.exchange_rate.read(&index)?;
    Ok((!stored.is_zero()).then_some(stored))
}

/// Snapshot required at the current block under the active policy.
pub fn current_vwap_snapshot(storage: StorageHandle) -> Result<VwapSnapshotId> {
    let now = storage.timestamp()?.to::<u64>();
    get_vwap_snapshot_id(now, &active_vwap_policy())
}

/// Finalized COEN/`iso_code` VWAP over the snapshot's window, in the pair's
/// six-decimal scale. `None` when the pair is unregistered or the window holds no
/// positive price. An open or malformed snapshot is an error.
pub fn get_finalized_window_vwap(
    storage: StorageHandle,
    iso_code: u16,
    snapshot: VwapSnapshotId,
) -> Result<Option<U256>> {
    let oracle = OracleContract::new(storage);
    let pair = AddressPair::new_coen_to(iso_code);
    if oracle.pair_index_of(pair)? == 0 {
        return Ok(None);
    }
    oracle.finalized_window_vwap(pair, snapshot)
}

/// Both COEN legs of a cross-currency settlement, read from the one trailing
/// snapshot required at the current block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettlementFxRates {
    pub snapshot: VwapSnapshotId,
    pub issuance_currency_vwap_minor: U256,
    pub reference_currency_vwap_minor: U256,
}

/// `None` while either leg lacks a finalized positive price in that window.
pub fn settlement_fx_rates(
    storage: StorageHandle,
    issuance_currency: u16,
    reference_currency: u16,
) -> Result<Option<SettlementFxRates>> {
    let snapshot = current_vwap_snapshot(storage.clone())?;
    let Some(issuance_currency_vwap_minor) =
        get_finalized_window_vwap(storage.clone(), issuance_currency, snapshot)?
    else {
        return Ok(None);
    };
    let Some(reference_currency_vwap_minor) =
        get_finalized_window_vwap(storage, reference_currency, snapshot)?
    else {
        return Ok(None);
    };
    Ok(Some(SettlementFxRates {
        snapshot,
        issuance_currency_vwap_minor,
        reference_currency_vwap_minor,
    }))
}

/// Reference currencies available for pricing through a storage-only caller.
pub fn reference_currencies(storage: StorageHandle) -> Result<Vec<u16>> {
    OracleContract::new(storage).reference_currencies.read_all()
}

/// Current COEN price for `iso_code`, accepted only when its canonical
/// publication timestamp is non-zero and no older than six hours.
pub fn fresh_coen_rate_for(storage: StorageHandle, iso_code: u16) -> Result<U256> {
    let (_, index) = require_coen_pair(storage.clone(), iso_code)?;
    fresh_rate_at_index(storage, index)?
        .ok_or_else(|| OracleError::StaleCoenRate { iso_code }.into())
}

/// Hook-safe variant of [`fresh_coen_rate_for`]. An unregistered, unpublished or
/// stale currency is not priceable in this block and therefore returns `None`.
/// Storage faults still propagate.
pub fn fresh_coen_rate_for_opt(storage: StorageHandle, iso_code: u16) -> Result<Option<U256>> {
    let oracle = OracleContract::new(storage.clone());
    let index = oracle.pair_index_of(AddressPair::new_coen_to(iso_code))?;
    if index == 0 {
        return Ok(None);
    }
    fresh_rate_at_index(storage, index)
}

/// Freshness-enforcing counterpart of [`currency_cross_rate`] for live economic
/// paths. Equal currencies and zero amounts retain the no-read short circuit.
pub fn fresh_currency_cross_rate(
    storage: StorageHandle,
    from_iso: u16,
    to_iso: u16,
    amount: U256,
) -> Result<U256> {
    cross_rate_with(from_iso, to_iso, amount, |iso_code| {
        fresh_coen_rate_for(storage.clone(), iso_code)
    })
}

fn fresh_rate_at_index(storage: StorageHandle, index: PairIndex) -> Result<Option<U256>> {
    let now = storage.timestamp()?.to::<u64>();
    let oracle = OracleContract::new(storage);
    let rate = oracle.exchange_rate.read(&index)?;
    let published = oracle.exchange_rate_timestamp.read(&index)?;
    Ok((!rate.is_zero()
        && published != 0
        && now.saturating_sub(published) <= FX_RATE_MAX_AGE_SECONDS)
        .then_some(rate))
}

/// Registry index of the `COEN/<iso_code>` pair, or `None` when it was never registered.
/// [`require_coen_pair`] is different: it collapses both cases into an error.
pub fn coen_pair_index_opt(storage: StorageHandle, iso_code: u16) -> Result<Option<PairIndex>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let index = oracle.pair_index_of(AddressPair::new_coen_to(iso_code))?;
    Ok((index != 0).then_some(index))
}

pub fn get_exchange_rate(storage: StorageHandle, base: Address, quote: Address) -> Result<U256> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_exchange_rate(base, quote)
}

/// The registered `COEN/<iso_code>` pair and its index, reverting when the
/// pair is not registered.
pub fn require_coen_pair(
    storage: StorageHandle,
    iso_code: u16,
) -> Result<(AddressPair, PairIndex)> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let pair = AddressPair::new_coen_to(iso_code);
    let index = oracle.pair_index_of(pair)?;
    if index == 0 {
        return Err(OracleError::PairNotRegistered { pair }.into());
    }
    Ok((pair, index))
}

pub fn register_pair(storage: StorageHandle, pair: AddressPair) -> Result<PairIndex> {
    let mut oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.register_pair(pair)
}

/// Public Oracle inputs used by the Tribute enclave.
///
/// Both VWAPs are exact WorldwideDay snapshots in the six-decimal COEN/ISO
/// domain. The S-curve belongs only to the independently selected reference
/// currency. The enclave owns the final `max(reference_vwap, reference_curve)`
/// and cross-currency nominal arithmetic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributePricingInputs {
    pub issuance_wwd_vwap_minor: U256,
    pub reference_wwd_vwap_minor: U256,
    pub reference_scurve_minor: U256,
}

/// Reads the three canonical public pricing inputs for one Tribute.
///
/// `None` means the issuance `COEN/<iso>` pair is not registered. Once the
/// issuance pair exists, zero fields represent missing daily data or a missing
/// reference pair. The Tribute host can then reject the transient unpriced
/// condition without collapsing it into an unsupported issuance.
pub fn tribute_pricing_inputs(
    storage: StorageHandle,
    issuance_currency: u16,
    reference_currency: u16,
    worldwide_day: WorldwideDay,
) -> Result<Option<TributePricingInputs>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage.clone());
    let issuance_pair = AddressPair::new_coen_to(issuance_currency);
    let issuance_index = oracle.pair_index_of(issuance_pair)?;
    if issuance_index == 0 {
        return Ok(None);
    }
    let issuance_wwd_vwap_minor = oracle
        .get_worldwide_day_vwap_for_pair(worldwide_day, issuance_index)?
        .unwrap_or(U256::ZERO);
    let reference_pair = AddressPair::new_coen_to(reference_currency);
    let reference_index = oracle.pair_index_of(reference_pair)?;
    let reference_wwd_vwap_minor = if reference_index == 0 {
        U256::ZERO
    } else {
        oracle
            .get_worldwide_day_vwap_for_pair(worldwide_day, reference_index)?
            .unwrap_or(U256::ZERO)
    };
    let reference_scurve_minor = if reference_index == 0 {
        U256::ZERO
    } else {
        get_max_active_scurve_value(storage, worldwide_day, reference_pair)?
    };
    Ok(Some(TributePricingInputs {
        issuance_wwd_vwap_minor,
        reference_wwd_vwap_minor,
        reference_scurve_minor,
    }))
}

/// One exchange-rate observation: the rate and the block that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateObservation {
    /// Rate in the pair's registered scale.
    pub rate: U256,
    pub block_number: u64,
    pub timestamp: u64,
}

/// Sets the exchange rate of `pair` from `observation` (system-only bootstrap
/// write).
pub fn set_exchange_rate(
    storage: StorageHandle,
    caller: Address,
    pair: AddressPair,
    observation: RateObservation,
) -> Result<()> {
    let mut oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.set_exchange_rate(
        caller,
        pair,
        observation.rate,
        observation.block_number,
        observation.timestamp,
    )
}

/// Stored WorldwideDay VWAP for the pair registered under `index`, or `None`
/// when the day has no snapshot or that pair had no data in it. Callers get the
/// index from [`require_coen_pair`] or [`register_pair`].
pub fn get_worldwide_day_vwap_for_pair(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
    index: PairIndex,
) -> Result<Option<U256>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_worldwide_day_vwap_for_pair(worldwide_day, index)
}

/// Every reference currency the closed UTC day `utc_day` priced, ascending by
/// currency. A currency is present when its `COEN/<iso>` pair is registered and
/// the day left a non-zero VWAP for it. An unpriced currency is absent rather
/// than zero. It never invokes calculation. A stored value is already finalized.
pub fn priced_reference_currencies(
    storage: StorageHandle,
    utc_day: u32,
) -> Result<Vec<(u16, U256)>> {
    let oracle = OracleContract::new(storage);
    let mut priced = Vec::new();
    // The day-type currency is priced from the same per-pair day VWAP as every
    // other currency. The OCOMP mirror of it is a copy, written only once the
    // profile is installed, and is not the price source. This function reads it
    // from its own pair, so the day-type price does not depend on 840's registry entry.
    let day_type_index = oracle.pair_index_of(DAY_TYPE_PAIR)?;
    if day_type_index != 0 {
        if let Some(vwap) = oracle
            .get_utc_day_vwap_for_pair(utc_day, day_type_index)?
            .filter(|value| !value.is_zero())
        {
            priced.push((DAY_TYPE_ISO, vwap));
        }
    }
    for iso_code in oracle.reference_currencies.read_all()? {
        if iso_code == DAY_TYPE_ISO {
            continue;
        }
        let index = oracle.pair_index_of(AddressPair::new_coen_to(iso_code))?;
        if index == 0 {
            continue;
        }
        if let Some(vwap) = oracle
            .get_utc_day_vwap_for_pair(utc_day, index)?
            .filter(|value| !value.is_zero())
        {
            priced.push((iso_code, vwap));
        }
    }
    priced.sort_by_key(|(iso_code, _)| *iso_code);
    Ok(priced)
}

/// Reads the authenticated collection counts. It never invokes calculation.
pub fn ocomp_pre_admission_projection(
    storage: StorageHandle,
) -> Result<OcompOraclePreAdmissionProjection> {
    let oracle = OracleContract::new(storage);
    let scurve_count = oracle.scurve_count.read()?;
    let scurve_oldest = oracle.scurve_oldest_idx.read()?;
    let active_scurve_entries = scurve_count
        .checked_sub(scurve_oldest)
        .ok_or(OracleOcompError::ScurveOldestExceedsWriteCount)?;

    Ok(OcompOraclePreAdmissionProjection {
        profile_ready: oracle.ocomp_profile_ready.read()?,
        oracle_state_version: oracle.ocomp_state_version.read()?,
        wwd_pair_entries: oracle.pair_count.read()?,
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
/// This is the single entry point for the day-rate decision. Pair resolution and
/// the snapshot lookup live here, behind one typed interface. Callers therefore
/// never touch the oracle's internal `pair_index` map. Genuine storage faults
/// propagate as `Err`. This keeps "no data yet" (`Ok(None)` -> caller's RED
/// fallback) distinct from "oracle broken".
pub fn day_type_pair_vwap(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
) -> Result<Option<U256>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let index = oracle.pair_index_of(DAY_TYPE_PAIR)?;
    if index == 0 {
        return Ok(None);
    }
    oracle.get_worldwide_day_vwap_for_pair(worldwide_day, index)
}

/// Computes and stores the WorldwideDay VWAP snapshot for `[start_time,
/// end_time)`. Returns `true` if it wrote a snapshot, `false` if the window held
/// no oracle data (a deterministic no-op, not an error).
pub fn store_worldwide_day_vwap_snapshot(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
    start_time: u64,
    end_time: u64,
) -> Result<bool> {
    let mut oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.store_worldwide_day_vwap_snapshot(worldwide_day, start_time, end_time)
}

/// Returns the stored VWAP for `pair` on that UTC calendar day, or `None`
/// when the day has no entry. `utc_day` is a yyyymmdd UTC date key.
/// A day above `utc_day_vwap_last_finalized` is not finalized.
/// After a gap wider than the backfill cap, a day at or below the watermark
/// can also be unfinalized. An empty entry is then not proof of no data.
pub fn get_utc_day_vwap(
    storage: StorageHandle,
    utc_day: u32,
    index: PairIndex,
) -> Result<Option<U256>> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_utc_day_vwap_for_pair(utc_day, index)
}

/// Whether a finalized day from `from_utc_day` on closed `COEN/<iso_code>` strictly
/// above `floor_minor`.
pub fn closed_above_floor(
    storage: StorageHandle,
    iso_code: u16,
    floor_minor: U256,
    from_utc_day: u32,
) -> Result<bool> {
    Ok(max_day_vwap_since(storage, iso_code, from_utc_day, Some(floor_minor))? > floor_minor)
}

/// The highest finalized daily VWAP of `COEN/<iso_code>` from `from_utc_day` on.
/// Zero when none.
pub fn max_utc_day_vwap_since(
    storage: StorageHandle,
    iso_code: u16,
    from_utc_day: u32,
) -> Result<U256> {
    max_day_vwap_since(storage, iso_code, from_utc_day, None)
}

fn max_day_vwap_since(
    storage: StorageHandle,
    iso_code: u16,
    from_utc_day: u32,
    stop_above: Option<U256>,
) -> Result<U256> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    let index = oracle.pair_index_of(AddressPair::new_coen_to(iso_code))?;
    if index == 0 {
        return Ok(U256::ZERO);
    }
    oracle.max_finalized_day_vwap_since(index, from_utc_day, stop_above)
}

/// Returns the finalized UTC-day VWAP for `COEN/<iso_code>` at its original `10^6` scale.
/// `utc_day` is a yyyymmdd UTC date key. Missing pairs, unavailable daily prices,
/// and stored zero prices return `None`. Oracle and storage errors propagate unchanged.
pub fn get_utc_day_vwap_for_iso(
    storage: StorageHandle,
    utc_day: u32,
    iso_code: u16,
) -> Result<Option<U256>> {
    let Some(index) = coen_pair_index_opt(storage.clone(), iso_code)? else {
        return Ok(None);
    };
    get_utc_day_vwap(storage, utc_day, index)
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

/// Annualized policy rate (scale `1e6`) for an independently registered ISO
/// 4217 code. The Credis Factory calls it at issuance to pin the position's
/// settlement policy without coupling it to reference-currency membership.
pub fn get_policy_rate(storage: StorageHandle, iso_code: u16) -> Result<U256> {
    let oracle: OracleContract<'_> = OracleContract::new(storage);
    oracle.get_policy_rate(iso_code)
}
