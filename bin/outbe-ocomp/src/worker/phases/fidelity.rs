mod fixed_reduce;
pub(in super::super) use fixed_reduce::execute_fixed_reduce_unit;

use super::super::exact_unit_output_source;
use super::super::planner_from_authority;
use super::super::primary_unit_spec_from_authority;
use super::super::require_authenticated_input;
use super::super::require_lease_active;
use super::super::UnitExecutionAuthority;
use super::super::WorkerError;

use crate::input_artifacts::decode_fidelity_subject_key;

use alloy_primitives::Address;
use alloy_primitives::U256;

use outbe_lysis::program_v1::artifacts::decode_enumerated_run;
use outbe_lysis::program_v1::artifacts::encode_fidelity_map_output;
use outbe_lysis::program_v1::artifacts::EnumeratedRunV1;
use outbe_lysis::program_v1::artifacts::LysisArtifactErrorV1;
use outbe_lysis::program_v1::phases::fidelity_map;
use outbe_lysis::program_v1::ObservationValueV1;
use outbe_lysis::program_v1::ObservedTributeV1;

use super::tribute_nominal::{refill_nominal, shard_tribute_nominals};

use outbe_ocomp_protocol::common::BoundedBytes;
use outbe_ocomp_protocol::input::AuthenticatedOpeningV1;
use outbe_ocomp_protocol::input::InputChunkKind;
use outbe_ocomp_protocol::input::InputManifestV1;
use outbe_ocomp_protocol::input::OpeningSourceKind;
use outbe_ocomp_protocol::league_snapshot::league_snapshot_slot;
use outbe_ocomp_protocol::profile::ProtocolBundleV1;
use outbe_ocomp_protocol::unit::InputPurpose;
use outbe_ocomp_protocol::unit::UnitArtifactV1;
use outbe_ocomp_protocol::unit::UnitPhase;
use outbe_ocomp_protocol::unit::UnitSpecV1;
use outbe_ocomp_protocol::unit::WorkOutputHeaderV1;
use outbe_ocomp_protocol::SchemaLimits;

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

pub(in super::super) fn execute_fidelity_map_unit(
    spec: &UnitSpecV1,
    authority: UnitExecutionAuthority<'_>,
) -> Result<UnitArtifactV1, WorkerError> {
    let UnitExecutionAuthority {
        manifest,
        input_chunks,
        bundle,
        limits,
        cancelled,
        ..
    } = authority;
    require_lease_active(cancelled)?;
    let (shard_ordinal, enumerated, producer_header) =
        validate_fidelity_enumerate_producer(spec, &authority)?;
    let leagues = collect_fidelity_opening_leagues(
        spec,
        manifest,
        input_chunks,
        (bundle, limits, cancelled),
    )?;
    let observed = collect_fidelity_observed_tributes(
        (input_chunks, shard_ordinal),
        &enumerated,
        &leagues,
        cancelled,
    )?;
    let output =
        fidelity_map(enumerated.start_ordinal, &observed).map_err(LysisArtifactErrorV1::from)?;
    let output_coverage_root = output.coverage_root()?;
    if output_coverage_root != producer_header.output_coverage_root {
        return Err(WorkerError::UnitBindingMismatch);
    }
    UnitArtifactV1::from_canonical_output(
        spec,
        WorkOutputHeaderV1 {
            source_coverage_root: producer_header.output_coverage_root,
            output_coverage_root,
            source_coverage_count: producer_header.output_coverage_count,
            output_coverage_count: output.aggregate.tribute_count,
        },
        BoundedBytes(encode_fidelity_map_output(&output, limits)?),
        limits,
    )
    .map_err(WorkerError::from)
}

