//! Cross-module NodFactory API.

use alloy_primitives::{Address, U256};
use outbe_compressed_entities::{EntityId36, ExecutionScope, ParentBodySource};
use outbe_nod::schema::NodIssueParams;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::runtime;

pub fn issue_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    params: &NodIssueParams,
) -> Result<EntityId36> {
    runtime::issue_nod(storage, scope, parent, params)
}

pub fn mine_gratis(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    caller: Address,
    nod_id: EntityId36,
    nonce: U256,
    asset: Address,
) -> Result<U256> {
    runtime::mine_gratis(storage, scope, parent, caller, nod_id, nonce, asset)
}
