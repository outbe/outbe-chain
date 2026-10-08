mod body;
mod pagination;

use body::{prepare_input, verify_stored};

use std::collections::BTreeSet;

use alloy_primitives::{Bytes, B256};
use alloy_sol_types::{sol, SolEvent};
use outbe_primitives::{
    addresses::{NOD_ADDRESS, TRIBUTE_ADDRESS},
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

use crate::{
    api::{
        nod_bucket_payload, nod_item_payload, tribute_payload, BodyInput, EntityRef,
        ExecutionScope, IdPageRequest, ParentBodySource, QueryRef, VerifiedBody, VerifiedBodyPage,
        MAX_ID_PAGE_LIMIT,
    },
    body_commitment, decode_nod_bucket_v1, decode_nod_item_v1, decode_tribute_v1,
    encode_nod_bucket_v1, encode_nod_item_v1, encode_tribute_v1,
    schema::{Collection, DeltaStatus, IndexKind, IndexRecord, PendingWord},
    state::State,
    Commitment, NodBucketBodyV1, NodItemBodyV1, StoredBody, TributeBodyV1, WwdEntityId,
    ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};

// Generated ABI methods follow the Solidity argument lists.
#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types)]
        "../../../contracts/precompiles/src/ITribute.sol"
    );
}
pub use abi::ITribute;
sol!(
    #![sol(alloy_sol_types = alloy_sol_types)]
    "../../../contracts/precompiles/src/INod.sol"
);

pub(crate) use INod::{NodBodyDeleted, NodBodyStored, NodBucketBodyDeleted, NodBucketBodyStored};
pub(crate) use ITribute::{TributeBodyDeleted, TributeBodyStored, TributePartitionRetired};

pub(crate) const READ_FIXED_GAS: u64 = 200;
pub(crate) const READ_GAS_PER_CANONICAL_BYTE: u64 = 8;
pub(crate) const INDEX_RECORD_SCAN_GAS: u64 = 300;
pub(crate) const PARENT_ID_GAS: u64 = 120;

struct PreparedBody {
    collection: Collection,
    entity_id: WwdEntityId,
    stored_body: StoredBody,
    commitment: Commitment,
    memberships: Vec<IndexRecord>,
}

impl PreparedBody {
    fn entity_ref(&self) -> EntityRef {
        match self.collection {
            Collection::Tribute => EntityRef::Tribute(self.entity_id),
            Collection::NodItem => EntityRef::NodItem(self.entity_id),
            Collection::NodBucket => EntityRef::NodBucket(self.entity_id),
        }
    }
}

pub(crate) fn read(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    entity: EntityRef,
) -> Result<Option<VerifiedBody>> {
    let (collection, entity_id) = entity_parts(entity);
    let state = State::new(storage.clone());
    let (_, pending, pending_body) = state.pending(collection, entity_id)?;
    match pending {
        PendingWord::Set(commitment) => {
            charge_body_read(&storage, scope, pending_body.len())?;
            let stored = crate::decode_stored_body(&pending_body)
                .map_err(|error| fatal(format!("invalid pending StoredBody: {error}")))?;
            verify_stored(entity, stored, commitment, BodyOrigin::Overlay).map(Some)
        }
        PendingWord::Deleted => Ok(None),
        PendingWord::Untouched => {
            let parent_root = state.root()?;
            let Some(commitment) = scope.read_parent_leaf_verified(entity, parent_root)? else {
                return Ok(None);
            };
            let stored = parent
                .get(entity)
                .map_err(PrecompileError::from)?
                .ok_or_else(|| {
                    PrecompileError::BodyReadCorruption(format!(
                        "committed body {entity_id} is missing from finalized parent"
                    ))
                })?;
            charge_body_read(&storage, scope, stored.encode().len())?;
            verify_stored(entity, stored, commitment, BodyOrigin::Parent)
                .map(Some)
                .map_err(|error| match error {
                    PrecompileError::BodyReadCorruption(message) => {
                        PrecompileError::BodyReadCorruption(format!(
                            "{message}; [CE_BODY_DIAGNOSTIC] evm_block={:?} evm_ce_root={parent_root} parent_root={:?} {} thread={:?}",
                            storage.block_number(),
                            scope.parent_root(),
                            scope.diagnostic_parent_binding(),
                            std::thread::current().id(),
                        ))
                    }
                    other => other,
                })
        }
    }
}

