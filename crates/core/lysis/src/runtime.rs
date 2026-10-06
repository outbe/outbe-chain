#[cfg(test)]
use crate::program_v1::{self, ProgramErrorV1};
use alloy_primitives::U256;
#[cfg(test)]
use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key, WorldwideDay};
use outbe_primitives::{error::Result, storage::StorageHandle};
use std::collections::BTreeMap;

#[cfg(test)]
pub(crate) fn consume_required_gratis(remaining: &mut U256, gratis_load: U256) -> Result<()> {
    *remaining =
        program_v1::validate_required_gratis(*remaining, gratis_load, 0).map_err(program_error)?;
    Ok(())
}

/// Computes the FI -> gratis-fraction map (fixed-point, SCALE = 10^6) from each
/// tribute's nominal amount and fidelity index. It uses only integer math. The result is
/// deterministic across nodes.
///
/// `nominal_amounts` and `tribute_fis` are index-aligned: entry `i` is the
/// nominal interest and fidelity index of the same tribute. `total_interest` is
/// the sum of all `nominal_amounts` (the caller precomputes it).
#[cfg(test)]
pub(crate) fn compute_fi_fraction_map(
    nominal_amounts: &[U256],
    tribute_fis: &[u16],
    total_interest: U256,
    lysis_limit_minor: U256,
) -> Result<std::collections::HashMap<u16, U256>> {
    program_v1::compute_fraction_hash_map(
        nominal_amounts,
        tribute_fis,
        total_interest,
        lysis_limit_minor,
    )
    .map_err(program_error)
}

#[cfg(test)]
fn program_error(error: ProgramErrorV1) -> PrecompileError {
    PrecompileError::BodyReadCorruption(error.to_string())
}

/// Freeze, once per WorldwideDay, the finalized VWAPs of the UTC day before its scheduled
/// processing `process_time`. Oracle COEN/ISO prices already use six-decimal Gratis units.
pub fn freeze_entry_price_snapshot(
    storage: StorageHandle,
    day: WorldwideDay,
    process_time: u64,
) -> Result<BTreeMap<u16, U256>> {
    if let Some(prices) = outbe_nod::api::entry_price_snapshot(storage.clone(), day)? {
        return Ok(prices);
    }
    let previous_day = previous_date_key(timestamp_to_date_key(process_time));
    let mut prices = BTreeMap::new();
    for iso in outbe_oracle::api::reference_currencies(storage.clone())? {
        if let Some(vwap) =
            outbe_oracle::api::get_utc_day_vwap_for_iso(storage.clone(), previous_day, iso)?
        {
            prices.insert(iso, vwap);
        }
    }
    outbe_nod::api::store_entry_price_snapshot(storage, day, previous_day, &prices)?;
    Ok(prices)
}
