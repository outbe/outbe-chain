use super::super::require_authenticated_input;
use super::super::require_lease_active;
use super::super::WorkerError;

use outbe_compressed_entities::decode_tribute_v1;

use outbe_lysis::program_v1::artifacts::encode_enumerated_run;

use outbe_lysis::program_v1::artifacts::enumerate_tributes;

use outbe_lysis::program_v1::TributeInputV1;
use outbe_ocomp_protocol::common::BoundedBytes;
use outbe_ocomp_protocol::input::AuthenticatedInputChunkV1;

use outbe_ocomp_protocol::input::InputChunkKind;
use outbe_ocomp_protocol::input::InputChunkRefV1;
use outbe_ocomp_protocol::input::InputManifestV1;

use outbe_ocomp_protocol::unit::InputPurpose;

use outbe_ocomp_protocol::unit::UnitArtifactV1;
use outbe_ocomp_protocol::unit::UnitInterval;

use outbe_ocomp_protocol::unit::UnitSpecV1;
use outbe_ocomp_protocol::unit::WorkOutputHeaderV1;

use outbe_ocomp_protocol::SchemaLimits;

use outbe_primitives::time::WorldwideDay;

use std::sync::atomic::AtomicBool;

pub(in super::super) fn execute_enumerate_unit(
    spec: &UnitSpecV1,
    manifest: &InputManifestV1,
    input_chunks: &[(InputChunkRefV1, AuthenticatedInputChunkV1)],
    producer_artifacts: &[UnitArtifactV1],
    limits: &SchemaLimits,
    cancelled: Option<&AtomicBool>,
) -> Result<UnitArtifactV1, WorkerError> {
    if !producer_artifacts.is_empty() || input_chunks.len() != 1 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let (reference, chunk) = &input_chunks[0];
    if chunk.kind != InputChunkKind::Tribute {
        return Err(WorkerError::UnitBindingMismatch);
    }
    require_authenticated_input(
        spec,
        InputPurpose::TributeStream,
        reference.semantic_digest,
        reference.record_count,
        reference.encoded_bytes,
    )?;
    let UnitInterval::EntityIdRange(range) = &spec.interval else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let mut tributes = Vec::new();
    tributes
        .try_reserve_exact(chunk.canonical_records_or_openings.len())
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    for record in &chunk.canonical_records_or_openings {
        require_lease_active(cancelled)?;
        let tribute = decode_tribute_v1(&record.0)?;
        let id = *tribute.tribute_id;
        if id < range.start || range.end.is_some_and(|end| id >= end) {
            return Err(WorkerError::UnitBindingMismatch);
        }
        tributes.push(TributeInputV1::from(&tribute));
    }
    if tributes
        .first()
        .map(|tribute| tribute.tribute_id.as_slice())
        != Some(&range.start.0)
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let start_ordinal = chunk
        .ordinal
        .checked_mul(256)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let run = enumerate_tributes(start_ordinal, WorldwideDay::new(manifest.wwd), &tributes)?;
    let coverage_root = run.coverage_root()?;
    let output_count =
        u32::try_from(run.ordered_records.len()).map_err(|_| WorkerError::UnitBindingMismatch)?;
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: coverage_root,
            output_coverage_root: coverage_root,
            source_coverage_count: output_count,
            output_coverage_count: output_count,
        },
        BoundedBytes(encode_enumerated_run(&run, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}