pub(crate) fn mint(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    new_body: BodyInput<'_>,
) -> Result<()> {
    scope.require_active()?;
    let prepared = prepare_input(new_body)?;
    let state = State::new(storage.clone());
    if current_commitment(scope, &state, prepared.collection, prepared.entity_id)?.is_some() {
        return Err(revert("compressed entity already exists"));
    }

    let locator = state.prepare_body_touch(scope, prepared.collection, prepared.entity_id)?;
    state.set_pending_prepared(locator, prepared.commitment, &prepared.stored_body.encode())?;
    for membership in &prepared.memberships {
        state.apply_index_add(scope, membership)?;
    }
    emit_stored(&storage, &prepared, None)
}

pub(crate) fn update(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    current: VerifiedBody,
    new_body: BodyInput<'_>,
) -> Result<()> {
    scope.require_active()?;
    let prepared = prepare_input(new_body)?;
    if current.entity != prepared.entity_ref() {
        return Err(revert(
            "compressed entity update collection or identity mismatch",
        ));
    }
    let state = State::new(storage.clone());
    require_capability_current(scope, &state, &current)?;

    let old_memberships = memberships_for_verified(&current)?;
    let old_set: BTreeSet<_> = old_memberships.into_iter().collect();
    let new_set: BTreeSet<_> = prepared.memberships.iter().cloned().collect();

    let locator = state.prepare_body_touch(scope, prepared.collection, prepared.entity_id)?;
    state.set_pending_prepared(locator, prepared.commitment, &prepared.stored_body.encode())?;
    for membership in old_set.difference(&new_set) {
        state.apply_index_remove(scope, membership)?;
    }
    for membership in new_set.difference(&old_set) {
        state.apply_index_add(scope, membership)?;
    }
    emit_stored(&storage, &prepared, Some(current.commitment))
}

pub(crate) fn delete(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    current: VerifiedBody,
) -> Result<()> {
    scope.require_active()?;
    let state = State::new(storage.clone());
    require_capability_current(scope, &state, &current)?;
    let (collection, entity_id) = entity_parts(current.entity);
    let memberships = memberships_for_verified(&current)?;

    let locator = state.prepare_body_touch(scope, collection, entity_id)?;
    state.set_deleted_prepared(locator)?;
    for membership in &memberships {
        state.apply_index_remove(scope, membership)?;
    }
    emit_deleted(&storage, current.entity, current.commitment)
}

pub(crate) fn list(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    query: QueryRef,
    request: IdPageRequest,
) -> Result<VerifiedBodyPage> {
    pagination::Pagination {
        storage,
        scope,
        parent,
        query,
        request,
    }
    .read()
}

fn calculate_commitment(entity_id: WwdEntityId, payload: &[u8]) -> Result<Commitment> {
    body_commitment(ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1, entity_id, payload)
        .map_err(|error| fatal(error.to_string()))
}

fn current_commitment(
    scope: &ExecutionScope,
    state: &State<'_>,
    collection: Collection,
    entity_id: WwdEntityId,
) -> Result<Option<Commitment>> {
    let (_, pending, body) = state.pending(collection, entity_id)?;
    match pending {
        PendingWord::Untouched => {
            scope.read_parent_leaf_verified(entity_from_parts(collection, entity_id), state.root()?)
        }
        PendingWord::Set(value) => {
            let stored = crate::decode_stored_body(&body)
                .map_err(|error| fatal(format!("invalid pending StoredBody: {error}")))?;
            verify_stored(
                entity_from_parts(collection, entity_id),
                stored,
                value,
                BodyOrigin::Overlay,
            )?;
            Ok(Some(value))
        }
        PendingWord::Deleted => Ok(None),
    }
}

fn require_capability_current(
    scope: &ExecutionScope,
    state: &State<'_>,
    current: &VerifiedBody,
) -> Result<()> {
    let (collection, entity_id) = entity_parts(current.entity);
    match current_commitment(scope, state, collection, entity_id)? {
        None => Err(revert("compressed entity is absent")),
        Some(actual) if actual != current.commitment => Err(revert(
            "verified body capability no longer matches current value",
        )),
        Some(_) => Ok(()),
    }
}

fn memberships_for_verified(body: &VerifiedBody) -> Result<Vec<IndexRecord>> {
    let id = body.entity_id();
    if let Some(tribute) = body.payload.as_encrypted_tribute() {
        return Ok(vec![
            IndexRecord::owner(IndexKind::TributeByOwner, tribute.context.owner, id),
            IndexRecord::day(tribute.context.worldwide_day, id),
        ]);
    }
    if let Some(tribute) = body.payload.as_tribute() {
        return Ok(vec![
            IndexRecord::owner(IndexKind::TributeByOwner, tribute.owner, id),
            IndexRecord::day(tribute.worldwide_day, id),
        ]);
    }
    if let Some(item) = body.payload.as_encrypted_nod_item() {
        return Ok(vec![
            IndexRecord::owner(IndexKind::NodByOwner, item.encrypted.terms.owner, id),
            IndexRecord::nod_all(id),
        ]);
    }
    if let Some(item) = body.payload.as_nod_item() {
        return Ok(vec![
            IndexRecord::owner(IndexKind::NodByOwner, item.owner, id),
            IndexRecord::nod_all(id),
        ]);
    }
    if body.payload.as_nod_bucket().is_some() {
        return Ok(Vec::new());
    }
    Err(fatal("verified payload has no typed variant"))
}

