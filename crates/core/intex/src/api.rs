//! Public Rust-to-Rust API for the Intex module.
//!
//! This is the only surface IntexFactory uses to read and write the registry.
//! IntexFactory decides every transition. Writes have no precompile dispatch, and
//! access is Rust-to-Rust only. Thus the API needs no trusted-caller checks.
//!
//! The registry is a thin ledger. Its record validation is the `issued_at`
//! existence sentinel. Other business validation (caps, defaults, zero economic
//! parameters) belongs to the caller (IntexFactory).

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::errors::{date_step_error, IntexError};
use crate::schema::{
    CallStamp, CreateSeriesParams, IntexContract, IntexState, SeriesId, SeriesRecord,
    SeriesRecordEntryExt,
};

/// Create a new Intex series record and enroll it in its call group.
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
        issued_units: params.issued_units,
        call_window_seconds: params.call_trigger.call_window_seconds,
        call_threshold_seconds: params.call_trigger.call_threshold_seconds,
        call_price_minor: params.call_price_minor,
        issued_at: params.issued_at,
        call_notice_period_seconds: params.call_trigger.call_notice_period_seconds,
        transit_margin_seconds: params.call_trigger.transit_margin_seconds,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
        worldwide_day: params.worldwide_day,
    };
    registry.create_series_record(&record)?;
    registry.join_call_group(&record)
}

/// The series' call, read from its group.
pub fn call_stamp(storage: &StorageHandle<'_>, record: &SeriesRecord) -> Result<CallStamp> {
    IntexContract::new(storage.clone()).call_stamp(record)
}

/// The series' state as of this block.
pub fn state(storage: &StorageHandle<'_>, record: &SeriesRecord) -> Result<IntexState> {
    let now = storage.now_secs()?;
    Ok(call_stamp(storage, record)?.state(now))
}

/// Deletes the series once nothing in it is left to settle or mine: no unpaid
/// units (all realized, or returned by its group's expiry) and no settled ones still
/// to mine. Returns whether it was deleted.
pub fn release_if_spent(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<bool> {
    let mut registry = IntexContract::new(storage.clone());
    let Some(record) = registry.get_series(series_id)? else {
        return Ok(false);
    };
    let unpaid = forfeitable_units(storage, &record)?.units;
    // Past the deadline the unpaid units still wait for the group's expiry to return them.
    let group = record.group_key().map_err(date_step_error)?;
    let returned = registry.group_forfeited(group)?;
    if registry.settled_units.read(&series_id)? != 0 || (unpaid != 0 && !returned) {
        return Ok(false);
    }
    registry.leave_call_group(group, series_id)?;
    registry.delete_series_record(series_id)?;
    Ok(true)
}

/// What a series forfeited after its transit close.
pub struct Forfeited {
    pub units: u32,
    pub promis_load_minor: U256,
}

/// The units a series would forfeit now and their load: everything issued and not yet
/// settled, mined or sent to the Gem Factory. Writes nothing.
pub fn forfeitable_units(storage: &StorageHandle<'_>, record: &SeriesRecord) -> Result<Forfeited> {
    let registry = IntexContract::new(storage.clone());
    let series_id = record.series_id;
    let settled = registry.settled_units.read(&series_id)?;
    let mined = registry.mined_units.read(&series_id)?;
    let gem_factory = registry.gem_factory_units.read(&series_id)?;
    // Checked: an underflow is corrupt state, not "nothing owed".
    let forfeited = record
        .issued_units
        .checked_sub(settled)
        .and_then(|left| left.checked_sub(mined))
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

/// Units of `series_id` paid for and not yet mined.
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
pub fn mined_units(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<u32> {
    IntexContract::new(storage.clone())
        .mined_units
        .read(&series_id)
}

/// Record `units` mined: they leave `settled_units` and join `mined_units`,
/// because mining burns the Settled balance it counts.
pub fn record_mined_units(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
    units: u32,
) -> Result<()> {
    let registry = IntexContract::new(storage.clone());
    // Mining more than is settled means the ledger and the token disagree.
    let settled = registry
        .settled_units
        .read(&series_id)?
        .checked_sub(units)
        .ok_or(IntexError::RealizedUnitsOverflow)?;
    let mined = registry
        .mined_units
        .read(&series_id)?
        .checked_add(units)
        .ok_or(IntexError::RealizedUnitsOverflow)?;
    registry.settled_units.write(&series_id, settled)?;
    registry.mined_units.write(&series_id, mined)?;
    add_owner_units(storage, series_id, owner, units, OwnerLedger::Mined)
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
        OwnerLedger::Mined => &registry.owner_mined_units,
        OwnerLedger::GemFactory => &registry.owner_gem_factory_units,
    };
    let total = ledger
        .read(&key)?
        .checked_add(units)
        .ok_or(IntexError::RealizedUnitsOverflow)?;
    ledger.write(&key, total)
}

/// Units of `series_id` this owner mined.
pub fn owner_mined_units(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
) -> Result<u32> {
    IntexContract::new(storage.clone())
        .owner_mined_units
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
    pub mined: u32,
    pub gem_factory: u32,
    pub forfeited: u32,
}

/// Read the disjoint counts. The ledgers hold `settled_units`, `mined_units` and
/// `gem_factory_units` directly. The unpaid remainder is active until expiry
/// decides it is forfeited.
pub fn unit_counts(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<UnitCounts> {
    let registry = IntexContract::new(storage.clone());
    let record = registry.load_series(series_id)?;
    let settled = registry.settled_units.read(&series_id)?;
    let gem_factory = registry.gem_factory_units.read(&series_id)?;
    let mined = registry.mined_units.read(&series_id)?;
    // Checked: an underflow is corrupt state, not an empty class.
    let unpaid = record
        .issued_units
        .checked_sub(settled)
        .and_then(|left| left.checked_sub(mined))
        .and_then(|left| left.checked_sub(gem_factory))
        .ok_or(IntexError::RealizedUnitsOverflow)?;
    let is_forfeited = state(storage, &record)? == IntexState::Forfeited;
    Ok(UnitCounts {
        issued: record.issued_units,
        active: if is_forfeited { 0 } else { unpaid },
        settled,
        mined,
        gem_factory,
        forfeited: if is_forfeited { unpaid } else { 0 },
    })
}

enum RealizedKind {
    Settled,
    GemFactory,
}

/// Which per-owner history ledger a record touches.
enum OwnerLedger {
    Mined,
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
    let mined = registry.mined_units.read(&series_id)?;
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
        .checked_add(mined)
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
    // The stamp picks the series' group, so the series moves to the backdated one.
    registry.leave_call_group(record.group_key().map_err(date_step_error)?, series_id)?;
    record.issued_at = issued_at;
    registry.update_series_record(&record)?;
    registry.join_call_group(&record)
}

/// Calls the group of `series_id` at `called_at`. Test-only: the call sweep calls groups
/// from the price window.
#[cfg(any(test, feature = "test-utils"))]
pub fn call_group_of(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    called_at: u32,
) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    let group = registry
        .load_series(series_id)?
        .group_key()
        .map_err(date_step_error)?;
    let close_at = registry.group_stamp(group, called_at)?.close_at;
    registry.record_group_called(group, called_at, close_at)
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

/// Number of live series (for dense enumeration).
pub fn total_series(storage: &StorageHandle<'_>) -> Result<u32> {
    IntexContract::new(storage.clone()).read_total_series()
}

/// `series_id` at a dense enumeration index.
pub fn series_id_at(storage: &StorageHandle<'_>, index: u32) -> Result<SeriesId> {
    IntexContract::new(storage.clone()).read_series_id_at(index)
}
