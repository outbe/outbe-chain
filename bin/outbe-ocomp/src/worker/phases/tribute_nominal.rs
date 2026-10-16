use std::collections::BTreeMap;

use alloy_primitives::{B256, U256};
use outbe_lysis::program_v1::TributeInputV1;
use outbe_ocomp_protocol::input::{AuthenticatedInputChunkV1, InputChunkKind, InputChunkRefV1};

use super::super::WorkerError;

/// Nominal of each Tribute in the one shard chunk.
///
/// A decoded enumerated row stores nominal as zero. Fidelity and Amount refill
/// it from this map before any amount math. A missing or duplicate id fails.
pub(super) fn shard_tribute_nominals(
    input_chunks: &[(InputChunkRefV1, AuthenticatedInputChunkV1)],
    shard_ordinal: u32,
) -> Result<BTreeMap<B256, U256>, WorkerError> {
    let mut matched = input_chunks.iter().filter(|(reference, chunk)| {
        reference.kind == InputChunkKind::Tribute
            && chunk.kind == InputChunkKind::Tribute
            && reference.ordinal == shard_ordinal
    });
    let Some((_, chunk)) = matched.next() else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    if matched.next().is_some() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let mut nominals = BTreeMap::new();
    for record in &chunk.canonical_records_or_openings {
        let tribute = outbe_tribute::record::decode_canonical(&record.0)?.calculation_view()?;
        let id = *tribute.tribute_id;
        if nominals.insert(id, tribute.nominal_amount_minor).is_some() {
            return Err(WorkerError::UnitBindingMismatch);
        }
    }
    Ok(nominals)
}

pub(super) fn refill_nominal(
    tribute: &mut TributeInputV1,
    nominals: &BTreeMap<B256, U256>,
) -> Result<(), WorkerError> {
    tribute.nominal_amount_minor = nominals
        .get(&*tribute.tribute_id)
        .copied()
        .ok_or(WorkerError::UnitBindingMismatch)?;
    Ok(())
}
