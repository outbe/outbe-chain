//! Public Rust-to-Rust API for the Intex module.
//!
//! This is the only surface IntexFactory uses to read and write the registry.
//! The lifecycle gates mirror the Origin `IntexNFT1155` state machine
//! (`markCalled`). Writes have no precompile dispatch, and access is
//! Rust-to-Rust only. Thus the API needs no trusted-caller checks.
//!
//! The registry is a thin ledger. Its record validation is the `issued_at`
//! existence sentinel. Other business validation (caps, defaults, zero economic
//! parameters) belongs to the caller (IntexFactory).

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::first_full_day;

use crate::errors::IntexError;
use crate::schema::{
    CreateSeriesParams, IntexContract, IntexState, SeriesId, SeriesRecord, SeriesRecordEntryExt,
};

/// Qualification is derived from a finalized full-day observation, never stored.
/// Readers and settlement use the same reference currency and immutable floor.
pub fn is_qualified(storage: &StorageHandle<'_>, series: &SeriesRecord) -> Result<bool> {
    outbe_oracle::api::closed_above_floor(
        storage.clone(),
        series.reference_currency,
        series.floor_price_minor,
        first_full_day(u64::from(series.issued_at)),
    )
}

/// Create a new Intex series record. The record always starts in `Issued`.
/// The function rejects a duplicate `series_id` (through the record-level create).
/// It also rejects a zero `issued_at`, because that value would make the record
/// read back as non-existent.
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
        issued_units: params.issued_units,
        call_window_seconds: params.call_trigger.call_window_seconds,
        call_threshold_seconds: params.call_trigger.call_threshold_seconds,
        call_price_minor: params.call_price_minor,
        state: IntexState::Issued as u8,
        issued_at: params.issued_at,
        called_at: 0,
        call_notice_period_seconds: params.call_trigger.call_notice_period_seconds,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
        worldwide_day: params.worldwide_day,
    };
    registry.create_series_record(&record)
}

/// `Issued -> Called`. Mirrors `markCalled`. `called_at` is the
/// block timestamp that the caller supplies (deterministic, with no wall clock here).
/// `Called` is terminal for these transitions.
pub fn mark_called(storage: &StorageHandle<'_>, series_id: SeriesId, called_at: u32) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    let mut record = registry.load_series(series_id)?;
    let state = record.lifecycle_state()?;
    if state != IntexState::Issued {
        return Err(IntexError::InvalidState {
            expected: IntexState::Issued as u8,
            actual: record.state,
        }
        .into());
    }
    record.state = IntexState::Called as u8;
    record.called_at = called_at;
    registry.update_series_record(&record)
}

/// What a series forfeited after its notice-period deadline.
pub struct Forfeited {
    pub units: u32,
    pub promis_load_minor: U256,
}

