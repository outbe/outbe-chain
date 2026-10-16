//! Canonical proof-backed materialization of certified NOD generations.

#[cfg(any(test, feature = "test-utils"))]
use alloy_primitives::U256;
use alloy_primitives::{Address, Bytes};
use alloy_sol_types::{SolError, SolEvent};
use outbe_chain_constants::NodMaterializationProfileV1;
use outbe_nod::NodContract;
#[cfg(any(test, feature = "test-utils"))]
use outbe_nod::NodIssueParams;
#[cfg(any(test, feature = "test-utils"))]
use outbe_ocomp_protocol::nod_materialization::{
    verify_nod_materialization_batch, NodMaterializationBatchV1,
};
#[cfg(any(test, feature = "test-utils"))]
use outbe_ocomp_protocol::result::NodActionV1;
use outbe_ocomp_protocol::{
    nod_materialization::{NodMaterializationHeadV1, ProtectedNodMaterializationV2},
    SchemaLimits,
};
use outbe_primitives::time::WorldwideDay;
#[cfg(any(test, feature = "test-utils"))]
use outbe_primitives::wwd_entity_id::WwdEntityId;
use outbe_primitives::{
    addresses::NOD_FACTORY_ADDRESS,
    error::{PrecompileError, Result},
    storage::StorageHandle,
};
use outbe_validatorset::delegation::ValidatorDelegateRole;

use crate::{errors::NodFactoryError, precompile::INodFactory, runtime};

/// Stable typed rejection codes used by the system-carrier failure boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NodMaterializationRejectionV1 {
    StaleQueueSequence = 1,
    StaleCursor = 2,
    AttemptLimit = 3,
    InvalidBatchShape = 4,
    InvalidProof = 5,
    DuplicateNod = 6,
    UnauthorizedSigner = 7,
}

/// Receipt-like local result of one successfully committed materialization batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodMaterializationOutcomeV1 {
    pub queue_sequence: u64,
    pub worldwide_day: u32,
    pub first_nod_ordinal: u32,
    pub next_nod_ordinal: u32,
    pub completed: bool,
}

/// Canonical ABI revert bytes for one typed materialization rejection.
#[must_use]
pub fn materialization_revert_data(rejection: NodMaterializationRejectionV1) -> Bytes {
    Bytes::from(
        INodFactory::NodMaterializationRejected {
            code: rejection as u8,
        }
        .abi_encode(),
    )
}

/// True only for the six race/proof failures that may become status-zero
/// system-carrier receipts without invalidating the surrounding block.
#[must_use]
pub fn is_soft_materialization_revert_data(data: &[u8]) -> bool {
    [
        NodMaterializationRejectionV1::StaleQueueSequence,
        NodMaterializationRejectionV1::StaleCursor,
        NodMaterializationRejectionV1::AttemptLimit,
        NodMaterializationRejectionV1::InvalidBatchShape,
        NodMaterializationRejectionV1::InvalidProof,
        NodMaterializationRejectionV1::DuplicateNod,
    ]
    .into_iter()
    .any(|rejection| data == materialization_revert_data(rejection).as_ref())
}

/// Maps the public handler's errors onto typed ABI reverts. Any other deterministic
/// rejection becomes `InvalidProof`, a status-zero receipt rather than a failed block.
pub(crate) fn typed_materialization_error(error: PrecompileError) -> PrecompileError {
    let rejection = match &error {
        PrecompileError::Revert(reason) => known_rejection(reason),
        PrecompileError::BodyReadCorruption(_) => None,
        _ => return error,
    }
    .unwrap_or_else(|| {
        tracing::warn!(target: "outbe::nodfactory", %error, "materialization carrier rejected");
        NodMaterializationRejectionV1::InvalidProof
    });
    PrecompileError::RevertBytes(materialization_revert_data(rejection))
}