fn validate_fidelity_enumerate_producer(
    spec: &UnitSpecV1,
    authority: &UnitExecutionAuthority<'_>,
) -> Result<(u32, EnumeratedRunV1, WorkOutputHeaderV1), WorkerError> {
    let shard_ordinal = authority
        .unit_index
        .checked_sub(authority.plan.primary_work_unit_count)
        .filter(|ordinal| *ordinal < authority.plan.primary_work_unit_count)
        .ok_or(WorkerError::UnitBindingMismatch)?;
    let [producer] = authority.producer_artifacts else {
        return Err(WorkerError::UnitBindingMismatch);
    };
    let enumerate_unit_id = exact_unit_output_source(spec, InputPurpose::EnumeratedTributes)?;
    let limits = authority.limits;
    let planner =
        planner_from_authority(authority.plan, authority.manifest, authority.bundle, limits)?;
    if planner.fidelity_map_unit_at(shard_ordinal, enumerate_unit_id, limits)? != *spec {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let primary_spec = primary_unit_spec_from_authority(&planner, shard_ordinal, authority)?;
    producer.validate_against(&primary_spec, limits)?;
    if producer.unit_id != enumerate_unit_id || producer.phase != UnitPhase::Enumerate {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let enumerated = decode_enumerated_run(producer.phase_payload(limits)?, limits)?;
    let producer_header = producer.output_header(limits)?;
    let expected_len = usize::try_from(producer_header.output_coverage_count)
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    if enumerated.coverage_root()? != producer_header.output_coverage_root
        || enumerated.ordered_records.len() != expected_len
    {
        return Err(WorkerError::UnitBindingMismatch);
    }
    Ok((shard_ordinal, enumerated, producer_header))
}

fn collect_fidelity_opening_leagues(
    spec: &UnitSpecV1,
    manifest: &InputManifestV1,
    input_chunks: &[(
        outbe_ocomp_protocol::input::InputChunkRefV1,
        outbe_ocomp_protocol::input::AuthenticatedInputChunkV1,
    )],
    (bundle, limits, cancelled): (&ProtocolBundleV1, &SchemaLimits, Option<&AtomicBool>),
) -> Result<BTreeMap<Address, u16>, WorkerError> {
    let mut leagues = BTreeMap::new();
    let mut fidelity_opening_count = 0_u32;
    let mut fidelity_encoded_bytes = 0_u64;
    for (reference, chunk) in input_chunks {
        require_lease_active(cancelled)?;
        match chunk.kind {
            InputChunkKind::Tribute => {}
            InputChunkKind::Fidelity => {
                fidelity_encoded_bytes = fidelity_encoded_bytes
                    .checked_add(reference.encoded_bytes)
                    .ok_or(WorkerError::UnitBindingMismatch)?;
                for encoded in &chunk.canonical_records_or_openings {
                    require_lease_active(cancelled)?;
                    fidelity_opening_count = fidelity_opening_count
                        .checked_add(1)
                        .ok_or(WorkerError::UnitBindingMismatch)?;
                    record_fidelity_opening_leagues(
                        &encoded.0,
                        manifest,
                        (bundle, limits, cancelled),
                        &mut leagues,
                    )?;
                }
            }
            InputChunkKind::Oracle => return Err(WorkerError::UnitBindingMismatch),
        }
    }
    require_authenticated_input(
        spec,
        InputPurpose::FidelityOpenings,
        manifest.fidelity_opening_root,
        fidelity_opening_count,
        fidelity_encoded_bytes,
    )?;
    Ok(leagues)
}

fn record_fidelity_opening_leagues(
    encoded: &[u8],
    manifest: &InputManifestV1,
    (bundle, limits, cancelled): (&ProtocolBundleV1, &SchemaLimits, Option<&AtomicBool>),
    leagues: &mut BTreeMap<Address, u16>,
) -> Result<(), WorkerError> {
    let opening = AuthenticatedOpeningV1::decode_canonical_record(encoded, limits)?;
    if opening.source_kind != OpeningSourceKind::Fidelity {
        return Err(WorkerError::UnitBindingMismatch);
    }
    opening.validate_against_bundle(bundle, limits)?;
    let raw = opening
        .decode_and_validate_raw_opening(manifest.checkpoint.finalized_state_root, limits)?;
    let owners = decode_fidelity_subject_key(&opening.canonical_subject_key.0)?;
    let slot_values = raw
        .ordered_slots
        .iter()
        .map(|slot| (slot.slot, slot.value))
        .collect::<BTreeMap<_, _>>();
    for owner in &owners {
        require_lease_active(cancelled)?;
        // Independently re-derive each owner's snapshot slot. Do not
        // trust slot order. The MPT-proven value is the on-chain
        // league that Metadosis committed for this day at prepare
        // time. A slot missing from the opening, a zero word or an
        // out-of-range word means that the owner was not snapshotted.
        // Any of these fails the whole unit with `UnitBindingMismatch`.
        let slot = league_snapshot_slot(manifest.wwd, *owner);
        let word = slot_values
            .get(&slot)
            .copied()
            .filter(|word| !word.is_zero() && *word <= U256::from(u16::MAX))
            .ok_or(WorkerError::UnitBindingMismatch)?;
        if leagues.insert(*owner, word.to::<u16>()).is_some() {
            return Err(WorkerError::UnitBindingMismatch);
        }
    }
    Ok(())
}

fn collect_fidelity_observed_tributes(
    (input_chunks, shard_ordinal): (
        &[(
            outbe_ocomp_protocol::input::InputChunkRefV1,
            outbe_ocomp_protocol::input::AuthenticatedInputChunkV1,
        )],
        u32,
    ),
    enumerated: &EnumeratedRunV1,
    leagues: &BTreeMap<Address, u16>,
    cancelled: Option<&AtomicBool>,
) -> Result<Vec<ObservedTributeV1>, WorkerError> {
    let nominals = shard_tribute_nominals(input_chunks, shard_ordinal)?;
    if nominals.len() != enumerated.ordered_records.len() {
        return Err(WorkerError::UnitBindingMismatch);
    }
    let mut observed = Vec::new();
    observed
        .try_reserve_exact(enumerated.ordered_records.len())
        .map_err(|_| WorkerError::UnitBindingMismatch)?;
    for record in &enumerated.ordered_records {
        require_lease_active(cancelled)?;
        let league = leagues
            .get(&record.tribute.owner)
            .copied()
            .ok_or(WorkerError::UnitBindingMismatch)?;
        let mut tribute = record.tribute.clone();
        refill_nominal(&mut tribute, &nominals)?;
        observed.push(ObservedTributeV1 {
            tribute,
            first_league: ObservationValueV1::Value(league),
            second_league: ObservationValueV1::Value(league),
            entry_price_minor: ObservationValueV1::Unavailable,
            nod_target_available: true,
        });
    }
    Ok(observed)
}
