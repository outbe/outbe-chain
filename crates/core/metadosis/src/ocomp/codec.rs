use crate::errors::storage_corruption_message;
use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::SchemaLimits;
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;

use super::state::{
    JobFsmSnapshot, JobFsmState, LiveAttemptSnapshot, ReadyAttemptSnapshot,
    RetainedRequestEffectSnapshot,
};

const SCHEDULER_MAGIC: [u8; 4] = *b"OMJS";
const SCHEDULER_VERSION: u16 = 1;
const SCHEDULER_PHASE_READY: u8 = 1;
const SCHEDULER_PHASE_PENDING: u8 = 2;
pub(super) const SCHEDULER_ENCODED_LEN: usize =
    4 + 2 + 1 + 4 + 8 + 8 + 32 + 8 + 8 + 1 + 8 + 32 + 32;
pub(super) struct FixedReader<'a> {
    encoded: &'a [u8],
    offset: usize,
}

impl<'a> FixedReader<'a> {
    pub(super) const fn new(encoded: &'a [u8]) -> Self {
        Self { encoded, offset: 0 }
    }

    pub(super) fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or_else(|| storage_corruption_message("OCOMP scheduler offset overflow"))?;
        let bytes = self
            .encoded
            .get(self.offset..end)
            .ok_or_else(|| storage_corruption_message("truncated OCOMP scheduler"))?;
        self.offset = end;
        bytes
            .try_into()
            .map_err(|_| storage_corruption_message("invalid OCOMP scheduler field width"))
    }

    pub(super) fn u8(&mut self) -> Result<u8> {
        Ok(self.take::<1>()?[0])
    }

    pub(super) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take::<4>()?))
    }

    pub(super) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take::<8>()?))
    }

    pub(super) fn finish(self) -> Result<()> {
        if self.offset == self.encoded.len() {
            Ok(())
        } else {
            Err(storage_corruption_message(
                "OCOMP scheduler has trailing bytes",
            ))
        }
    }
}

pub(super) fn max_canonical_object_bytes(limits: &SchemaLimits) -> Result<usize> {
    limits
        .codec
        .max_body_bytes
        .checked_add(outbe_ocomp_protocol::OCB1_HEADER_LEN)
        .ok_or_else(|| storage_corruption_message("OCOMP canonical object byte cap overflow"))
}

pub(super) fn read_canonical_optional<T>(
    bytes: &outbe_primitives::storage::types::StorageBytes<'_>,
    max_encoded_bytes: usize,
    decode: impl FnOnce(&[u8]) -> core::result::Result<T, outbe_ocomp_protocol::ProtocolError>,
    label: &'static str,
) -> Result<Option<T>> {
    let len = bytes.len()?;
    if len == 0 {
        return Ok(None);
    }
    if len > max_encoded_bytes {
        return Err(storage_corruption_message(format!(
            "{label} exceeds canonical byte cap"
        )));
    }
    let encoded = bytes.read()?;
    decode(&encoded)
        .map(Some)
        .map_err(|error| storage_corruption_message(format!("decode {label}: {error}")))
}

pub(super) fn encode_scheduler(state: &JobFsmState) -> Result<Vec<u8>> {
    encode_scheduler_snapshot(&scheduler_snapshot(state)?)
}

pub(super) fn scheduler_snapshot(state: &JobFsmState) -> Result<JobFsmSnapshot> {
    let mut snapshot = state.snapshot();
    snapshot.terminal.clear();
    if snapshot.live.is_none() && snapshot.ready.is_none() {
        return Err(storage_corruption_message(
            "OCOMP scheduler state has no active attempt",
        ));
    }
    Ok(snapshot)
}

pub(super) fn encode_scheduler_snapshot(snapshot: &JobFsmSnapshot) -> Result<Vec<u8>> {
    let mut encoded = Vec::with_capacity(SCHEDULER_ENCODED_LEN);
    encoded.extend_from_slice(&SCHEDULER_MAGIC);
    encoded.extend_from_slice(&SCHEDULER_VERSION.to_be_bytes());
    match (snapshot.ready, snapshot.live) {
        (Some(ready), None) => {
            encoded.push(SCHEDULER_PHASE_READY);
            encoded.extend_from_slice(&snapshot.worldwide_day.value().to_be_bytes());
            encoded.extend_from_slice(&ready.pending_nonce.to_be_bytes());
            encoded.extend_from_slice(&ready.next_check_height.to_be_bytes());
            encoded.extend_from_slice(B256::ZERO.as_slice());
            encoded.extend_from_slice(&0_u64.to_be_bytes());
            encoded.extend_from_slice(&0_u64.to_be_bytes());
            encode_retained_effect(&mut encoded, ready.retained_effect);
        }
        (None, Some(live)) => {
            encoded.push(SCHEDULER_PHASE_PENDING);
            encoded.extend_from_slice(&snapshot.worldwide_day.value().to_be_bytes());
            encoded.extend_from_slice(&live.pending_nonce.to_be_bytes());
            encoded.extend_from_slice(&0_u64.to_be_bytes());
            encoded.extend_from_slice(live.intent_id.as_slice());
            encoded.extend_from_slice(&live.requested_height.to_be_bytes());
            encoded.extend_from_slice(&live.deadline_height.unwrap_or(0).to_be_bytes());
            encode_retained_effect(&mut encoded, Some(live.retained_effect));
        }
        _ => {
            return Err(storage_corruption_message(
                "encode invalid OCOMP scheduler phase cardinality",
            ))
        }
    }
    if encoded.len() != SCHEDULER_ENCODED_LEN {
        return Err(storage_corruption_message(
            "OCOMP scheduler encoded length mismatch",
        ));
    }
    Ok(encoded)
}

