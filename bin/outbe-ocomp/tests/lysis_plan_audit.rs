mod support;

use alloy_primitives::{Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
use outbe_lysis::program_v1::planner::{
    LysisPlanTopologyV1, LysisPlannerBindingsV1, LysisPlannerV1, PlannedUnitPositionV1,
};
use outbe_ocomp::{
    admission_catalog::{AdmissionPositionV1, VerifiedAdmissionCatalog},
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    control::poc_schema_limits,
    input_artifacts::{
        poc_input_list_limits, publish_input_artifact_set, InputArtifactContents,
        InputArtifactIdentity,
    },
    input_ref_catalog::{InputRefCatalogError, VerifiedInputChunkRefCatalog},
    lysis_plan_audit::{ExactLysisPlanError, LocalLysisPlanAuditV1, LysisPlanAuditStepV1},
};
use outbe_ocomp_protocol::{
    common::{BoundedBytes, ProofBytes},
    input::{
        materialize_authenticated_openings, CheckpointIdentityV1, InputChunkKind, InputManifestV1,
    },
    opening::{
        partition_lysis_opening_subjects, LysisOpeningsProofV1, RawContractOpeningProofV1,
        RawStorageSlotV1,
    },
    registry::ObjectKind,
    result::OutputManifestEntryV1,
    unit::{UnitArtifactV1, UnitPhase, WorkOutputHeaderV1},
    CasObjectRefV1,
};

const CAS_LIMITS: CasLimits = CasLimits {
    max_object_bytes: 1_048_576,
    max_total_bytes: 64 * 1_048_576,
};

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

struct Fixture {
    _directory: tempfile::TempDir,
    cas_root: std::path::PathBuf,
    input_ref_root: std::path::PathBuf,
    admission_root: std::path::PathBuf,
    limits: outbe_ocomp_protocol::SchemaLimits,
    bundle: PinnedProtocolBundle,
    target_ordinal: u32,
    expected_count: u32,
}

fn fixture(substitute_bucket_spec: bool, corrupt_fidelity_root: bool) -> Fixture {
    let limits = poc_schema_limits();
    let list_limits = poc_input_list_limits();
    let bundle = support::protocol_bundle();
    let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
    let pinned_bundle = PinnedProtocolBundle::decode(
        &bundle.encode_canonical(&limits).unwrap(),
        bundle_hash,
        &limits,
    )
    .unwrap();
    let job_id = hash(0x30);
    let day = WorldwideDay::new(20_260_725);

    let mut tributes = (0..257_u32)
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
                nominal_amount_minor: U256::from(1),
                reference_currency: if index % 3 == 0 { 978 } else { 392 },
                tribute_price_minor: U256::from(1),
                exclude_from_intex_issuance: false,
            }
        })
        .collect::<Vec<_>>();
    tributes.sort_by_key(|tribute| tribute.tribute_id);
    let owners = tributes
        .iter()
        .map(|tribute| tribute.owner)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut settlement_isos = tributes
        .iter()
        .map(|tribute| tribute.reference_currency)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    settlement_isos.push(840);
    settlement_isos.sort_unstable();
    settlement_isos.dedup();
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
    for subjects in partition_lysis_opening_subjects(&owners, &settlement_isos, &limits).unwrap() {
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
            &bundle,
            &limits,
        )
        .unwrap();
        fidelity_openings.push(openings.fidelity);
        match &oracle_opening {
            None => oracle_opening = Some(openings.oracle),
            Some(existing) => assert_eq!(existing, &openings.oracle),
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let cas_root = directory.path().join("cas");
    let input_ref_root = directory.path().join("input-refs");
    let admission_root = directory.path().join("admissions");
    let cas = FilesystemCas::open(&cas_root, CasWriterRole::Supervisor, CAS_LIMITS).unwrap();
    let published = publish_input_artifact_set(
        &cas,
        &input_ref_root,
        &bundle,
        InputArtifactContents {
            identity: InputArtifactIdentity {
                job_id,
                attempt: 1,
                checkpoint: CheckpointIdentityV1 {
                    finalized_block_number: 90,
                    finalized_block_hash: hash(0x31),
                    finalized_state_root,
                    finalized_ce_root: hash(0x33),
                    ce_schema_version: 1,
                },
                wwd: day.value(),
                sealed_tribute_collection_key: hash(0x34),
                sealed_tribute_collection_root: hash(0x35),
            },
            canonical_tributes: tributes
                .iter()
                .map(|tribute| encode_tribute_v1(tribute).unwrap())
                .collect(),
            fidelity_openings,
            oracle_opening: oracle_opening.unwrap(),
        },
        &limits,
        list_limits,
    )
    .unwrap();
    let mut manifest = InputManifestV1::decode_canonical(
        cas.read_verified(&published.manifest_ref).unwrap().bytes(),
        &limits,
    )
    .unwrap();
    let input_refs_for_plan = VerifiedInputChunkRefCatalog::open(
        &input_ref_root,
        &cas,
        &published.manifest_ref,
        limits,
        list_limits,
    )
    .unwrap();
    let all_input_refs = input_refs_for_plan
        .exact_cursor()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let tribute_refs = all_input_refs
        .iter()
        .filter(|reference| reference.kind == InputChunkKind::Tribute)
        .cloned()
        .collect::<Vec<_>>();
    drop(input_refs_for_plan);
    let (manifest_ref, selected_input_ref_root) = if corrupt_fidelity_root {
        manifest.fidelity_opening_root = hash(0xee);
        let mut manifest_ref = cas
            .publish_bytes(&manifest.encode_canonical(&limits).unwrap())
            .unwrap();
        manifest_ref.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
        let selected_input_ref_root = directory.path().join("input-refs-corrupt-root");
        let mut catalog = VerifiedInputChunkRefCatalog::open(
            &selected_input_ref_root,
            &cas,
            &manifest_ref,
            limits,
            list_limits,
        )
        .unwrap();
        for reference in &all_input_refs {
            catalog.admit(reference).unwrap();
        }
        catalog.exact_cursor().unwrap().for_each(|item| {
            item.unwrap();
        });
        drop(catalog);
        (manifest_ref, selected_input_ref_root)
    } else {
        (published.manifest_ref, input_ref_root)
    };

    let planner = LysisPlannerV1::new(LysisPlannerBindingsV1 {
        protocol_bundle_hash: bundle_hash,
        job_id,
        attempt: 1,
        input_manifest_hash: manifest.manifest_hash(&limits).unwrap(),
        input_manifest_encoded_bytes: manifest_ref.encoded_bytes,
        fidelity_opening_root: manifest.fidelity_opening_root,
        oracle_opening_root: manifest.oracle_opening_root,
        wwd: manifest.wwd,
        lysis_budget: U256::from(200),
        logical_evaluation_time: 1_784_765_900,
        tribute_count: manifest.tribute_count,
        lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
        planner_spec_version: bundle.planner_spec_version,
        reducer_spec_version: bundle.reducer_spec_version,
    })
    .unwrap();
    let plan = planner
        .commit_primary_catalog(tribute_refs.clone(), &limits)
        .unwrap();
    let plan_ref = cas
        .publish_bytes(&plan.encode_canonical_record(&limits).unwrap())
        .unwrap();

    let reader = FilesystemCasReader::open(&cas_root, CAS_LIMITS).unwrap();
    let input_refs = VerifiedInputChunkRefCatalog::reopen(
        &selected_input_ref_root,
        &reader,
        limits,
        list_limits,
    )
    .unwrap();
    let mut admissions =
        VerifiedAdmissionCatalog::open(&admission_root, &cas, &plan_ref, &manifest_ref, limits)
            .unwrap();
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count).unwrap();
    let target_ordinal = topology.phase_offset(UnitPhase::BucketShuffle).unwrap()
        + topology.phase_unit_count(UnitPhase::BucketShuffle)
        - 1;

    for plan_ordinal in 0..topology.total_unit_count() {
        let mut spec = {
            let audit = LocalLysisPlanAuditV1::open(
                &admissions,
                &input_refs,
                &reader,
                &pinned_bundle,
                &limits,
            )
            .unwrap();
            audit.candidate_spec_at(plan_ordinal).unwrap()
        };
        if substitute_bucket_spec && plan_ordinal == target_ordinal {
            let producer = spec
                .canonical_ordered_inputs
                .get_mut(1)
                .expect("merged Bucket spec has producer input");
            producer.source_id = hash(0xf1);
            spec.validate_semantics(&limits).unwrap();
        }
        let artifact = UnitArtifactV1::from_canonical_output(
            &spec,
            WorkOutputHeaderV1 {
                source_coverage_root: hash(0xa1),
                output_coverage_root: hash(0xa2),
                source_coverage_count: 1,
                output_coverage_count: 1,
            },
            BoundedBytes(vec![0x42]),
            &limits,
        )
        .unwrap();
        let mut artifact_ref = cas
            .publish_bytes(&artifact.encode_canonical(&limits).unwrap())
            .unwrap();
        artifact_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
        let result_entry = match topology.plan_position_at(plan_ordinal).unwrap() {
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::RootReduce,
                level: 0,
                index,
            } => Some(OutputManifestEntryV1 {
                chunk_ordinal: index,
                result_chunk_hash: hash(0xb1),
                result_chunk_ref: CasObjectRefV1 {
                    transport_digest: hash(0xb2),
                    encoded_bytes: 1,
                    expected_ocb1_kind: Some(ObjectKind::ResultChunkV1.tag()),
                },
            }),
            _ => None,
        };
        admissions
            .admit_verified_unit(
                AdmissionPositionV1 { plan_ordinal },
                &spec,
                artifact_ref,
                result_entry,
            )
            .unwrap();
    }
    drop(admissions);
    drop(input_refs);
    drop(reader);
    drop(cas);

    Fixture {
        _directory: directory,
        cas_root,
        input_ref_root: selected_input_ref_root,
        admission_root,
        limits,
        bundle: pinned_bundle,
        target_ordinal,
        expected_count: topology.total_unit_count(),
    }
}

