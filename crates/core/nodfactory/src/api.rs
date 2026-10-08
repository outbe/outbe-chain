//! Cross-module NodFactory API.

use alloy_primitives::{Address, Bytes, U256};
use outbe_compressed_entities::{ExecutionReaders, ExecutionScope, ParentBodySource, WwdEntityId};
#[cfg(any(test, feature = "test-utils"))]
use outbe_ocomp_protocol::nod_materialization::NodMaterializationBatchV1;
use outbe_ocomp_protocol::{nod_materialization::ProtectedNodMaterializationV2, SchemaLimits};
use outbe_primitives::nod_encryption::EncryptedNodV2;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::runtime;

pub use crate::runtime::{MineGratisRequest, SettleNodRequest};

pub use crate::certified::{install_certified_generation, CertifiedNodGenerationV1};
pub use crate::materialization::NodMaterializationOutcomeV1;

pub fn issue_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    encrypted: &EncryptedNodV2,
) -> Result<WwdEntityId> {
    runtime::issue_nod(storage, scope, parent, encrypted)
}

pub fn mine_gratis(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    request: MineGratisRequest,
) -> Result<Bytes> {
    runtime::mine_gratis(storage, scope, parent, request)
}

/// Authorizes and atomically applies one canonical certified-NOD batch.
#[cfg(any(test, feature = "test-utils"))]
pub fn materialize_certified_nods(
    storage: &StorageHandle<'_>,
    readers: ExecutionReaders<'_, '_, impl ParentBodySource>,
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
        storage, readers, batch, profile, limits,
    )
}

/// A protected carrier and the schema limits used to validate it.
pub struct ProtectedMaterializationRequest<'a> {
    pub carrier: &'a ProtectedNodMaterializationV2,
    pub limits: &'a SchemaLimits,
}

/// Authorizes and atomically materializes a protected encrypted batch.
pub fn materialize_encrypted_certified_nods(
    storage: &StorageHandle<'_>,
    readers: ExecutionReaders<'_, '_, impl ParentBodySource>,
    caller: Address,
    request: ProtectedMaterializationRequest<'_>,
) -> Result<NodMaterializationOutcomeV1> {
    let ExecutionReaders { scope, parent } = readers;
    let ProtectedMaterializationRequest { carrier, limits } = request;
    crate::materialization::authorize_materializer(storage.clone(), caller)?;
    let profile = outbe_chain_constants::NodMaterializationProfileV1 {
        batch_subtree_height: outbe_chain_constants::get_nod_materialization_batch_subtree_height(),
        retry_interval_blocks: outbe_chain_constants::get_nod_materialization_retry_interval_blocks(
        ),
        max_attempts_per_block:
            outbe_chain_constants::get_nod_materialization_max_attempts_per_block(),
    };
    storage.clone().with_checkpoint(|| {
        crate::materialization::consume_materialization_attempt(storage, profile)?;
        crate::materialization::materialize_protected_after_attempt(
            storage,
            scope,
            parent,
            carrier,
            crate::materialization::MaterializationRules { profile, limits },
        )
    })
}

/// What settling `nod_id` with `asset` costs, which of the Nod's two currencies
/// that asset settles on, and the VWAP snapshot an issuance-currency payment
/// must name.
pub fn quote_settlement(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    nod_id: WwdEntityId,
    asset: Address,
) -> Result<(u16, U256, U256)> {
    runtime::quote_settlement(storage, scope, parent, nod_id, asset)
}

/// Pays a qualified or called Nod's cost in ERC20 base units.
pub fn settle_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    request: SettleNodRequest,
) -> Result<()> {
    runtime::settle_nod(storage, scope, parent, request)
}
