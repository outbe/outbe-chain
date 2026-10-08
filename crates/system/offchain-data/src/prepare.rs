mod session;
mod sources;

use std::collections::BTreeSet;

use alloy_primitives::B256;
use outbe_compressed_entities::{
    body_commitment, encode_nod_bucket_v1, encode_nod_item_v2, WwdEntityId,
    ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_nod::{NodBucketState, NodItemState};
use outbe_offchain_storage::{AtomicWriteBatch, StorageMetadata};
use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_tribute::{RetainedTributeReader, TributeRecord, TributeRepositoryReader};

use super::decode::{
    decode_event, is_projection_pair, validate_normalized_block, EntityIdentity, ProjectionEvent,
};
use super::state::ProjectionSource;
use super::{
    DayRetirement, FinalizedBlock, OffchainDataProjection, PreparedBlock, PreparedReceipt,
    ProjectionError,
};

impl OffchainDataProjection {
    /// Decodes and simulates the block.
    /// With no day route, this function performs no writes.
    /// With a day route, a Tribute read can migrate legacy keys.
    /// That migration writes the shared database and the day databases.
    pub fn prepare_block(&self, block: &FinalizedBlock) -> Result<PreparedBlock, ProjectionError> {
        self.validate_next_block(block.number, block.hash)?;
        validate_normalized_block(block)?;

        let decoded_receipts = sources::decode_receipts(block)?;
        let sources = sources::ProjectionSessions::load(self, &decoded_receipts)?;
        let mut projection = session::BlockProjection::new(self, sources);
        let mut prepared_receipts = Vec::new();
        for (receipt, events) in decoded_receipts {
            let mut batch = AtomicWriteBatch::new();
            for event in events {
                let planned = projection.plan_event(event)?;
                batch.extend(planned.operations().iter().cloned());
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
            day_retirements: projection.day_retirements,
        })
    }
}

fn reject_tribute_after_retirement(
    routed: bool,
    retired_days: &BTreeSet<u32>,
    tribute_id: WwdEntityId,
) -> Result<(), ProjectionError> {
    if routed && retired_days.contains(&tribute_id.worldwide_day().value()) {
        return Err(ProjectionError::TributeStoredAfterDayRetirement { tribute_id });
    }
    Ok(())
}

fn record_day_retirement(
    retirements: &mut Vec<DayRetirement>,
    retired_days: &mut BTreeSet<u32>,
    day: u32,
    pin: Option<outbe_tribute::RetainedTributePin>,
) {
    retired_days.insert(day);
    let action = match pin {
        Some(pin) => DayRetirement::Retain {
            day,
            lease: pin.input_lease_id,
        },
        None => DayRetirement::Drop(day),
    };
    if let Some(existing) = retirements.iter_mut().find(|item| match item {
        DayRetirement::Drop(recorded) | DayRetirement::Retain { day: recorded, .. } => {
            *recorded == day
        }
    }) {
        *existing = action;
    } else {
        retirements.push(action);
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
    old: Option<&TributeRecord>,
    previous: B256,
    first_in_block: bool,
) -> Result<(), ProjectionError> {
    if first_in_block {
        return Ok(());
    }
    let current = match old {
        Some(body) => {
            let stored = body
                .stored_body()
                .map_err(|error| ProjectionError::CorruptProjectedBody(error.to_string()))?;
            let commitment = body_commitment(
                ACTIVE_COMMITMENT_SCHEME,
                stored.schema_version(),
                identity,
                stored.payload(),
            )
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
            let payload = encode_nod_item_v2(&outbe_nod::canonical_item(body))
                .map_err(|error| ProjectionError::CorruptProjectedBody(error.to_string()))?;
            let commitment = body_commitment(
                ACTIVE_COMMITMENT_SCHEME,
                outbe_compressed_entities::NOD_BODY_SCHEMA_V2,
                identity,
                &payload,
            )
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
