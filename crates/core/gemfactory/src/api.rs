use alloy_primitives::{Address, U256};
use outbe_intex::SeriesId;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::GemIssueParams;

pub fn issue_gem(storage: &StorageHandle<'_>, params: GemIssueParams) -> Result<U256> {
    runtime::issue_gem(storage, params)
}

pub fn issue_gem_position(
    storage: &StorageHandle<'_>,
    caller: Address,
    source_intex_id: SeriesId,
    units: U256,
) -> Result<U256> {
    runtime::issue_gem_position(storage, caller, source_intex_id, units)
}

pub fn issue_merchant_gem(
    storage: &StorageHandle<'_>,
    caller: Address,
    position_id: U256,
    owner: Address,
    promis_load: U256,
) -> Result<U256> {
    runtime::issue_merchant_gem(storage, caller, position_id, owner, promis_load)
}

pub fn settle_gem(
    storage: &StorageHandle<'_>,
    caller: Address,
    gem_id: U256,
    asset: Address,
    snapshot_id: U256,
) -> Result<()> {
    runtime::settle_gem(storage, caller, gem_id, asset, snapshot_id)
}

pub fn mine_promis(
    storage: &StorageHandle<'_>,
    gem_id: U256,
    nonce: u64,
    auth: outbe_promisfactory::api::ModifyAuth,
) -> Result<U256> {
    runtime::mine_promis(storage, gem_id, nonce, auth)
}