fn emit_stored(
    storage: &StorageHandle<'_>,
    body: &PreparedBody,
    previous: Option<Commitment>,
) -> Result<()> {
    let previous = commitment_b256(previous);
    let new_commitment = commitment_b256(Some(body.commitment));
    let canonical_payload = Bytes::copy_from_slice(body.stored_body.payload());
    let id = body.entity_id.to_u256();
    let event = match body.collection {
        Collection::Tribute => TributeBodyStored {
            tributeId: id,
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: body.stored_body.schema_version(),
            previousCommitment: previous,
            newCommitment: new_commitment,
            canonicalPayload: canonical_payload,
        }
        .encode_log_data(),
        Collection::NodItem => NodBodyStored {
            nodId: id,
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: body.stored_body.schema_version(),
            previousCommitment: previous,
            newCommitment: new_commitment,
            canonicalPayload: canonical_payload,
        }
        .encode_log_data(),
        Collection::NodBucket => NodBucketBodyStored {
            bucketId: id,
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: body.stored_body.schema_version(),
            previousCommitment: previous,
            newCommitment: new_commitment,
            canonicalPayload: canonical_payload,
        }
        .encode_log_data(),
    };
    let emitter = if body.collection == Collection::Tribute {
        TRIBUTE_ADDRESS
    } else {
        NOD_ADDRESS
    };
    storage.emit_event(emitter, event)
}

fn emit_deleted(
    storage: &StorageHandle<'_>,
    entity: EntityRef,
    previous: Commitment,
) -> Result<()> {
    let previous = commitment_b256(Some(previous));
    let id = entity.entity_id().to_u256();
    let (emitter, event) = match entity {
        EntityRef::Tribute(_) => (
            TRIBUTE_ADDRESS,
            TributeBodyDeleted {
                tributeId: id,
                previousCommitment: previous,
            }
            .encode_log_data(),
        ),
        EntityRef::NodItem(_) => (
            NOD_ADDRESS,
            NodBodyDeleted {
                nodId: id,
                previousCommitment: previous,
            }
            .encode_log_data(),
        ),
        EntityRef::NodBucket(_) => (
            NOD_ADDRESS,
            NodBucketBodyDeleted {
                bucketId: id,
                previousCommitment: previous,
            }
            .encode_log_data(),
        ),
    };
    storage.emit_event(emitter, event)
}

const fn entity_from_parts(collection: Collection, id: WwdEntityId) -> EntityRef {
    match collection {
        Collection::Tribute => EntityRef::Tribute(id),
        Collection::NodItem => EntityRef::NodItem(id),
        Collection::NodBucket => EntityRef::NodBucket(id),
    }
}

const fn entity_parts(entity: EntityRef) -> (Collection, WwdEntityId) {
    match entity {
        EntityRef::Tribute(id) => (Collection::Tribute, id),
        EntityRef::NodItem(id) => (Collection::NodItem, id),
        EntityRef::NodBucket(id) => (Collection::NodBucket, id),
    }
}

fn charge_body_read(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    bytes: usize,
) -> Result<()> {
    let bytes = u64::try_from(bytes).map_err(|_| fatal("body length exceeds gas range"))?;
    scope.deduct_explicit_gas(
        storage,
        READ_FIXED_GAS.saturating_add(READ_GAS_PER_CANONICAL_BYTE.saturating_mul(bytes)),
    )
}

fn commitment_b256(value: Option<Commitment>) -> B256 {
    value.map_or(B256::ZERO, |commitment| B256::from(*commitment.as_bytes()))
}

fn input_error(error: impl core::fmt::Display) -> PrecompileError {
    revert(error.to_string())
}

#[derive(Clone, Copy, Debug)]
enum BodyOrigin {
    Overlay,
    Parent,
}

impl BodyOrigin {
    fn invalid(self, message: impl Into<String>) -> PrecompileError {
        match self {
            Self::Overlay => fatal(message),
            Self::Parent => PrecompileError::BodyReadCorruption(message.into()),
        }
    }
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}

fn revert(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Revert(message.into())
}
