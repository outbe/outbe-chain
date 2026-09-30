use std::collections::BTreeSet;

use alloy_primitives::B256;
use outbe_compressed_entities::{
    body_commitment, encode_nod_bucket_v1, encode_nod_item_v1, encode_tribute_v1, WwdEntityId,
    ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_nod::{NodBucketState, NodItemState, NodRepositoryReader};
use outbe_offchain_storage::{AtomicWriteBatch, StorageMetadata};
use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_tribute::{RetainedTributeReader, TributeData, TributeRepositoryReader};

use super::decode::{
    decode_event, is_projection_pair, validate_normalized_block, EntityIdentity, ProjectionEvent,
};
use super::state::ProjectionSource;
use super::{
    FinalizedBlock, OffchainDataProjection, PreparedBlock, PreparedReceipt, ProjectionError,
};

impl OffchainDataProjection {
    /// Decodes and simulates the entire block without performing any writes.
    pub fn prepare_block(&self, block: &FinalizedBlock) -> Result<PreparedBlock, ProjectionError> {
        self.validate_next_block(block.number, block.hash)?;
        validate_normalized_block(block)?;

        let mut decoded_receipts = Vec::with_capacity(block.receipts.len());
        for receipt in &block.receipts {
            let mut events = Vec::new();
            for log in &receipt.logs {
                let Some(signature) = log.data.topics().first().copied() else {
                    continue;
                };
                let source = ProjectionSource {
                    block_number: block.number,
                    block_hash: block.hash,
                    tx_hash: receipt.tx_hash,
                    transaction_index: receipt.transaction_index,
                    log_index: log.log_index,
                    emitter: log.emitter,
                    event_signature: signature,
                };
                let recognized = is_projection_pair(log.emitter, signature);
                if !receipt.success {
                    if recognized {
                        return Err(ProjectionError::ProjectionLogInFailedReceipt(Box::new(
                            source,
                        )));
                    }
                    continue;
                }
                if let Some(event) = decode_event(source, &log.data)? {
                    events.push(event);
                }
            }
            decoded_receipts.push((receipt, events));
        }

        let tribute_reader = TributeRepositoryReader::new(self.reader.clone());
        let retained_tribute_reader = RetainedTributeReader::new(self.reader.clone());
        let nod_reader = NodRepositoryReader::new(self.reader.clone());
        let mut tribute_ids = BTreeSet::new();
        let mut nod_ids = BTreeSet::new();
        let mut bucket_ids = BTreeSet::new();

        for (_, events) in &decoded_receipts {
            for event in events {
                match event.identity() {
                    None => {}
                    Some(EntityIdentity::Tribute(id)) => {
                        tribute_ids.insert(id);
                    }
                    Some(EntityIdentity::Nod(id)) => {
                        nod_ids.insert(id);
                    }
                    Some(EntityIdentity::Bucket(key)) => {
                        bucket_ids.insert(key);
                    }
                }
                if let ProjectionEvent::TributePartitionRetired { worldwide_day } = event {
                    super::retirement::collect_ids_for_retired_day(
                        &tribute_reader,
                        *worldwide_day,
                        &mut tribute_ids,
                    )?;
                }
            }
        }

        let tribute_ids: Vec<_> = tribute_ids.into_iter().collect();
        let nod_ids: Vec<_> = nod_ids.into_iter().collect();
        let bucket_ids: Vec<_> = bucket_ids.into_iter().collect();
        let mut tributes = tribute_reader.projection_session(&tribute_ids)?;
        for tribute_id in &tribute_ids {
            validate_existing_record("Tribute", tributes.current_with_metadata(*tribute_id)?)?;
        }
        let mut nods = nod_reader.projection_session(&nod_ids, &bucket_ids)?;
        for nod_id in &nod_ids {
            validate_existing_record("Nod", nods.current_item_with_metadata(*nod_id)?)?;
        }
        for bucket_id in &bucket_ids {
            validate_existing_record("Nod bucket", nods.current_bucket_with_metadata(*bucket_id)?)?;
        }

        let mut prepared_receipts = Vec::new();
        let mut seen_tributes = BTreeSet::new();
        let mut seen_nods = BTreeSet::new();
        let mut seen_buckets = BTreeSet::new();
        for (receipt, events) in decoded_receipts {
            let mut batch = AtomicWriteBatch::new();
            for event in events {
                match event {
                    ProjectionEvent::TributeStored {
                        source,
                        tribute_id,
                        stored_body,
                        previous_commitment,
                    } => {
                        let old = tributes.current(tribute_id)?;
                        validate_tribute_transition(
                            tribute_id,
                            old,
                            previous_commitment,
                            seen_tributes.insert(tribute_id),
                        )?;
                        let planned = tributes.store(
                            tribute_id,
                            stored_body,
                            Some(source.to_storage_metadata()?),
                        )?;
                        batch.extend(planned.operations().iter().cloned());
                    }
                    ProjectionEvent::TributeDeleted {
                        tribute_id,
                        previous_commitment,
                    } => {
                        let old = tributes.current(tribute_id)?;
                        validate_tribute_transition(
                            tribute_id,
                            old,
                            previous_commitment,
                            seen_tributes.insert(tribute_id),
                        )?;
                        let planned = tributes.delete(tribute_id)?;
                        batch.extend(planned.operations().iter().cloned());
                    }
                    ProjectionEvent::TributePartitionRetired { worldwide_day } => {
                        let retention_pin = self
                            .tribute_retention_selector
                            .as_ref()
                            .map(|selector| selector.active_pin_for(worldwide_day))
                            .transpose()
                            .map_err(|reason| ProjectionError::RetentionSelector {
                                worldwide_day,
                                reason,
                            })?
                            .flatten();
                        if let Some(pin) = retention_pin {
                            if pin.worldwide_day != worldwide_day {
                                return Err(ProjectionError::RetentionPinDayMismatch {
                                    requested: worldwide_day,
                                    selected: pin.worldwide_day,
                                });
                            }
                        }
                        super::retirement::plan_retired_partition(
                            &mut tributes,
                            &retained_tribute_reader,
                            &tribute_ids,
                            worldwide_day,
                            retention_pin,
                            &mut batch,
                        )?;
                    }
                    ProjectionEvent::NodStored {
                        source,
                        nod_id,
                        stored_body,
                        previous_commitment,
                    } => {
                        let old = nods.current_item(nod_id)?;
                        validate_nod_transition(
                            nod_id,
                            old,
                            previous_commitment,
                            seen_nods.insert(nod_id),
                        )?;
                        let planned = nods.store_item(
                            nod_id,
                            stored_body,
                            Some(source.to_storage_metadata()?),
                        )?;
                        batch.extend(planned.operations().iter().cloned());
                    }
                    ProjectionEvent::NodDeleted {
                        nod_id,
                        previous_commitment,
                    } => {
                        let old = nods.current_item(nod_id)?;
                        validate_nod_transition(
                            nod_id,
                            old,
                            previous_commitment,
                            seen_nods.insert(nod_id),
                        )?;
                        let planned = nods.delete_item(nod_id)?;
                        batch.extend(planned.operations().iter().cloned());
                    }
                    ProjectionEvent::BucketStored {
                        source,
                        bucket_id,
                        stored_body,
                        previous_commitment,
                    } => {
                        let old = nods.current_bucket(bucket_id)?;
                        validate_bucket_transition(
                            bucket_id,
                            old,
                            previous_commitment,
                            seen_buckets.insert(bucket_id),
                        )?;
                        let planned = nods.store_bucket(
                            bucket_id,
                            stored_body,
                            Some(source.to_storage_metadata()?),
                        )?;
                        batch.extend(planned.operations().iter().cloned());
                    }
                    ProjectionEvent::BucketDeleted {
                        bucket_id,
                        previous_commitment,
                    } => {
                        let old = nods.current_bucket(bucket_id)?;
                        validate_bucket_transition(
                            bucket_id,
                            old,
                            previous_commitment,
                            seen_buckets.insert(bucket_id),
                        )?;
                        let planned = nods.delete_bucket(bucket_id)?;
                        batch.extend(planned.operations().iter().cloned());
                    }
                }
            }
            if !batch.is_empty() {
                batch.validate()?;
                prepared_receipts.push(PreparedReceipt {
                    tx_hash: receipt.tx_hash,
                    transaction_index: receipt.transaction_index,
                    batch,
                });
            }
        }

        Ok(PreparedBlock {
            checkpoint: ProjectionCheckpoint {
                block_number: block.number,
                block_hash: block.hash,
            },
            receipts: prepared_receipts,
        })
    }
}

pub(super) fn validate_existing_record<T>(
    entity: &'static str,
    record: Option<(&T, Option<&StorageMetadata>)>,
) -> Result<(), ProjectionError> {
    let Some((_body, metadata)) = record else {
        return Ok(());
    };
    let metadata = metadata.ok_or(ProjectionError::MissingProjectionMetadata { entity })?;
    ProjectionSource::from_storage_metadata(metadata)?;
    Ok(())
}

pub(super) fn validate_tribute_transition(
    identity: WwdEntityId,
    old: Option<&TributeData>,
    previous: B256,
    first_in_block: bool,
) -> Result<(), ProjectionError> {
    if first_in_block {
        return Ok(());
    }
    let current = match old {
        Some(body) => {
            let payload = encode_tribute_v1(&outbe_tribute::canonical_body(body))
                .map_err(|error| ProjectionError::CorruptProjectedBody(error.to_string()))?;
            let commitment =
                body_commitment(ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1, identity, &payload)
                    .map_err(|error| ProjectionError::CorruptProjectedBody(error.to_string()))?;
            B256::from(*commitment.as_bytes())
        }
        None => B256::ZERO,
    };
    validate_transition("Tribute", identity, current, previous)
}

pub(super) fn validate_nod_transition(
    identity: WwdEntityId,
    old: Option<&NodItemState>,
    previous: B256,
    first_in_block: bool,
) -> Result<(), ProjectionError> {
    if first_in_block {
        return Ok(());
    }
    let current = match old {
        Some(body) => {
            let payload = encode_nod_item_v1(&outbe_nod::canonical_item(body))
                .map_err(|error| ProjectionError::CorruptProjectedBody(error.to_string()))?;
            let commitment =
                body_commitment(ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1, identity, &payload)
                    .map_err(|error| ProjectionError::CorruptProjectedBody(error.to_string()))?;
            B256::from(*commitment.as_bytes())
        }
        None => B256::ZERO,
    };
    validate_transition("Nod", identity, current, previous)
}

pub(super) fn validate_bucket_transition(
    identity: WwdEntityId,
    old: Option<&NodBucketState>,
    previous: B256,
    first_in_block: bool,
) -> Result<(), ProjectionError> {
    if first_in_block {
        return Ok(());
    }
    let current = match old {
        Some(body) => {
            let payload = encode_nod_bucket_v1(&outbe_nod::canonical_bucket(body))
                .map_err(|error| ProjectionError::CorruptProjectedBody(error.to_string()))?;
            let commitment =
                body_commitment(ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1, identity, &payload)
                    .map_err(|error| ProjectionError::CorruptProjectedBody(error.to_string()))?;
            B256::from(*commitment.as_bytes())
        }
        None => B256::ZERO,
    };
    validate_transition("Nod bucket", identity, current, previous)
}

pub(super) fn validate_transition(
    entity: &'static str,
    identity: WwdEntityId,
    current: B256,
    previous: B256,
) -> Result<(), ProjectionError> {
    if current == previous {
        return Ok(());
    }
    Err(ProjectionError::CommitmentTransitionMismatch {
        entity,
        identity,
        expected_previous: previous,
        actual: current,
    })
}
