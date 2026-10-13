use alloy_primitives::{Address, U256};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_intex::SeriesId;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::GemIssueParams;

pub fn issue_gem(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    params: GemIssueParams,
) -> Result<U256> {
    runtime::issue_gem(storage, scope, parent, params)
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
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    params: crate::schema::MerchantGemIssueParams,
) -> Result<U256> {
    runtime::issue_merchant_gem(storage, scope, parent, params)
}

pub fn settle_gem(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    params: crate::schema::GemSettlementParams,
) -> Result<()> {
    runtime::settle_gem(storage, scope, parent, params)
}

pub fn mine_promis(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    params: crate::schema::GemMiningProof,
    auth: outbe_promisfactory::api::ModifyAuth,
) -> Result<U256> {
    runtime::mine_promis(storage, scope, parent, params, auth)
}
