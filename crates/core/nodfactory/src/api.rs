//! Cross-module NodFactory API.

use alloy_primitives::{Address, U256};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_nod::schema::NodIssueParams;
use outbe_ocomp_protocol::{nod_materialization::NodMaterializationBatchV1, SchemaLimits};
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::runtime;

pub use crate::certified::{install_certified_generation, CertifiedNodGenerationV1};
pub use crate::materialization::NodMaterializationOutcomeV1;

pub fn issue_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    params: &NodIssueParams,
) -> Result<WwdEntityId> {
    runtime::issue_nod(storage, scope, parent, params)
}

pub fn settle_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    payer: Address,
    nod_id: WwdEntityId,
) -> Result<U256> {
    runtime::settle_nod(storage, scope, parent, payer, nod_id)
}

pub fn mine_gratis(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    caller: Address,
    nod_id: WwdEntityId,
    nonce: U256,
    auth: outbe_gratisfactory::api::ModifyAuth,
) -> Result<U256> {
    runtime::mine_gratis(storage, scope, parent, caller, nod_id, nonce, auth)
}

/// Authorizes and atomically applies one canonical certified-NOD batch.
pub fn materialize_certified_nods(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    caller: Address,
    batch: &NodMaterializationBatchV1,
    limits: &SchemaLimits,
) -> Result<NodMaterializationOutcomeV1> {
    crate::materialization::authorize_materializer(storage.clone(), caller)?;
    let profile = outbe_chain_constants::NodMaterializationProfileV1 {
        batch_subtree_height: outbe_chain_constants::get_nod_materialization_batch_subtree_height(),
        retry_interval_blocks: outbe_chain_constants::get_nod_materialization_retry_interval_blocks(
        ),
        max_attempts_per_block:
            outbe_chain_constants::get_nod_materialization_max_attempts_per_block(),
    };
    crate::materialization::materialize_certified_nods_authorized(
        storage, scope, parent, batch, profile, limits,
    )
}