fn known_rejection(reason: &str) -> Option<NodMaterializationRejectionV1> {
    let rejection = if reason == NodFactoryError::UnauthorizedMaterializer.to_string() {
        NodMaterializationRejectionV1::UnauthorizedSigner
    } else if reason == NodFactoryError::MaterializationAttemptLimit.to_string() {
        NodMaterializationRejectionV1::AttemptLimit
    } else if reason == NodFactoryError::StaleMaterializationQueue.to_string() {
        NodMaterializationRejectionV1::StaleQueueSequence
    } else if reason == NodFactoryError::StaleMaterializationCursor.to_string() {
        NodMaterializationRejectionV1::StaleCursor
    } else if reason == NodFactoryError::InvalidMaterializationBatchShape.to_string() {
        NodMaterializationRejectionV1::InvalidBatchShape
    } else if reason == NodFactoryError::InvalidMaterializationProof.to_string() {
        NodMaterializationRejectionV1::InvalidProof
    } else if reason == NodFactoryError::DuplicateMaterializedNod.to_string() {
        NodMaterializationRejectionV1::DuplicateNod
    } else {
        return None;
    };
    Some(rejection)
}

/// Repeats current ACTIVE OCOMP-role authorization at the execution owner.
pub fn authorize_materializer(storage: StorageHandle<'_>, caller: Address) -> Result<()> {
    let authorized = outbe_validatorset::contract::ValidatorSet::new(storage)
        .resolve_validator_for_role(caller, ValidatorDelegateRole::Ocomp)?
        .is_some();
    if !authorized {
        return Err(NodFactoryError::UnauthorizedMaterializer.into());
    }
    Ok(())
}

/// Consumes the canonical per-block allowance before proof work.
pub fn consume_materialization_attempt(
    storage: &StorageHandle<'_>,
    profile: NodMaterializationProfileV1,
) -> Result<()> {
    let block_height = storage.block_number()?;
    let nod = NodContract::new(storage.clone());
    let stored_height = nod.ocomp_materialization_attempt_height.read()?;
    let stored_count = nod.ocomp_materialization_attempt_count.read()?;
    let next_count = if stored_height == block_height {
        stored_count
            .checked_add(1)
            .ok_or(NodFactoryError::MaterializationAttemptCounterOverflow)?
    } else {
        1
    };
    if next_count > profile.max_attempts_per_block {
        return Err(NodFactoryError::MaterializationAttemptLimit.into());
    }
    nod.ocomp_materialization_attempt_height
        .write(block_height)?;
    nod.ocomp_materialization_attempt_count.write(next_count)
}

/// A certified batch, the load of each of its Nods and the schema limits that validate it.
#[cfg(any(test, feature = "test-utils"))]
pub struct CertifiedMaterializationRequest<'a> {
    pub batch: &'a NodMaterializationBatchV1,
    pub loads: &'a [U256],
    pub limits: &'a SchemaLimits,
}

/// Applies one already-authorized canonical batch atomically.
#[cfg(any(test, feature = "test-utils"))]
pub fn materialize_certified_nods_authorized(
    storage: &StorageHandle<'_>,
    request: CertifiedMaterializationRequest<'_>,
    profile: NodMaterializationProfileV1,
) -> Result<NodMaterializationOutcomeV1> {
    storage.clone().with_checkpoint(|| {
        consume_materialization_attempt(storage, profile)?;
        materialize_after_attempt(storage, request, profile)
    })
}

