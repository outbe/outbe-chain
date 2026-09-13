use super::super::require_lease_active;
use super::super::UnitExecutionAuthority;
use super::super::WorkerError;

use crate::lysis_phase_replay::replay_output_finalize_artifact;

use outbe_lysis::program_v1::planner::LysisPlanTopologyV1;

use outbe_ocomp_protocol::unit::UnitArtifactV1;

use outbe_ocomp_protocol::unit::UnitPhase;
use outbe_ocomp_protocol::unit::UnitSpecV1;

pub(in super::super) fn execute_output_finalize_unit(
    spec: &UnitSpecV1,
    authority: UnitExecutionAuthority<'_>,
) -> Result<UnitArtifactV1, WorkerError> {
    let UnitExecutionAuthority {
        plan,
        unit_index,
        manifest,
        input_chunks,
        producer_artifacts,
        bundle,
        limits,
        cancelled,
        ..
    } = authority;
    require_lease_active(cancelled)?;
    if !input_chunks.is_empty() || producer_artifacts.len() != 2 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)?;
    let shard_ordinal = unit_index
        .checked_sub(topology.phase_offset(UnitPhase::OutputFinalize)?)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    if shard_ordinal >= plan.primary_work_unit_count {
        return Err(WorkerError::UnitBindingMismatch);
    }
    replay_output_finalize_artifact(
        shard_ordinal,
        spec,
        &producer_artifacts[0],
        &producer_artifacts[1],
        plan,
        manifest,
        bundle,
        limits,
    )
    .map_err(WorkerError::from)
}