#[test]
fn cold_restart_rejects_a_self_consistent_artifact_for_the_wrong_plan_spec() {
    let fixture = fixture(true, false);
    let reader = FilesystemCasReader::open(&fixture.cas_root, CAS_LIMITS).unwrap();
    let input_refs = VerifiedInputChunkRefCatalog::reopen(
        &fixture.input_ref_root,
        &reader,
        fixture.limits,
        poc_input_list_limits(),
    )
    .unwrap();
    let admissions =
        VerifiedAdmissionCatalog::reopen(&fixture.admission_root, &reader, fixture.limits).unwrap();
    let audit = LocalLysisPlanAuditV1::open(
        &admissions,
        &input_refs,
        &reader,
        &fixture.bundle,
        &fixture.limits,
    )
    .unwrap();

    let error = audit
        .audit_cursor()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap_err();
    assert!(matches!(
        error,
        ExactLysisPlanError::UnexpectedUnitId { plan_ordinal }
            if plan_ordinal == fixture.target_ordinal
    ));
}

#[test]
fn cold_restart_streams_the_complete_exact_plan_when_every_spec_matches() {
    let fixture = fixture(false, false);
    let reader = FilesystemCasReader::open(&fixture.cas_root, CAS_LIMITS).unwrap();
    let input_refs = VerifiedInputChunkRefCatalog::reopen(
        &fixture.input_ref_root,
        &reader,
        fixture.limits,
        poc_input_list_limits(),
    )
    .unwrap();
    let admissions =
        VerifiedAdmissionCatalog::reopen(&fixture.admission_root, &reader, fixture.limits).unwrap();
    let audit = LocalLysisPlanAuditV1::open(
        &admissions,
        &input_refs,
        &reader,
        &fixture.bundle,
        &fixture.limits,
    )
    .unwrap();

    let mut cursor = audit.audit_cursor().unwrap();
    let first = cursor.next().unwrap().unwrap();
    assert_eq!(
        first,
        LysisPlanAuditStepV1::InputChecked {
            ordinal: 0,
            kind: InputChunkKind::Tribute,
        }
    );
    let mut steps = vec![first];
    steps.extend(cursor.collect::<Result<Vec<_>, _>>().unwrap());
    assert_eq!(steps.last(), Some(&LysisPlanAuditStepV1::Complete));
    let verified = steps
        .iter()
        .filter_map(|step| match step {
            LysisPlanAuditStepV1::Artifact(artifact) => Some(artifact),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        u32::try_from(verified.len()).unwrap(),
        fixture.expected_count
    );
    for (ordinal, item) in verified.iter().enumerate() {
        assert_eq!(item.plan_ordinal(), u32::try_from(ordinal).unwrap());
        assert_eq!(
            item.artifact().unit_id,
            item.spec().unit_id(&fixture.limits).unwrap()
        );
    }
}

#[test]
fn fidelity_membership_search_reports_bounded_progress_before_the_next_input_chunk() {
    let fixture = fixture(false, false);
    let reader = FilesystemCasReader::open(&fixture.cas_root, CAS_LIMITS).unwrap();
    let input_refs = VerifiedInputChunkRefCatalog::reopen(
        &fixture.input_ref_root,
        &reader,
        fixture.limits,
        poc_input_list_limits(),
    )
    .unwrap();
    let admissions =
        VerifiedAdmissionCatalog::reopen(&fixture.admission_root, &reader, fixture.limits).unwrap();
    let audit = LocalLysisPlanAuditV1::open(
        &admissions,
        &input_refs,
        &reader,
        &fixture.bundle,
        &fixture.limits,
    )
    .unwrap();
    let mut cursor = audit.audit_cursor().unwrap();
    loop {
        if cursor.next().unwrap().unwrap()
            == (LysisPlanAuditStepV1::InputChecked {
                ordinal: 2,
                kind: InputChunkKind::Fidelity,
            })
        {
            break;
        }
    }

    let mut checked_owners = 0_usize;
    let mut search_probes = 0_usize;
    loop {
        match cursor.next().unwrap().unwrap() {
            LysisPlanAuditStepV1::FidelityOwnerMembershipProbe { .. } => {
                search_probes += 1;
            }
            LysisPlanAuditStepV1::FidelityOwnerMembershipChecked { .. } => {
                checked_owners += 1;
            }
            LysisPlanAuditStepV1::InputChecked {
                ordinal: 3,
                kind: InputChunkKind::Fidelity,
            } => break,
            other => panic!("unexpected audit progress before the next Fidelity input: {other:?}"),
        }
    }
    assert_eq!(checked_owners, 256);
    assert!(search_probes > 0);
}

#[test]
fn cold_restart_rejects_manifest_opening_roots_not_derived_from_the_cas_inputs() {
    let fixture = fixture(false, true);
    let reader = FilesystemCasReader::open(&fixture.cas_root, CAS_LIMITS).unwrap();
    let input_refs = VerifiedInputChunkRefCatalog::reopen(
        &fixture.input_ref_root,
        &reader,
        fixture.limits,
        poc_input_list_limits(),
    )
    .unwrap();
    let admissions =
        VerifiedAdmissionCatalog::reopen(&fixture.admission_root, &reader, fixture.limits).unwrap();
    let audit = LocalLysisPlanAuditV1::open(
        &admissions,
        &input_refs,
        &reader,
        &fixture.bundle,
        &fixture.limits,
    )
    .unwrap();

    let mut artifact_was_released = false;
    let mut cursor = audit.audit_cursor().unwrap();
    let error = loop {
        match cursor.next() {
            Some(Ok(LysisPlanAuditStepV1::Artifact(_))) => artifact_was_released = true,
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("corrupt Fidelity root must prevent exact closure"),
        }
    };
    assert!(!artifact_was_released);
    assert!(matches!(error, ExactLysisPlanError::AuthorityMismatch(_)));
}

#[test]
fn exact_closure_reports_bounded_progress_before_rejecting_a_later_catalog_tail() {
    let fixture = fixture(false, false);
    std::fs::write(
        fixture.input_ref_root.join("unexpected-tail"),
        b"not a catalog record",
    )
    .unwrap();
    let reader = FilesystemCasReader::open(&fixture.cas_root, CAS_LIMITS).unwrap();
    let input_refs = VerifiedInputChunkRefCatalog::reopen(
        &fixture.input_ref_root,
        &reader,
        fixture.limits,
        poc_input_list_limits(),
    )
    .unwrap();
    let admissions =
        VerifiedAdmissionCatalog::reopen(&fixture.admission_root, &reader, fixture.limits).unwrap();
    let audit = LocalLysisPlanAuditV1::open(
        &admissions,
        &input_refs,
        &reader,
        &fixture.bundle,
        &fixture.limits,
    )
    .unwrap();

    let mut cursor = audit.audit_cursor().unwrap();
    assert_eq!(
        cursor.next().unwrap().unwrap(),
        LysisPlanAuditStepV1::InputChecked {
            ordinal: 0,
            kind: InputChunkKind::Tribute,
        }
    );
    let error = loop {
        match cursor.next() {
            Some(Ok(LysisPlanAuditStepV1::Artifact(_))) => {
                panic!("input catalog must close before any artifact is released")
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("unexpected catalog tail must prevent exact closure"),
        }
    };
    assert!(matches!(
        error,
        ExactLysisPlanError::InputRef(InputRefCatalogError::UnexpectedCatalogEntry(_))
    ));
}