#[cfg(any(test, feature = "test-utils"))]
pub(crate) fn materialize_after_attempt(
    storage: &StorageHandle<'_>,
    request: CertifiedMaterializationRequest<'_>,
    profile: NodMaterializationProfileV1,
) -> Result<NodMaterializationOutcomeV1> {
    let CertifiedMaterializationRequest {
        batch,
        loads,
        limits,
    } = request;
    let nod = NodContract::new(storage.clone());
    let head = nod
        .ocomp_materialization_head()?
        .ok_or(NodFactoryError::StaleMaterializationQueue)?;
    if batch.queue_sequence != head.queue_sequence {
        return Err(NodFactoryError::StaleMaterializationQueue.into());
    }
    if batch.first_nod_ordinal != head.next_nod_ordinal {
        return Err(NodFactoryError::StaleMaterializationCursor.into());
    }
    let verified =
        verify_nod_materialization_batch(batch, &head, profile.batch_subtree_height, limits)
            .map_err(|_| PrecompileError::from(NodFactoryError::InvalidMaterializationProof))?;

    let worldwide_day = WorldwideDay::new(head.worldwide_day);
    // Read before the last batch clears it: every Nod carries the certified Lysis freeze instant.
    let issued_at = nod
        .ocomp_certified_generation(worldwide_day)?
        .ok_or(NodFactoryError::MaterializationHeadProjectionMissing)?
        .issued_at;
    require_derived_nod_ids(verified.actions(), worldwide_day)?;

    if loads.len() != verified.actions().len() || loads.iter().any(|load| load.is_zero()) {
        return Err(NodFactoryError::MaterializationTestLoadMismatch.into());
    }
    for (action, load) in verified.actions().iter().zip(loads) {
        let params = NodIssueParams {
            owner: action.owner,
            worldwide_day,
            league_id: action.league_id,
            gratis_load_minor: *load,
            entry_price_minor: action.entry_price_minor,
            issuance_currency: action.issuance_currency,
            reference_currency: action.reference_currency,
        };
        let encrypted = outbe_nod::test_support::encrypted_fixture(&params, storage.chain_id()?);
        runtime::issue_nod_at(storage, &encrypted, issued_at)
            .map_err(duplicate_materialized_nod)?;
    }

    finish_materialization(storage, &head, verified.actions().len())
}

pub(crate) struct MaterializationRules<'a> {
    pub profile: NodMaterializationProfileV1,
    pub limits: &'a SchemaLimits,
}

/// Verifies the private witness against authority read from this node's chain state.
pub(crate) fn materialize_protected_after_attempt(
    storage: &StorageHandle<'_>,
    carrier: &ProtectedNodMaterializationV2,
    rules: MaterializationRules<'_>,
) -> Result<NodMaterializationOutcomeV1> {
    let nod = NodContract::new(storage.clone());
    let head = nod
        .ocomp_materialization_head()?
        .ok_or(NodFactoryError::StaleMaterializationQueue)?;
    validate_carrier_cursor(carrier, &head)?;
    let authority = state_materialization_authority(
        storage,
        &head,
        carrier,
        rules.profile.batch_subtree_height,
        rules.limits,
    )?;
    let _context = outbe_tee::call_context::ContextScope::from_storage(storage)?;
    let encrypted = outbe_tee::nod_state_materialization::open_state_nods(&authority, carrier)
        .map_err(materialization_enclave_error)?;
    validate_encrypted_materialization(
        &encrypted,
        carrier,
        authority.source.chain_id,
        head.worldwide_day,
    )?;
    let issued_at = nod
        .ocomp_certified_generation(WorldwideDay::new(head.worldwide_day))?
        .ok_or(NodFactoryError::MaterializationHeadProjectionMissing)?
        .issued_at;
    for body in &encrypted {
        runtime::issue_nod_at(storage, body, issued_at).map_err(duplicate_materialized_nod)?;
    }
    finish_materialization(storage, &head, encrypted.len())
}

fn validate_carrier_cursor(
    carrier: &ProtectedNodMaterializationV2,
    head: &NodMaterializationHeadV1,
) -> Result<()> {
    if carrier.queue_sequence != head.queue_sequence {
        return Err(NodFactoryError::StaleMaterializationQueue.into());
    }
    if carrier.first_nod_ordinal != head.next_nod_ordinal {
        return Err(NodFactoryError::StaleMaterializationCursor.into());
    }
    Ok(())
}

