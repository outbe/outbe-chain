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

pub fn mint_gem_position(
    storage: &StorageHandle<'_>,
    merchant: Address,
    source_intex_id: u32,
    amount: U256,
) -> Result<U256> {
    runtime::mint_gem_position(storage, merchant, source_intex_id, amount)
}

pub fn mint_merchant_gem(
    storage: &StorageHandle<'_>,
    position_id: U256,
    owner: Address,
    gem_load: U256,
) -> Result<U256> {
    runtime::mint_merchant_gem(storage, position_id, owner, gem_load)
}

pub fn settle_gem(storage: &StorageHandle<'_>, caller: Address, gem_id: U256) -> Result<()> {
    runtime::settle_gem(storage, caller, gem_id)
}

pub fn mine_gem_promis(
    storage: &StorageHandle<'_>,
    caller: Address,
    gem_id: U256,
    nonce: U256,
    auth: outbe_promisfactory::api::ModifyAuth,
) -> Result<U256> {
    runtime::mine_gem_promis(storage, caller, gem_id, nonce, auth)
}
