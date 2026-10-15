use super::super::exact_unit_output_source;
use super::super::planner_from_authority;
use super::super::primary_unit_spec_from_authority;
use super::super::require_authenticated_input;
use super::super::require_lease_active;
use super::super::UnitExecutionAuthority;
use super::super::WorkerError;

use crate::input_artifacts::decode_oracle_subject_key;
use alloy_primitives::U256;

use outbe_lysis::program_v1::artifacts::decode_enumerated_run;
use outbe_lysis::program_v1::artifacts::decode_fidelity_map_output;

use outbe_lysis::program_v1::artifacts::decode_fixed_reduce_output;

use outbe_lysis::program_v1::artifacts::encode_amount_run;

use outbe_lysis::program_v1::artifacts::LysisArtifactErrorV1;

use outbe_lysis::program_v1::phases::amount_map;

use outbe_lysis::program_v1::planner::LysisPlanTopologyV1;

use outbe_lysis::program_v1::ObservationValueV1;
use outbe_lysis::program_v1::ObservedTributeV1;

use super::tribute_nominal::{refill_nominal, shard_tribute_nominals};

use outbe_ocomp_protocol::common::BoundedBytes;

use outbe_ocomp_protocol::input::AuthenticatedOpeningV1;
use outbe_ocomp_protocol::input::InputChunkKind;

use outbe_ocomp_protocol::input::OpeningSourceKind;

use outbe_ocomp_protocol::unit::BinaryReducerNode;

use outbe_ocomp_protocol::unit::InputPurpose;

use outbe_ocomp_protocol::unit::UnitArtifactV1;
use outbe_ocomp_protocol::unit::UnitInterval;
use outbe_ocomp_protocol::unit::UnitPhase;
use outbe_ocomp_protocol::unit::UnitSpecV1;
use outbe_ocomp_protocol::unit::WorkOutputHeaderV1;

use outbe_nod::openings::evaluate_entry_prices;

use outbe_primitives::time::WorldwideDay;