/// Reconstructs state-source semantics from retained local records. Public
/// ciphertext terms select lookups only; enclave open must still authenticate
/// the complete carrier against this authority before any Nod can be issued.
pub fn state_materialization_authority(
    storage: &StorageHandle<'_>,
    head: &NodMaterializationHeadV1,
    carrier: &ProtectedNodMaterializationV2,
    subtree_height: u8,
    limits: &SchemaLimits,
) -> Result<outbe_tee::nod_state_materialization::StateNodAuthority> {
    use outbe_tee::{
        nod_state_materialization::StateNodAuthority,
        tribute_source::{TributeSourceCheckpoint, TributeSourceSubject},
    };
    let invalid = || PrecompileError::from(NodFactoryError::InvalidMaterializationProof);
    let generation = NodContract::new(storage.clone())
        .ocomp_certified_generation(WorldwideDay::new(head.worldwide_day))?
        .ok_or_else(invalid)?;
    if carrier.encrypted_nods.is_empty()
        || carrier.encrypted_nods.len()
            > outbe_ocomp_protocol::nod_materialization::MAX_NOD_MATERIALIZATION_ACTIONS
        || carrier.encrypted_nods.len()
            > head.nod_count.saturating_sub(head.next_nod_ordinal) as usize
    {
        return Err(invalid());
    }
    let checkpoint = generation.source_checkpoint;
    let source = TributeSourceSubject {
        checkpoint: TributeSourceCheckpoint {
            block_number: checkpoint.block_number,
            block_hash: checkpoint.block_hash,
            state_root: checkpoint.state_root,
        },
        chain_id: storage.chain_id()?,
        worldwide_day: generation.worldwide_day,
        tribute_count: generation.tribute_count,
    };
    let subject = outbe_tribute::state_proof::TributeStateProofSubject::from(source);
    let mut source_bindings = Vec::with_capacity(carrier.encrypted_nods.len());
    let mut seen = std::collections::BTreeSet::new();
    for encoded in &carrier.encrypted_nods {
        if encoded.0.len() > limits.max_bounded_bytes {
            return Err(invalid());
        }
        let body: outbe_primitives::nod_encryption::EncryptedNodV2 =
            serde_json::from_slice(&encoded.0).map_err(|_| invalid())?;
        let terms = &body.terms;
        let id = outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(
            terms.owner,
            source.worldwide_day,
        )
        .map_err(|_| invalid())?;
        if terms.chain_id != source.chain_id
            || terms.worldwide_day != source.worldwide_day
            || terms.nod_id != id
            || !seen.insert(id)
        {
            return Err(invalid());
        }
        source_bindings.push(outbe_tribute::state_proof::retained_source_binding(
            storage, &subject, id,
        )?);
    }
    Ok(StateNodAuthority {
        source,
        head: head.encode_canonical(limits).map_err(|_| invalid())?,
        subtree_height,
        source_bindings,
        nod_gratis_load_max: generation.nod_gratis_load_max,
    })
}

fn materialization_enclave_error(error: outbe_tee::TransportError) -> PrecompileError {
    match error {
        outbe_tee::TransportError::NodMaterializationRejected(_) => {
            NodFactoryError::InvalidMaterializationProof.into()
        }
        other => NodFactoryError::materialization_enclave(other).into(),
    }
}

fn validate_encrypted_materialization(
    encrypted: &[outbe_primitives::nod_encryption::EncryptedNodV2],
    carrier: &ProtectedNodMaterializationV2,
    chain_id: u64,
    day: u32,
) -> Result<()> {
    let valid = !encrypted.is_empty()
        && encrypted.len() == carrier.encrypted_nods.len()
        && encrypted.iter().all(|body| {
            let terms = &body.terms;
            (terms.chain_id, terms.worldwide_day.value()) == (chain_id, day)
                && !terms.owner.is_zero()
                && outbe_nod::pricing::is_issuable_entry(terms.entry_price_minor)
        });
    if !valid {
        return Err(NodFactoryError::InvalidMaterializationProof.into());
    }
    Ok(())
}

