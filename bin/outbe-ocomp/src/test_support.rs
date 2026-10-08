//! Synthetic protocol fixtures for catalog and materialization tests.
//! These builders do not execute the worker pipeline.

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{derive_poseidon_entity_id, TributeBodyV1};
use outbe_lysis::program_v1::{
    planner::{LysisPlannerBindingsV1, LysisPlannerV1},
    result::{LysisListSubtreeCarrierV1, RootReduceSummaryV1},
};
use outbe_ocomp_protocol::{
    common::ProofBytes,
    input::{materialize_authenticated_openings, AuthenticatedOpeningV1, InputManifestV1},
    opening::{
        partition_lysis_opening_subjects, LysisOpeningsProofV1, RawContractOpeningProofV1,
        RawStorageSlotV1,
    },
    profile::ProtocolBundleV1,
    result::{ContributorActionV1, NodActionV1, OutputManifestEntryV1, ResultChunkV1},
    CasObjectRefV1, ListKind, SchemaLimits,
};
use outbe_primitives::time::WorldwideDay;

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

pub fn tribute_population(day: WorldwideDay, tribute_count: u32) -> Vec<TributeBodyV1> {
    let mut tributes = (0..tribute_count)
        .map(|index| {
            let mut owner_bytes = [0_u8; 20];
            owner_bytes[16..].copy_from_slice(&(index + 1).to_be_bytes());
            let owner = Address::from(owner_bytes);
            TributeBodyV1 {
                tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
                owner,
                worldwide_day: day,
                issuance_amount_minor: U256::from(1),
                issuance_currency: if index % 2 == 0 { 840 } else { 826 },
                nominal_amount_minor: U256::from((index % 7) + 1),
                reference_currency: if index % 3 == 0 { 978 } else { 392 },
                tribute_price_minor: U256::from(1),
                exclude_from_intex_issuance: false,
            }
        })
        .collect::<Vec<_>>();
    tributes.sort_by_key(|tribute| tribute.tribute_id);
    tributes
}
pub fn contributor_population(tributes: &[TributeBodyV1]) -> Vec<ContributorActionV1> {
    let mut contributors_by_owner = tributes
        .iter()
        .map(|tribute| ContributorActionV1 {
            owner: tribute.owner,
            source_tribute_id: *tribute.tribute_id,
            nominal_amount_minor: tribute.nominal_amount_minor,
        })
        .collect::<Vec<_>>();
    contributors_by_owner
        .sort_by_key(|contributor| (contributor.owner, contributor.source_tribute_id));
    contributors_by_owner
}
pub fn nod_actions(tributes: &[TributeBodyV1], first_ordinal: u32) -> Vec<NodActionV1> {
    tributes
        .iter()
        .enumerate()
        .map(|(local, tribute)| {
            let tribute_id = *tribute.tribute_id;
            NodActionV1 {
                raw_ordinal: first_ordinal
                    .checked_add(u32::try_from(local).unwrap())
                    .unwrap(),
                tribute_id,
                nod_id: tribute_id,
                owner: tribute.owner,
                wwd: tribute.worldwide_day.value(),
                league_id: 1,
                gratis_load_minor: U256::ONE,
                entry_price_minor: U256::ZERO,
                settlement_cost_minor: U256::from(2),
                issuance_currency: tribute.issuance_currency,
                reference_currency: tribute.reference_currency,
            }
        })
        .collect()
}
pub struct FixtureOpenings {
    pub fidelity: Vec<AuthenticatedOpeningV1>,
    pub oracle: AuthenticatedOpeningV1,
}
pub fn fixture_openings(
    bundle: &ProtocolBundleV1,
    job_id: B256,
    day: WorldwideDay,
    tributes: &[TributeBodyV1],
    limits: &SchemaLimits,
) -> FixtureOpenings {
    let bundle_hash = bundle.protocol_bundle_hash(limits).unwrap();
    let owners = tributes
        .iter()
        .map(|tribute| tribute.owner)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut reference_isos = tributes
        .iter()
        .map(|tribute| tribute.reference_currency)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    reference_isos.push(840);
    reference_isos.sort_unstable();
    reference_isos.dedup();
    let finalized_state_root = hash(0x32);
    let raw_opening = |address, slot_byte| RawContractOpeningProofV1 {
        contract_address: address,
        state_root: finalized_state_root,
        ordered_slots: vec![RawStorageSlotV1 {
            slot: hash(slot_byte),
            value: U256::from(1),
        }],
        account_proof: ProofBytes(vec![0xa1]),
        storage_proof: ProofBytes(vec![0xb1]),
    };
    let mut fidelity_openings = Vec::new();
    let mut oracle_opening = None;
    for subjects in partition_lysis_opening_subjects(&owners, &reference_isos, limits).unwrap() {
        let openings = materialize_authenticated_openings(
            &LysisOpeningsProofV1 {
                protocol_bundle_hash: bundle_hash,
                job_id,
                finalized_block_hash: hash(0x31),
                finalized_state_root,
                wwd: day.value(),
                subjects,
                fidelity: raw_opening(Address::repeat_byte(0x63), 0x64),
                oracle: raw_opening(Address::repeat_byte(0x65), 0x66),
            },
            bundle,
            limits,
        )
        .unwrap();
        fidelity_openings.push(openings.fidelity);
        match &oracle_opening {
            None => oracle_opening = Some(openings.oracle),
            Some(existing) => assert_eq!(existing, &openings.oracle),
        }
    }

    FixtureOpenings {
        fidelity: fidelity_openings,
        oracle: oracle_opening.unwrap(),
    }
}
pub fn fixture_planner(
    bundle: &ProtocolBundleV1,
    job_id: B256,
    manifest_ref: &CasObjectRefV1,
    manifest: &InputManifestV1,
    limits: &SchemaLimits,
) -> LysisPlannerV1 {
    let bundle_hash = bundle.protocol_bundle_hash(limits).unwrap();
    LysisPlannerV1::new(LysisPlannerBindingsV1 {
        protocol_bundle_hash: bundle_hash,
        job_id,
        attempt: 0,
        input_manifest_hash: manifest.manifest_hash(limits).unwrap(),
        input_manifest_encoded_bytes: manifest_ref.encoded_bytes,
        fidelity_opening_root: manifest.fidelity_opening_root,
        oracle_opening_root: manifest.oracle_opening_root,
        wwd: manifest.wwd,
        lysis_limit_minor: U256::from(200),
        logical_evaluation_time: 1_784_765_900,
        tribute_count: manifest.tribute_count,
        lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
        planner_spec_version: bundle.planner_spec_version,
        reducer_spec_version: bundle.reducer_spec_version,
    })
    .unwrap()
}

