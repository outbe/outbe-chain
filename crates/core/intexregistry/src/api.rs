//! Public Rust-to-Rust API for the IntexRegistry module.
//!
//! This is the only surface IntexFactory uses to read and write the registry.
//! The lifecycle gates mirror the Origin `IntexNFT1155` state machine
//! (`markQualified` / `markCalled`). There is no precompile dispatch for writes
//! and access is Rust-to-Rust only, so no trusted-caller checks are needed.
//!
//! The registry is a thin ledger: the only thing it validates is the
//! `issued_at` existence sentinel. Business validation (caps, defaults, zero
//! economic parameters) belongs to the caller (IntexFactory).

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::errors::IntexRegistryError;
use crate::schema::{CreateSeriesParams, IntexRegistryContract, IntexState, SeriesRecord};

/// Create a new Intex series record. Always born in `Issued`. Rejects a
/// duplicate `series_id` (via the record-level create) and a zero `issued_at`
/// (which would make the record read back as non-existent).
pub fn create_series(storage: &StorageHandle<'_>, params: CreateSeriesParams) -> Result<()> {
    if params.issued_at == 0 {
        return Err(IntexRegistryError::ZeroIssuedAt.into());
    }

    let mut registry = IntexRegistryContract::new(storage.clone());
    let record = SeriesRecord {
        series_id: params.series_id,
        // u128 -> U256 widening is always lossless (see schema docs).
        intex_size: U256::from(params.intex_size),
        intex_strike_price: params.intex_strike_price,
        coen_price_floor: params.coen_price_floor,
        issued_intex_count: params.issued_intex_count,
        call_window_days: params.call_trigger.window_days,
        call_threshold_days: params.call_trigger.threshold_days,
        coen_price_call_trigger: params.call_trigger.coen_price_call_trigger,
        state: IntexState::Issued as u8,
        issued_at: params.issued_at,
        called_at: 0,
        intex_call_period: params.intex_call_period,
    };
    registry.create_series_record(&record)
}

/// `Issued -> Qualified`. Mirrors `markQualified`.
pub fn mark_qualified(storage: &StorageHandle<'_>, series_id: u32) -> Result<()> {
    let mut registry = IntexRegistryContract::new(storage.clone());
    let mut record = registry.load_series(series_id)?;
    if record.lifecycle_state()? != IntexState::Issued {
        return Err(IntexRegistryError::InvalidState {
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
    let mut registry = IntexRegistryContract::new(storage.clone());
    let mut record = registry.load_series(series_id)?;
    let state = record.lifecycle_state()?;
    if state != IntexState::Issued && state != IntexState::Qualified {
        return Err(IntexRegistryError::InvalidState {
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
    IntexRegistryContract::new(storage.clone()).load_series(series_id)
}

/// Read a series record; `None` if the series does not exist.
pub fn get_series(storage: &StorageHandle<'_>, series_id: u32) -> Result<Option<SeriesRecord>> {
    IntexRegistryContract::new(storage.clone()).get_series(series_id)
}

/// Whether a series exists.
pub fn series_exists(storage: &StorageHandle<'_>, series_id: u32) -> Result<bool> {
    IntexRegistryContract::new(storage.clone()).series_exists(series_id)
}

/// Number of series ever created (for dense enumeration).
pub fn total_series(storage: &StorageHandle<'_>) -> Result<u64> {
    IntexRegistryContract::new(storage.clone()).read_total_series()
}

/// `series_id` at a dense enumeration index.
pub fn series_id_at(storage: &StorageHandle<'_>, index: u64) -> Result<u32> {
    IntexRegistryContract::new(storage.clone()).read_series_id_at(index)
}