/// The units a called series forfeits and their load, for the caller to return to the pool.
/// This function writes nothing, so the caller must return that load exactly once.
pub fn expire_series(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<Forfeited> {
    let registry = IntexContract::new(storage.clone());
    let record = registry.load_series(series_id)?;
    if record.lifecycle_state()? != IntexState::Called {
        return Err(IntexError::InvalidState {
            expected: IntexState::Called as u8,
            actual: record.state,
        }
        .into());
    }

    let settled = registry.settled_units.read(&series_id)?;
    let exercised = registry.exercised_units.read(&series_id)?;
    let gem_factory = registry.gem_factory_units.read(&series_id)?;
    // Checked: an underflow is corrupt state, not "nothing owed".
    let forfeited = record
        .issued_units
        .checked_sub(settled)
        .and_then(|left| left.checked_sub(exercised))
        .and_then(|left| left.checked_sub(gem_factory))
        .ok_or(IntexError::RealizedUnitsOverflow)?;

    Ok(Forfeited {
        units: forfeited,
        promis_load_minor: record.promis_load_minor,
    })
}

/// Record `units` settled: their load belongs to the settler from now on.
pub fn record_settled_units(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    units: u32,
) -> Result<()> {
    add_realized_units(storage, series_id, units, RealizedKind::Settled)
}

/// Record `units` sent to the Gem Factory by `owner`: their load moved into the position.
pub fn record_gem_factory_units(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
    units: u32,
) -> Result<()> {
    add_realized_units(storage, series_id, units, RealizedKind::GemFactory)?;
    add_owner_units(storage, series_id, owner, units, OwnerLedger::GemFactory)
}

/// Units of `series_id` paid for and not yet exercised.
pub fn settled_units(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<u32> {
    IntexContract::new(storage.clone())
        .settled_units
        .read(&series_id)
}

/// Units of `series_id` sent to the Gem Factory.
pub fn gem_factory_units(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<u32> {
    IntexContract::new(storage.clone())
        .gem_factory_units
        .read(&series_id)
}

/// Units of `series_id` burned into Promis so far.
pub fn exercised_units(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<u32> {
    IntexContract::new(storage.clone())
        .exercised_units
        .read(&series_id)
}

/// Record `units` exercised: they leave `settled_units` and join `exercised_units`,
/// because exercising burns the Settled balance it counts.
pub fn record_exercised_units(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
    units: u32,
) -> Result<()> {
    let registry = IntexContract::new(storage.clone());
    // Exercising more than is settled means the ledger and the token disagree.
    let settled = registry
        .settled_units
        .read(&series_id)?
        .checked_sub(units)
        .ok_or(IntexError::RealizedUnitsOverflow)?;
    let exercised = registry
        .exercised_units
        .read(&series_id)?
        .checked_add(units)
        .ok_or(IntexError::RealizedUnitsOverflow)?;
    registry.settled_units.write(&series_id, settled)?;
    registry.exercised_units.write(&series_id, exercised)?;
    add_owner_units(storage, series_id, owner, units, OwnerLedger::Exercised)
}

/// Adds to one owner's history ledger for the series.
fn add_owner_units(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
    units: u32,
    ledger: OwnerLedger,
) -> Result<()> {
    let registry = IntexContract::new(storage.clone());
    let key = IntexContract::owner_units_key(series_id, owner);
    let ledger = match ledger {
        OwnerLedger::Exercised => &registry.owner_exercised_units,
        OwnerLedger::GemFactory => &registry.owner_gem_factory_units,
    };
    let total = ledger
        .read(&key)?
        .checked_add(units)
        .ok_or(IntexError::RealizedUnitsOverflow)?;
    ledger.write(&key, total)
}

/// Units of `series_id` this owner exercised.
pub fn owner_exercised_units(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
) -> Result<u32> {
    IntexContract::new(storage.clone())
        .owner_exercised_units
        .read(&IntexContract::owner_units_key(series_id, owner))
}

/// Units of `series_id` this owner sent to the Gem Factory.
pub fn owner_gem_factory_units(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
) -> Result<u32> {
    IntexContract::new(storage.clone())
        .owner_gem_factory_units
        .read(&IntexContract::owner_units_key(series_id, owner))
}

/// The disjoint classes an issued unit can be in. They sum to `issued`.
pub struct UnitCounts {
    pub issued: u32,
    pub active: u32,
    pub settled: u32,
    pub exercised: u32,
    pub gem_factory: u32,
    pub forfeited: u32,
}

/// Read the disjoint counts. The ledgers hold `settled_units`, `exercised_units` and
/// `gem_factory_units` directly. The unpaid remainder is active until expiry
/// decides it is forfeited.
pub fn unit_counts(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<UnitCounts> {
    let registry = IntexContract::new(storage.clone());
    let record = registry.load_series(series_id)?;
    let settled = registry.settled_units.read(&series_id)?;
    let gem_factory = registry.gem_factory_units.read(&series_id)?;
    let exercised = registry.exercised_units.read(&series_id)?;
    // Checked: an underflow is corrupt state, not an empty class.
    let unpaid = record
        .issued_units
        .checked_sub(settled)
        .and_then(|left| left.checked_sub(exercised))
        .and_then(|left| left.checked_sub(gem_factory))
        .ok_or(IntexError::RealizedUnitsOverflow)?;
    let now = storage.timestamp()?.to::<u64>();
    let expired = record.effective_state(now)? == IntexState::Expired;
    Ok(UnitCounts {
        issued: record.issued_units,
        active: if expired { 0 } else { unpaid },
        settled,
        exercised,
        gem_factory,
        forfeited: if expired { unpaid } else { 0 },
    })
}

enum RealizedKind {
    Settled,
    GemFactory,
}

/// Which per-owner history ledger a record touches.
enum OwnerLedger {
    Exercised,
    GemFactory,
}

fn add_realized_units(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    units: u32,
    kind: RealizedKind,
) -> Result<()> {
    if units == 0 {
        return Ok(());
    }
    let registry = IntexContract::new(storage.clone());
    // One slot, not the whole record: this runs on every settle.
    let issued = registry.series.entry(series_id).issued_units().read()?;
    let settled = registry.settled_units.read(&series_id)?;
    let exercised = registry.exercised_units.read(&series_id)?;
    let gem_factory = registry.gem_factory_units.read(&series_id)?;

    let (settled, gem_factory) = match kind {
        RealizedKind::Settled => (
            settled
                .checked_add(units)
                .ok_or(IntexError::RealizedUnitsOverflow)?,
            gem_factory,
        ),
        RealizedKind::GemFactory => (
            settled,
            gem_factory
                .checked_add(units)
                .ok_or(IntexError::RealizedUnitsOverflow)?,
        ),
    };
    // Crossing the issued count means the ledgers disagree.
    if settled
        .checked_add(exercised)
        .and_then(|left| left.checked_add(gem_factory))
        .is_none_or(|realized| realized > issued)
    {
        return Err(IntexError::RealizedUnitsOverflow.into());
    }

    match kind {
        RealizedKind::Settled => registry.settled_units.write(&series_id, settled),
        RealizedKind::GemFactory => registry.gem_factory_units.write(&series_id, gem_factory),
    }
}

/// Backdate a series' issuance stamp. Test-only. The Called sweep counts breach
/// days from `issued_at`. A scenario that cannot spend days to live through those
/// days must be able to place issuance before the days it seeded.
#[cfg(any(test, feature = "test-utils"))]
pub fn set_issued_at(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    issued_at: u32,
) -> Result<()> {
    if issued_at == 0 {
        return Err(IntexError::ZeroIssuedAt.into());
    }
    let mut registry = IntexContract::new(storage.clone());
    let mut record = registry.load_series(series_id)?;
    record.issued_at = issued_at;
    registry.update_series_record(&record)
}

/// Read a series record; errors if the series does not exist.
pub fn read_series(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<SeriesRecord> {
    IntexContract::new(storage.clone()).load_series(series_id)
}

/// Read a series record; `None` if the series does not exist.
pub fn get_series(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
) -> Result<Option<SeriesRecord>> {
    IntexContract::new(storage.clone()).get_series(series_id)
}

/// Whether a series exists.
pub fn series_exists(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<bool> {
    IntexContract::new(storage.clone()).series_exists(series_id)
}

/// Number of series ever created (for dense enumeration).
pub fn total_series(storage: &StorageHandle<'_>) -> Result<u64> {
    IntexContract::new(storage.clone()).read_total_series()
}

/// `series_id` at a dense enumeration index.
pub fn series_id_at(storage: &StorageHandle<'_>, index: u64) -> Result<SeriesId> {
    IntexContract::new(storage.clone()).read_series_id_at(index)
}
