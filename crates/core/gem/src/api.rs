use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::first_full_day;

use crate::errors::GemError;
use crate::schema::{GemAddParams, GemContract, GemData, GemState};

pub fn add_gem(storage: &StorageHandle<'_>, params: GemAddParams) -> Result<U256> {
    if params.owner.is_zero() {
        return Err(GemError::InvalidOwner.into());
    }
    // Only the genesis type is issued without a floor.
    if params.floor_price_minor.is_zero() && params.gem_type != crate::schema::GENESIS_GEM_TYPE {
        return Err(GemError::ZeroFloorPrice.into());
    }

    let mut gem = GemContract::new(storage.clone());
    let params_profile = crate::config::read_from(&gem, storage.chain_id()?)?;
    let gem_id = GemContract::generate_gem_id(
        params.owner,
        params.promis_load_minor,
        storage.block_number()?,
    );

    let item = GemData {
        gem_id,
        owner: params.owner,
        gem_type: params.gem_type,
        promis_load_minor: params.promis_load_minor,
        entry_price_minor: params.entry_price_minor,
        floor_price_minor: params.floor_price_minor,
        call_price_minor: params.call_price_minor,
        call_rate: params.call_rate,
        call_window_seconds: params_profile.call_window_seconds,
        call_threshold_seconds: params_profile.call_threshold_seconds,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
        state: GemState::Issued as u8,
        issued_at: params.issued_at,
        called_at: 0,
        call_notice_period_seconds: params_profile.call_notice_period_seconds,
        settled_at: 0,
    };
    gem.add_gem(&item)?;
    Ok(gem_id)
}

/// Burn a settled gem (promis mining). Forfeit burns of Called gems go through
/// the internal `GemContract::burn` from the daily scan, not this entry point.
pub fn burn(storage: &StorageHandle<'_>, gem_id: U256) -> Result<()> {
    let mut gem = GemContract::new(storage.clone());
    let item = gem.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
    if item.state != GemState::Settled as u8 {
        return Err(GemError::InvalidState.into());
    }
    gem.burn(&item)
}

pub fn set_state(storage: &StorageHandle<'_>, gem_id: U256, new_state: GemState) -> Result<()> {
    let mut gem = GemContract::new(storage.clone());
    gem.set_state(gem_id, new_state)
}

/// Issuance-time privilege bit. Absent storage reads as zero.
const ISSUED_BEFORE_FIRST_WWD: u8 = 1;

/// Records this Genesis issue's classification. `false` writes 0 and replaces a
/// privilege left on an id that burn freed in the same block. A later day does not
/// rewrite a gem that is not issued again.
pub fn record_genesis_issuance_privilege(
    storage: &StorageHandle<'_>,
    gem_id: U256,
    issued_before_first_wwd: bool,
) -> Result<()> {
    let value = if issued_before_first_wwd {
        ISSUED_BEFORE_FIRST_WWD
    } else {
        0
    };
    GemContract::new(storage.clone())
        .issued_before_first_wwd
        .write(&gem_id, value)
}

/// Qualified once a finalized daily VWAP closed above the floor. A zero floor clears on the
/// first eligible full day, since every positive price exceeds it.
///
/// A Genesis gem issued before any Worldwide Day keeps that privilege from
/// issuance. The bit is not recomputed from the creation count or from days
/// retained now.
pub fn is_qualified(storage: &StorageHandle<'_>, item: &GemData) -> Result<bool> {
    if item.gem_type == crate::schema::GENESIS_GEM_TYPE
        && GemContract::new(storage.clone())
            .issued_before_first_wwd
            .read(&item.gem_id)?
            == ISSUED_BEFORE_FIRST_WWD
    {
        return Ok(true);
    }
    outbe_oracle::api::closed_above_floor(
        storage.clone(),
        item.reference_currency,
        item.floor_price_minor,
        first_full_day(item.issued_at),
    )
}

pub fn get_gem(storage: &StorageHandle<'_>, gem_id: U256) -> Result<Option<GemData>> {
    let gem = GemContract::new(storage.clone());
    gem.get_gem(gem_id)
}

/// The call bucket a gem belongs to; zero once it left one, or for a gem from before buckets.
pub fn bucket_of(storage: &StorageHandle<'_>, gem_id: U256) -> Result<alloy_primitives::B256> {
    GemContract::new(storage.clone()).gem_bucket.read(&gem_id)
}
