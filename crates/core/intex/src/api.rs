//! Public Rust-to-Rust API for the Intex module.
//!
//! This is the only surface IntexFactory uses to read and write the registry.
//! The lifecycle gates mirror the Origin `IntexNFT1155` state machine
//! (`markQualified` / `markCalled`). There is no precompile dispatch for writes
//! and access is Rust-to-Rust only, so no trusted-caller checks are needed.
//!
//! The registry is a thin ledger: the only thing it validates is the
//! `issued_at` existence sentinel. Business validation (caps, defaults, zero
//! economic parameters) belongs to the caller (IntexFactory).

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::errors::IntexError;
use crate::schema::{CreateSeriesParams, DistProgress, IntexContract, IntexState, SeriesRecord};

/// Create a new Intex series record. Always born in `Issued`. Rejects a
/// duplicate `series_id` (via the record-level create) and a zero `issued_at`
/// (which would make the record read back as non-existent).
pub fn create_series(storage: &StorageHandle<'_>, params: CreateSeriesParams) -> Result<()> {
    if params.issued_at == 0 {
        return Err(IntexError::ZeroIssuedAt.into());
    }

    let mut registry = IntexContract::new(storage.clone());
    let record = SeriesRecord {
        series_id: params.series_id,
        // u128 -> U256 widening is always lossless (see schema docs).
        promis_load_minor: U256::from(params.promis_load_minor),
        entry_price_minor: params.entry_price_minor,
        floor_price_minor: params.floor_price_minor,
        issued_intex_count: params.issued_intex_count,
        call_window_days: params.call_trigger.window_days,
        call_threshold_days: params.call_trigger.threshold_days,
        call_price_minor: params.call_price_minor,
        state: IntexState::Issued as u8,
        issued_at: params.issued_at,
        called_at: 0,
        intex_call_period: params.call_trigger.intex_call_period,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
        worldwide_day: params.worldwide_day,
    };
    registry.create_series_record(&record)
}

/// `Issued -> Qualified`. Mirrors `markQualified`.
pub fn mark_qualified(storage: &StorageHandle<'_>, series_id: u32) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    let mut record = registry.load_series(series_id)?;
    if record.lifecycle_state()? != IntexState::Issued {
        return Err(IntexError::InvalidState {
            expected: IntexState::Issued as u8,
            actual: record.state,
        }
        .into());
    }
    record.state = IntexState::Qualified as u8;
    registry.update_series_record(&record)
}

/// `Issued | Qualified -> Called`. Mirrors `markCalled`. `called_at` is the
/// block timestamp supplied by the caller (deterministic; no wall clock here).
/// `Called` is terminal for these transitions.
pub fn mark_called(storage: &StorageHandle<'_>, series_id: u32, called_at: u32) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    let mut record = registry.load_series(series_id)?;
    let state = record.lifecycle_state()?;
    if state != IntexState::Issued && state != IntexState::Qualified {
        return Err(IntexError::InvalidState {
            expected: IntexState::Qualified as u8,
            actual: record.state,
        }
        .into());
    }
    record.state = IntexState::Called as u8;
    record.called_at = called_at;
    registry.update_series_record(&record)
}

/// Read a series record; errors if the series does not exist.
pub fn read_series(storage: &StorageHandle<'_>, series_id: u32) -> Result<SeriesRecord> {
    IntexContract::new(storage.clone()).load_series(series_id)
}

/// Read a series record; `None` if the series does not exist.
pub fn get_series(storage: &StorageHandle<'_>, series_id: u32) -> Result<Option<SeriesRecord>> {
    IntexContract::new(storage.clone()).get_series(series_id)
}

/// Whether a series exists.
pub fn series_exists(storage: &StorageHandle<'_>, series_id: u32) -> Result<bool> {
    IntexContract::new(storage.clone()).series_exists(series_id)
}