pub fn root_summary_fixture(
    plan_hash: B256,
    chunk: &ResultChunkV1,
    tributes: &[TributeBodyV1],
    entry: &OutputManifestEntryV1,
    limits: &SchemaLimits,
) -> RootReduceSummaryV1 {
    let index = chunk.chunk_ordinal;
    let actions = &chunk.ordered_nod_actions;
    let contributors = &chunk.ordered_eligible_contributors;
    let start = usize::try_from(chunk.first_nod_ordinal).unwrap();
    let count = u32::try_from(actions.len()).unwrap();
    let nod_records = actions
        .iter()
        .map(|action| action.encode_canonical_record(limits).unwrap())
        .collect::<Vec<_>>();
    let bucket_records = (start..start + actions.len())
        .map(|ordinal| ordinal.to_be_bytes().to_vec())
        .collect::<Vec<_>>();
    let contributor_records = contributors
        .iter()
        .map(|action| action.encode_canonical_record(limits).unwrap())
        .collect::<Vec<_>>();
    let manifest_records = vec![entry.encode_canonical_record(limits).unwrap()];
    let chunk_hash_records = vec![entry.result_chunk_hash.as_slice().to_vec()];
    let carrier = |kind, records: &[Vec<u8>]| {
        LysisListSubtreeCarrierV1::from_primary_page(kind, index, records, limits.max_bounded_bytes)
            .unwrap()
    };
    let raw_nominal_total = tributes.iter().fold(U256::ZERO, |total, tribute| {
        total.checked_add(tribute.nominal_amount_minor).unwrap()
    });
    let nod_cost_total = actions.iter().fold(U256::ZERO, |total, action| {
        total.checked_add(action.settlement_cost_minor).unwrap()
    });
    RootReduceSummaryV1 {
        protocol_bundle_hash: chunk.protocol_bundle_hash,
        job_id: chunk.job_id,
        attempt: 0,
        plan_hash,
        covered_primary_start: index,
        covered_primary_count: 1,
        nod_actions: carrier(ListKind::NodActions, &nod_records),
        bucket_records: carrier(ListKind::BucketRecords, &bucket_records),
        contributor_actions: carrier(ListKind::ContributorActions, &contributor_records),
        output_manifest_entries: carrier(ListKind::CompleteOutputManifest, &manifest_records),
        result_chunk_hashes: LysisListSubtreeCarrierV1::from_primary_page(
            ListKind::ResultChunkHashes,
            index,
            &chunk_hash_records,
            B256::len_bytes(),
        )
        .unwrap(),
        tribute_count: count,
        nod_count: count,
        bucket_count: count,
        contributor_count: u32::try_from(contributors.len()).unwrap(),
        tribute_nominal_total: raw_nominal_total,
        eligible_nominal_total: raw_nominal_total,
        lysis_allocation_minor: U256::from(count),
        nod_cost_total,
        first_error_ordinal: None,
    }
}