fn finish_materialization(
    storage: &StorageHandle<'_>,
    head: &NodMaterializationHeadV1,
    count: usize,
) -> Result<NodMaterializationOutcomeV1> {
    let nod = NodContract::new(storage.clone());
    let worldwide_day = WorldwideDay::new(head.worldwide_day);
    let action_count =
        u32::try_from(count).map_err(|_| NodFactoryError::MaterializationActionCountOverflow)?;
    let next_nod_ordinal = head
        .next_nod_ordinal
        .checked_add(action_count)
        .ok_or(NodFactoryError::MaterializationCursorOverflow)?;
    if next_nod_ordinal > head.nod_count {
        return Err(NodFactoryError::MaterializationCursorExceedsCount.into());
    }
    let block_number = storage.block_number()?;
    let completed = next_nod_ordinal == head.nod_count;
    if completed {
        let stored_head = nod.ocomp_materialization_head_sequence.read()?;
        let stored_tail = nod.ocomp_materialization_tail_sequence.read()?;
        if stored_head != head.queue_sequence || stored_head >= stored_tail {
            return Err(NodFactoryError::MaterializationFifoChanged.into());
        }
        let next_head = stored_head
            .checked_add(1)
            .ok_or(NodFactoryError::MaterializationFifoHeadOverflow)?;
        nod.ocomp_materialization_queue_wwd
            .write(&stored_head, WorldwideDay::new(0))?;
        nod.ocomp_materialization_head_sequence.write(next_head)?;
        nod.clear_ocomp_certified_generation(worldwide_day)?;
    } else {
        nod.ocomp_materialization_next_nod_ordinal
            .write(&worldwide_day, next_nod_ordinal)?;
        nod.ocomp_materialization_last_progress_height
            .write(&worldwide_day, block_number)?;
    }

    storage.emit_event(
        NOD_FACTORY_ADDRESS,
        INodFactory::NodMaterializationProgress {
            queueSequence: head.queue_sequence,
            worldwideDay: head.worldwide_day,
            generation: head.generation,
            firstNodOrdinal: head.next_nod_ordinal,
            nextNodOrdinal: next_nod_ordinal,
            completed,
            blockNumber: block_number,
        }
        .encode_log_data(),
    )?;

    Ok(NodMaterializationOutcomeV1 {
        queue_sequence: head.queue_sequence,
        worldwide_day: head.worldwide_day,
        first_nod_ordinal: head.next_nod_ordinal,
        next_nod_ordinal,
        completed,
    })
}

#[cfg(any(test, feature = "test-utils"))]
fn require_derived_nod_ids(actions: &[NodActionV1], worldwide_day: WorldwideDay) -> Result<()> {
    for action in actions {
        let derived_nod_id = outbe_nod::identity::generate_nod_id(action.owner, worldwide_day)?;
        let supplied_nod_id = WwdEntityId::try_from(action.nod_id.0.as_slice())
            .map_err(|_| PrecompileError::from(NodFactoryError::InvalidMaterializationProof))?;
        if supplied_nod_id != derived_nod_id
            || !outbe_nod::pricing::is_issuable_entry(action.entry_price_minor)
        {
            return Err(NodFactoryError::InvalidMaterializationProof.into());
        }
    }
    Ok(())
}

fn duplicate_materialized_nod(error: PrecompileError) -> PrecompileError {
    if NodFactoryError::revert_reason(&error)
        .is_some_and(|reason| reason == NodFactoryError::NodAlreadyExists.to_string())
    {
        NodFactoryError::DuplicateMaterializedNod.into()
    } else {
        error
    }
}

/// Encodes the exact current public head response.
pub fn encode_materialization_head(
    head: Option<NodMaterializationHeadV1>,
    limits: &SchemaLimits,
) -> Result<(bool, Bytes)> {
    match head {
        Some(head) => head
            .encode_canonical(limits)
            .map(|bytes| (true, bytes.into()))
            .map_err(|error| NodFactoryError::EncodeMaterializationHead(error).into()),
        None => Ok((false, Bytes::new())),
    }
}