pub(in super::super) fn execute_amount_map_unit(
    spec: &UnitSpecV1,
    authority: UnitExecutionAuthority<'_>,
) -> Result<UnitArtifactV1, WorkerError> {
    let UnitExecutionAuthority {
        manifest,
        input_chunks,
        producer_artifacts,
        bundle,
        limits,
        cancelled,
        ..
    } = authority;
    require_lease_active(cancelled)?;
    let (shard_ordinal, enumerate_unit_id, fidelity_unit_id, fraction_root_unit_id) =
        resolve_amount_map_bindings(spec, &authority)?;
    let (enumerated, enumerate_header, fidelity, root_output) = validate_amount_producers(
        spec,
        &authority,
        (
            shard_ordinal,
            enumerate_unit_id,
            fidelity_unit_id,
            fraction_root_unit_id,
        ),
        producer_artifacts,
    )?;
    let entry_prices = evaluate_oracle_entry_prices(spec, manifest, input_chunks, bundle, limits)?;
    let observed = collect_observed_tributes(
        (input_chunks, shard_ordinal),
        &enumerated,
        &fidelity,
        &entry_prices,
        cancelled,
    )?;
    let amount = amount_map(
        enumerated.start_ordinal,
        &observed,
        &fidelity.observations,
        &root_output.ordered_fractions,
        authority.plan.lysis_limit_minor / U256::from(1024),
    )
    .map_err(LysisArtifactErrorV1::from)?;
    let output_coverage_root = amount.coverage_root()?;
    if output_coverage_root != enumerate_header.output_coverage_root {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let output_count = u32::try_from(amount.ordered_records.len())
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: enumerate_header.output_coverage_root,
            output_coverage_root,
            source_coverage_count: enumerate_header.output_coverage_count,
            output_coverage_count: output_count,
        },
        BoundedBytes(encode_amount_run(&amount, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}

fn resolve_amount_map_bindings(
    spec: &UnitSpecV1,
    authority: &UnitExecutionAuthority<'_>,
) -> Result<
    (
        u32,
        alloy_primitives::B256,
        alloy_primitives::B256,
        alloy_primitives::B256,
    ),
    WorkerError,
> {
    if authority.producer_artifacts.len() != 3 {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let topology = LysisPlanTopologyV1::new(authority.plan.primary_work_unit_count)?;
    let amount_offset = authority
        .plan
        .primary_work_unit_count
        .checked_mul(2)
        .and_then(|offset| offset.checked_add(topology.phase_unit_count(UnitPhase::FixedReduce)))
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let shard_ordinal = authority
        .unit_index
        .checked_sub(amount_offset)
        .filter(|ordinal| *ordinal < authority.plan.primary_work_unit_count)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let enumerate_unit_id = exact_unit_output_source(spec, InputPurpose::EnumeratedTributes)?;
    let fidelity_unit_id = exact_unit_output_source(spec, InputPurpose::FidelityPartials)?;
    let fraction_root_unit_id = exact_unit_output_source(spec, InputPurpose::FiFractionTable)?;
    Ok((
        shard_ordinal,
        enumerate_unit_id,
        fidelity_unit_id,
        fraction_root_unit_id,
    ))
}

type ValidatedAmountProducers = (
    outbe_lysis::program_v1::artifacts::EnumeratedRunV1,
    WorkOutputHeaderV1,
    outbe_lysis::program_v1::phases::FidelityMapOutputV1,
    outbe_lysis::program_v1::artifacts::FixedReduceOutputV1,
);

fn validate_amount_producers(
    spec: &UnitSpecV1,
    authority: &UnitExecutionAuthority<'_>,
    (shard_ordinal, enumerate_unit_id, fidelity_unit_id, fraction_root_unit_id): (
        u32,
        alloy_primitives::B256,
        alloy_primitives::B256,
        alloy_primitives::B256,
    ),
    producer_artifacts: &[UnitArtifactV1],
) -> Result<ValidatedAmountProducers, WorkerError> {
    let limits = authority.limits;
    let planner =
        planner_from_authority(authority.plan, authority.manifest, authority.bundle, limits)?;
    let primary_spec = primary_unit_spec_from_authority(&planner, shard_ordinal, authority)?;
    if primary_spec.unit_id(limits)? != enumerate_unit_id
        || planner.amount_map_unit_at(
            shard_ordinal,
            &primary_spec,
            fidelity_unit_id,
            fraction_root_unit_id,
            limits,
        )? != *spec
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let (enumerated, enumerate_header) =
        validate_enumerate_producer(&producer_artifacts[0], &primary_spec, limits)?;
    let fidelity_spec = planner.fidelity_map_unit_at(shard_ordinal, enumerate_unit_id, limits)?;
    let fidelity = validate_fidelity_producer(
        &producer_artifacts[1],
        &fidelity_spec,
        fidelity_unit_id,
        &enumerate_header,
        limits,
    )?;
    let root_output = validate_fraction_root_producer(
        spec,
        &producer_artifacts[2],
        fraction_root_unit_id,
        (
            authority.plan.primary_work_unit_count,
            authority.plan.tribute_count,
        ),
        limits,
    )?;
    Ok((enumerated, enumerate_header, fidelity, root_output))
}

fn validate_enumerate_producer(
    enumerate_artifact: &UnitArtifactV1,
    primary_spec: &UnitSpecV1,
    limits: &outbe_ocomp_protocol::SchemaLimits,
) -> Result<
    (
        outbe_lysis::program_v1::artifacts::EnumeratedRunV1,
        WorkOutputHeaderV1,
    ),
    WorkerError,
> {
    enumerate_artifact.validate_against(primary_spec, limits)?;
    let enumerated = decode_enumerated_run(enumerate_artifact.phase_payload(limits)?, limits)?;
    let enumerate_header = enumerate_artifact.output_header(limits)?;
    let expected_count = u32::try_from(enumerated.ordered_records.len())
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    if enumerated.coverage_root()? != enumerate_header.output_coverage_root
        || enumerate_header.output_coverage_count != expected_count
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok((enumerated, enumerate_header))
}

fn validate_fidelity_producer(
    fidelity_artifact: &UnitArtifactV1,
    fidelity_spec: &UnitSpecV1,
    fidelity_unit_id: alloy_primitives::B256,
    enumerate_header: &WorkOutputHeaderV1,
    limits: &outbe_ocomp_protocol::SchemaLimits,
) -> Result<outbe_lysis::program_v1::phases::FidelityMapOutputV1, WorkerError> {
    fidelity_artifact.validate_against(fidelity_spec, limits)?;
    if fidelity_artifact.unit_id != fidelity_unit_id {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let fidelity = decode_fidelity_map_output(fidelity_artifact.phase_payload(limits)?, limits)?;
    let fidelity_header = fidelity_artifact.output_header(limits)?;
    if fidelity.coverage_root()? != fidelity_header.output_coverage_root
        || fidelity_header.output_coverage_root != enumerate_header.output_coverage_root
        || fidelity.aggregate.tribute_count != fidelity_header.output_coverage_count
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(fidelity)
}

fn validate_fraction_root_producer(
    spec: &UnitSpecV1,
    root_artifact: &UnitArtifactV1,
    fraction_root_unit_id: alloy_primitives::B256,
    (primary_work_unit_count, tribute_count): (u32, u32),
    limits: &outbe_ocomp_protocol::SchemaLimits,
) -> Result<outbe_lysis::program_v1::artifacts::FixedReduceOutputV1, WorkerError> {
    root_artifact.validate_semantics(limits)?;
    let topology = LysisPlanTopologyV1::new(primary_work_unit_count)?;
    let mut root_interval_binding = spec.clone();
    root_interval_binding.phase = UnitPhase::FixedReduce;
    root_interval_binding.interval = UnitInterval::BinaryReducerNode(BinaryReducerNode {
        level: topology.tree().height(),
        index: 0,
    });
    let expected_binding = (
        fraction_root_unit_id,
        spec.protocol_bundle_hash,
        spec.job_id,
        spec.attempt,
        UnitPhase::FixedReduce,
        root_interval_binding.interval_commitment(limits)?,
    );
    let actual_binding = (
        root_artifact.unit_id,
        root_artifact.protocol_bundle_hash,
        root_artifact.job_id,
        root_artifact.attempt,
        root_artifact.phase,
        root_artifact.interval_commitment,
    );
    if actual_binding != expected_binding {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let root_output = decode_fixed_reduce_output(root_artifact.phase_payload(limits)?, limits)?;
    let root_header = root_artifact.output_header(limits)?;
    let root_aggregate = root_output
        .aggregate
        .as_ref()
        .ok_or(WorkerError::UnitBindingMismatch)?;
    if root_output.ordered_fractions.is_empty()
        || root_aggregate.tribute_count != tribute_count
        || root_output.coverage.final_root(tribute_count)? != root_header.output_coverage_root
        || root_header.output_coverage_count != tribute_count
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok(root_output)
}

fn evaluate_oracle_entry_prices(
    spec: &UnitSpecV1,
    manifest: &outbe_ocomp_protocol::input::InputManifestV1,
    input_chunks: &[(
        outbe_ocomp_protocol::input::InputChunkRefV1,
        outbe_ocomp_protocol::input::AuthenticatedInputChunkV1,
    )],
    bundle: &outbe_ocomp_protocol::profile::ProtocolBundleV1,
    limits: &outbe_ocomp_protocol::SchemaLimits,
) -> Result<std::collections::BTreeMap<u16, alloy_primitives::U256>, WorkerError> {
    let mut oracle_chunks = input_chunks
        .iter()
        .filter(|(_, chunk)| chunk.kind == InputChunkKind::Oracle);
    let (oracle_reference, oracle_chunk) = oracle_chunks
        .next()
        .filter(|(_, chunk)| chunk.canonical_records_or_openings.len() == 1)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    if oracle_chunks.next().is_some() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    require_authenticated_input(
        spec,
        InputPurpose::OracleOpenings,
        manifest.oracle_opening_root,
        1,
        oracle_reference.encoded_bytes,
    )?;
    let opening = AuthenticatedOpeningV1::decode_canonical_record(
        &oracle_chunk.canonical_records_or_openings[0].0,
        limits,
    )?;
    opening.validate_against_bundle(bundle, limits)?;
    let (oracle_wwd, reference_isos) = decode_oracle_subject_key(&opening.canonical_subject_key.0)?;
    let raw = opening
        .decode_and_validate_raw_opening(manifest.checkpoint.finalized_state_root, limits)?;
    if opening.source_kind != OpeningSourceKind::Oracle
        || oracle_wwd != manifest.wwd
        || raw.contract_address != outbe_primitives::addresses::NOD_ADDRESS
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let raw_slots = raw
        .ordered_slots
        .iter()
        .map(|slot| (slot.slot, slot.value))
        .collect::<Vec<_>>();
    evaluate_entry_prices(WorldwideDay::new(manifest.wwd), &reference_isos, &raw_slots)
        .map_err(|_| WorkerError::UnitBindingMismatch)
}

fn collect_observed_tributes(
    (input_chunks, shard_ordinal): (
        &[(
            outbe_ocomp_protocol::input::InputChunkRefV1,
            outbe_ocomp_protocol::input::AuthenticatedInputChunkV1,
        )],
        u32,
    ),
    enumerated: &outbe_lysis::program_v1::artifacts::EnumeratedRunV1,
    fidelity: &outbe_lysis::program_v1::phases::FidelityMapOutputV1,
    entry_prices: &std::collections::BTreeMap<u16, alloy_primitives::U256>,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Vec<ObservedTributeV1>, WorkerError> {
    let nominals = shard_tribute_nominals(input_chunks, shard_ordinal)?;
    if nominals.len() != enumerated.ordered_records.len() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let mut observed = Vec::new();
    observed
        .try_reserve_exact(enumerated.ordered_records.len())
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    for (record, fidelity) in enumerated
        .ordered_records
        .iter()
        .zip(&fidelity.observations)
    {
        require_lease_active(cancelled)?;
        if record.raw_ordinal != fidelity.raw_ordinal
            || record.tribute.tribute_id != fidelity.tribute_id
        {
            return Err(WorkerError::UnitBindingMismatch);
        }
        let mut tribute = record.tribute.clone();
        refill_nominal(&mut tribute, &nominals)?;
        observed.push(ObservedTributeV1 {
            tribute,
            first_league: ObservationValueV1::Value(fidelity.pre_distribution_league),
            second_league: ObservationValueV1::Value(fidelity.issuance_league),
            entry_price_minor: entry_prices
                .get(&record.tribute.reference_currency)
                .copied()
                .map_or(ObservationValueV1::Unavailable, ObservationValueV1::Value),
            nod_target_available: true,
        });
    }
    Ok(observed)
}
