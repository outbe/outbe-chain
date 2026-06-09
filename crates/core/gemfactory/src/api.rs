use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::GemTypes;

pub fn mint_gem(
    storage: &StorageHandle<'_>,
    owner: Address,
    gem_type: GemTypes,
    gem_load: U256,
    issuance_currency: u16,
    reference_currency: u16,
) -> Result<U256> {
    runtime::mint_gem(
        storage,
        owner,
        gem_type,
        gem_load,
        issuance_currency,
        reference_currency,
    )
}

pub fn settle_gem(storage: &StorageHandle<'_>, caller: Address, gem_id: U256) -> Result<()> {
    runtime::settle_gem(storage, caller, gem_id)
}

pub fn mine_gem_promis(
    storage: &StorageHandle<'_>,
    caller: Address,
    gem_id: U256,
    nonce: U256,
) -> Result<U256> {
    runtime::mine_gem_promis(storage, caller, gem_id, nonce)
}
