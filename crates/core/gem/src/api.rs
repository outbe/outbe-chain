use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::errors::GemError;
use crate::schema::{GemAddParams, GemContract, GemData, GemState};

pub fn add_gem(storage: &StorageHandle<'_>, params: GemAddParams) -> Result<U256> {
    if params.owner.is_zero() {
        return Err(GemError::InvalidOwner.into());
    }

    let mut gem = GemContract::new(storage.clone());
    let gem_id =
        GemContract::generate_gem_id(params.owner, params.gem_load, storage.block_number()?);

    let item = GemData {
        gem_id,
        owner: params.owner,
        gem_type: params.gem_type,
        gem_load: params.gem_load,
        entry_price: params.entry_price,
        cost_amount: params.cost_amount,
        floor_price: params.floor_price,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
        state: params.initial_state as u8,
        issued_at: params.issued_at,
    };
    gem.add_gem(&item)?;
    Ok(gem_id)
}

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

pub fn get_gem(storage: &StorageHandle<'_>, gem_id: U256) -> Result<Option<GemData>> {
    let gem = GemContract::new(storage.clone());
    gem.get_gem(gem_id)
}