fn encode_retained_effect(encoded: &mut Vec<u8>, effect: Option<RetainedRequestEffectSnapshot>) {
    match effect {
        Some(effect) => {
            encoded.push(1);
            encoded.extend_from_slice(&effect.effect_nonce.to_be_bytes());
            encoded.extend_from_slice(&effect.lysis_limit_minor.to_be_bytes::<32>());
            encoded.extend_from_slice(effect.receipt_hash.as_slice());
        }
        None => {
            encoded.push(0);
            encoded.extend_from_slice(&0_u64.to_be_bytes());
            encoded.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
            encoded.extend_from_slice(B256::ZERO.as_slice());
        }
    }
}

pub(super) fn decode_scheduler(encoded: &[u8]) -> Result<JobFsmSnapshot> {
    if encoded.len() != SCHEDULER_ENCODED_LEN {
        return Err(storage_corruption_message(
            "OCOMP scheduler has non-canonical length",
        ));
    }
    let mut reader = FixedReader::new(encoded);
    if reader.take::<4>()? != SCHEDULER_MAGIC
        || u16::from_be_bytes(reader.take::<2>()?) != SCHEDULER_VERSION
    {
        return Err(storage_corruption_message(
            "OCOMP scheduler magic/version mismatch",
        ));
    }
    let phase = reader.u8()?;
    let worldwide_day = WorldwideDay::new(reader.u32()?);
    let pending_nonce = reader.u64()?;
    let next_check_height = reader.u64()?;
    let intent_id = B256::from(reader.take::<32>()?);
    let requested_height = reader.u64()?;
    let deadline_height = reader.u64()?;
    let retained_effect = decode_retained_effect(&mut reader)?;
    reader.finish()?;

    decode_scheduler_phase(SchedulerFields {
        phase,
        worldwide_day,
        pending_nonce,
        next_check_height,
        intent_id,
        requested_height,
        deadline_height,
        retained_effect,
    })
}

struct SchedulerFields {
    phase: u8,
    worldwide_day: WorldwideDay,
    pending_nonce: u64,
    next_check_height: u64,
    intent_id: B256,
    requested_height: u64,
    deadline_height: u64,
    retained_effect: Option<RetainedRequestEffectSnapshot>,
}

fn decode_scheduler_phase(fields: SchedulerFields) -> Result<JobFsmSnapshot> {
    let SchedulerFields {
        phase,
        worldwide_day,
        pending_nonce,
        next_check_height,
        intent_id,
        requested_height,
        deadline_height,
        retained_effect,
    } = fields;
    let (ready, live) = match phase {
        SCHEDULER_PHASE_READY
            if (intent_id, requested_height, deadline_height) == (B256::ZERO, 0, 0) =>
        {
            (
                Some(ReadyAttemptSnapshot {
                    pending_nonce,
                    next_check_height,
                    retained_effect,
                }),
                None,
            )
        }
        SCHEDULER_PHASE_PENDING
            if next_check_height == 0 && !intent_id.is_zero() && retained_effect.is_some() =>
        {
            (
                None,
                Some(LiveAttemptSnapshot {
                    intent_id,
                    pending_nonce,
                    requested_height,
                    deadline_height: (deadline_height != 0).then_some(deadline_height),
                    retained_effect: retained_effect.ok_or_else(|| {
                        storage_corruption_message("pending OCOMP scheduler has no retained effect")
                    })?,
                }),
            )
        }
        _ => {
            return Err(storage_corruption_message(
                "OCOMP scheduler phase/index fields are inconsistent",
            ))
        }
    };
    Ok(JobFsmSnapshot {
        worldwide_day,
        ready,
        live,
        terminal: Vec::new(),
    })
}

fn decode_retained_effect(
    reader: &mut FixedReader<'_>,
) -> Result<Option<RetainedRequestEffectSnapshot>> {
    let present = reader.u8()?;
    let effect_nonce = reader.u64()?;
    let lysis_limit_minor = U256::from_be_bytes(reader.take::<32>()?);
    let receipt_hash = B256::from(reader.take::<32>()?);
    match present {
        0 if effect_nonce == 0 && lysis_limit_minor.is_zero() && receipt_hash.is_zero() => Ok(None),
        1 if !receipt_hash.is_zero() => Ok(Some(RetainedRequestEffectSnapshot {
            effect_nonce,
            lysis_limit_minor,
            receipt_hash,
        })),
        _ => Err(storage_corruption_message(
            "OCOMP scheduler retained-effect fields are inconsistent",
        )),
    }
}