/// Number of series ever created (for dense enumeration).
pub fn total_series(storage: &StorageHandle<'_>) -> Result<u64> {
    IntexContract::new(storage.clone()).read_total_series()
}

/// `series_id` at a dense enumeration index.
pub fn series_id_at(storage: &StorageHandle<'_>, index: u64) -> Result<u32> {
    IntexContract::new(storage.clone()).read_series_id_at(index)
}

// -------------------------------------------------------------------------
// Creator-reward: contributors + paginated distribution
// -------------------------------------------------------------------------

/// Record the (pre-deduplicated) contributor list for a series. Called once
/// per series by lysis, before the tributes are burned. Each entry is
/// `(tribute owner, Σ nominal_amount_minor)`.
pub fn record_contributors(
    storage: &StorageHandle<'_>,
    series_id: u32,
    contributors: &[(Address, U256)],
) -> Result<()> {
    IntexContract::new(storage.clone()).write_contributors(series_id, contributors)
}

/// Number of contributors recorded for a series (0 if none).
pub fn contributor_count(storage: &StorageHandle<'_>, series_id: u32) -> Result<u32> {
    IntexContract::new(storage.clone()).read_contributor_count(series_id)
}

/// Σ of all contributor nominals for a series (the proportionality denominator).
pub fn contributor_total(storage: &StorageHandle<'_>, series_id: u32) -> Result<U256> {
    IntexContract::new(storage.clone()).read_contributor_total(series_id)
}

/// `(owner, nominal)` of the contributor at a dense index.
pub fn contributor_at(
    storage: &StorageHandle<'_>,
    series_id: u32,
    index: u32,
) -> Result<(Address, U256)> {
    IntexContract::new(storage.clone()).read_contributor_at(series_id, index)
}

/// Full contributor list for a series.
pub fn read_contributors(
    storage: &StorageHandle<'_>,
    series_id: u32,
) -> Result<Vec<(Address, U256)>> {
    let registry = IntexContract::new(storage.clone());
    let count = registry.read_contributor_count(series_id)?;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        out.push(registry.read_contributor_at(series_id, i)?);
    }
    Ok(out)
}

/// Open a paginated distribution for a series: create the progress record
/// (cursor 0, nothing paid yet) and enroll the series in the active set the
/// begin-block hook drains.
pub fn start_distribution(
    storage: &StorageHandle<'_>,
    series_id: u32,
    amount: U256,
    total_nominal: U256,
) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    registry.create_dist_progress(&DistProgress {
        series_id,
        amount,
        total_nominal,
        paid_so_far: U256::ZERO,
        cursor: 0,
        active: 1,
    })?;
    registry.push_active_dist(series_id)
}

/// In-flight distribution progress for a series; `None` when none is open.
pub fn get_progress(storage: &StorageHandle<'_>, series_id: u32) -> Result<Option<DistProgress>> {
    IntexContract::new(storage.clone()).get_dist_progress(series_id)
}

/// Persist an updated progress record (advanced cursor / paid total).
pub fn save_progress(storage: &StorageHandle<'_>, progress: &DistProgress) -> Result<()> {
    IntexContract::new(storage.clone()).update_dist_progress(progress)
}

/// Finish a distribution: drop the progress record, the active-set entry, and
/// the (now spent) contributor list.
pub fn clear_distribution(storage: &StorageHandle<'_>, series_id: u32) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    registry.delete_dist_progress(series_id)?;
    registry.remove_active_dist(series_id)?;
    registry.clear_contributors(series_id)
}

/// Number of in-flight distributions (for the begin-block drain).
pub fn active_dist_count(storage: &StorageHandle<'_>) -> Result<u32> {
    IntexContract::new(storage.clone()).read_active_dist_count()
}

/// `series_id` of the active distribution at a dense index.
pub fn active_dist_at(storage: &StorageHandle<'_>, index: u32) -> Result<u32> {
    IntexContract::new(storage.clone()).read_active_dist_at(index)
}
