mod receipt_only;
use crate::snapshot::{
    tests::headers::fingerprint,
    validation::{ocomp::verify_export_inputs, Incomplete},
};
use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
use outbe_node::ocomp::retention::ExportAuthorityV1;
use outbe_ocomp::{
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    control::poc_schema_limits,
    export_binding::{ExportBindingCandidate, ExportedManifestBindingStore},
    export_receipt::{ExportReceiptCandidate, ExportReceiptStore},
    input_artifacts::derive_input_chunk_ref,
    input_ref_catalog::VerifiedInputChunkRefCatalog,
    supervisor::DiscoveryRecord,
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    control::{FinalizedJobSpecV1, FinalizedJobSummaryV1, SnapshotHandoffV1},
    input::{
        AuthenticatedInputChunkV1, CheckpointIdentityV1, Compression, InputChunkKind,
        InputManifestV1,
    },
    intent::{
        ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
        FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
        MetadosisExpectedStatus, NodTargetPreconditionV1, TributeInputBindingV1,
    },
    profile::ProtocolBundleV1,
    registry::{FIDELITY_OPENING_CODEC_ID, ORACLE_OPENING_CODEC_ID, TRIBUTE_BODY_CODEC_ID},
    state::{OcompFinalizedJobV1, OcompJobRecordV1, OcompJobStatus},
    CasObjectRefV1, ListKind, ObjectKind, OrderedListLimits, SnapshotExportCommittedV1,
};
use outbe_primitives::time::WorldwideDay;
use std::{fs, path::PathBuf};
const CAS_LIMITS: CasLimits = CasLimits {
    max_object_bytes: 1_048_576,
    max_total_bytes: 8_388_608,
};
fn hash(byte: u8) -> B256 {
    B256::repeat_byte(if byte == 0 { 0xff } else { byte })
}
fn protocol_bundle() -> ProtocolBundleV1 {
    ProtocolBundleV1 {
        protocol_version: 1,
        fork_id: hash(1),
        intent_codec_id: hash(2),
        finalized_intent_proof_codec_id: hash(3),
        tribute_body_codec_id: TRIBUTE_BODY_CODEC_ID,
        fidelity_opening_codec_id: FIDELITY_OPENING_CODEC_ID,
        oracle_opening_codec_id: ORACLE_OPENING_CODEC_ID,
        result_codec_id: hash(4),
        action_codec_id: hash(5),
        activation_codec_id: hash(6),
        evidence_codec_id: hash(7),
        request_semantics_version: 1,
        lysis_program_semantics_hash: hash(8),
        planner_spec_version: 1,
        reducer_spec_version: 1,
        activation_apply_semantics_hash: hash(9),
        effect_contract_registry_hash: hash(10),
        object_codec_registry_hash: hash(11),
        correctness_profile_id: hash(12),
        capacity_profile_id: hash(13),
        result_signature_profile_id: hash(14),
        finality_verifier_and_vote_domain_id: hash(15),
        consensus_committee_history_schema_version: 1,
        ocomp_committee_schema_version: 1,
        proof_system_and_verifier_key_id: None,
        da_codec_and_binding_verifier_id: None,
        anti_equivocation_journal_schema_hash: hash(16),
        mode_pause_revocation_semantics_hash: hash(17),
        upgrade_fsm_semantics_hash: hash(18),
        release_requirement_catalog_sequence: 1,
        release_requirement_catalog_hash: hash(19),
        release_requirement_catalog_parent_hash: hash(20),
        release_gate_authority_envelope_hash: hash(21),
        release_approval_policy_hash: hash(22),
        release_validator_command_artifact_hash: hash(23),
        consensus_state_schema_version: 1,
        migration_manifest_hash: hash(24),
        required_upgrade_handler_set_hash: hash(25),
    }
}
fn finalized_job_spec(
    seed: u8,
    cursor: u64,
    chain_id: u64,
    genesis_hash: B256,
) -> FinalizedJobSpecV1 {
    let limits = poc_schema_limits();
    let day = 20_260_901_u32;
    let bundle = protocol_bundle();
    let protocol_bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
    let collection_key = hash(seed.wrapping_add(2));
    let collection_root = hash(seed.wrapping_add(3));
    let nominal = U256::from(1);
    let intent = JobIntentV1 {
        chain_id,
        genesis_hash,
        fork_id: bundle.fork_id,
        wwd: day,
        pending_nonce: 0,
        attempt: 0,
        protocol_bundle_hash,
        ce_sealed_root: hash(seed.wrapping_add(5)),
        sealed_tribute_collection_key: collection_key,
        sealed_tribute_collection_root: collection_root,
        authenticated_day_count: 1,
        authenticated_day_nominal: nominal,
        pre_admission_envelope_hash: hash(seed.wrapping_add(6)),
        source_availability_policy_id: hash(seed.wrapping_add(7)),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: nominal,
            previous_vwap: nominal,
            current_vwap: nominal,
            gratis_demand: U256::ZERO,
            day_gratis_limit_minor: U256::ZERO,
            lysis_limit_minor: nominal,
            desis_limit_minor: U256::ZERO,
            request_limit_split_receipt_hash: hash(seed.wrapping_add(8)),
        },
        logical_evaluation_height: cursor,
        logical_evaluation_time: cursor,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: day,
                source_generation: 1,
                collection_key,
                sealed_collection_root: collection_root,
                exact_count: 1,
                exact_nominal_total: nominal,
            },
            nod: NodTargetPreconditionV1 {
                wwd: day,
                target_generation: 1,
                namespace_root_before: hash(seed.wrapping_add(9)),
                max_nod_count: 1,
            },
            contributors: ContributorTargetPreconditionV1 {
                worldwide_day: day,
                expected_series_version: 1,
                max_contributor_count: 1,
                max_eligible_nominal_total: nominal,
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: day,
                pending_nonce: 0,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 1,
            },
        },
        result_validator_set_epoch: 1,
        result_committee_set_hash: hash(seed.wrapping_add(10)),
        result_ocomp_binding_hash: hash(seed.wrapping_add(11)),
        result_member_count: 4,
        result_quorum_threshold: 3,
        custody_committee_epoch_hash: None,
    };
    let finalized_block_hash = hash(seed.wrapping_add(12));
    let finalized_state_root = hash(seed.wrapping_add(13));
    FinalizedJobSpecV1 {
        summary: FinalizedJobSummaryV1 {
            cursor,
            job_id: intent
                .job_id(finalized_block_hash, finalized_state_root, &limits)
                .unwrap(),
            intent_id: intent.intent_id(&limits).unwrap(),
            finalized_block_hash,
            finalized_state_root,
            protocol_bundle_hash,
            open_height: cursor + 1,
            deadline_height: cursor + 1_801,
        },
        canonical_job_intent: BoundedBytes(intent.encode_canonical(&limits).unwrap()),
    }
}
fn job_from_spec(spec: &FinalizedJobSpecV1) -> OcompJobRecordV1 {
    let limits = poc_schema_limits();
    let job = OcompJobRecordV1 {
        intent: JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, &limits).unwrap(),
        intent_height: spec.summary.cursor,
        status: OcompJobStatus::VotingOpen,
        finalized: Some(OcompFinalizedJobV1 {
            job_id: spec.summary.job_id,
            finalized_request_block_hash: spec.summary.finalized_block_hash,
            finalized_request_state_root: spec.summary.finalized_state_root,
            finality_recorded_height: spec.summary.open_height - 4,
            open_height: spec.summary.open_height,
            deadline_height: spec.summary.deadline_height,
            quorum: None,
        }),
        terminal: None,
    };
    job.validate_semantics(&limits).unwrap();
    job
}
struct Fixture {
    directory: tempfile::TempDir,
    binding_root: PathBuf,
    receipt_root: PathBuf,
    catalog_root: PathBuf,
    cas_root: PathBuf,
    job: OcompJobRecordV1,
    binding_ref: CasObjectRefV1,
    receipt_ref: CasObjectRefV1,
    manifest_ref: CasObjectRefV1,
    chunk_ref: CasObjectRefV1,
    manifest_hash: B256,
    committed: SnapshotExportCommittedV1,
}
fn fixture(seed: u8, damage: Option<&str>) -> Fixture {
    let limits = poc_schema_limits();
    let list_limits = OrderedListLimits::new(16, 4096, 4096);
    let bundle = protocol_bundle();
    let mut spec = finalized_job_spec(seed, 90, 1, B256::repeat_byte(250));
    spec.summary.open_height = 94;
    let intent = JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, &limits).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let cas_root = directory.path().join("cas-v1");
    let job_hex = hex::encode(spec.summary.job_id);
    let binding_root = directory
        .path()
        .join("supervisor-v1/export-bindings")
        .join(&job_hex);
    let catalog_root = directory
        .path()
        .join("exporter-v1/input-refs")
        .join(&job_hex);
    let receipt_base = directory.path().join("exporter-v1/receipts");
    let receipt_root = receipt_base.join(&job_hex);
    let bundles = directory.path().join("protocol-bundles-v1");
    fs::create_dir_all(&bundles).unwrap();
    fs::write(
        bundles.join(format!(
            "{}.ocb1",
            hex::encode(spec.summary.protocol_bundle_hash)
        )),
        bundle.encode_canonical(&limits).unwrap(),
    )
    .unwrap();
    let cas_limits = CAS_LIMITS;
    let cas = FilesystemCas::open(&cas_root, CasWriterRole::SnapshotExporter, cas_limits).unwrap();
    let reader = FilesystemCasReader::open(&cas_root, cas_limits).unwrap();
    let day = WorldwideDay::new(intent.wwd);
    let owner = Address::repeat_byte(1);
    let tribute = TributeBodyV1 {
        tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
        owner,
        worldwide_day: day,
        issuance_amount_minor: U256::from(1),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(1),
        reference_currency: 978,
        tribute_price_minor: U256::from(1),
        exclude_from_intex_issuance: false,
    };
    let chunk = AuthenticatedInputChunkV1 {
        protocol_bundle_hash: spec.summary.protocol_bundle_hash,
        job_id: spec.summary.job_id,
        kind: InputChunkKind::Tribute,
        ordinal: 0,
        canonical_records_or_openings: vec![BoundedBytes(encode_tribute_v1(&tribute).unwrap())],
    };
    let mut chunk_ref = cas
        .publish_bytes(&chunk.encode_canonical(&limits).unwrap())
        .unwrap();
    chunk_ref.expected_ocb1_kind = Some(ObjectKind::AuthenticatedInputChunkV1.tag());
    let input_ref =
        derive_input_chunk_ref(&reader.read_verified(&chunk_ref).unwrap(), &bundle, &limits)
            .unwrap()
            .reference;
    let manifest = InputManifestV1 {
        protocol_bundle_hash: spec.summary.protocol_bundle_hash,
        job_id: spec.summary.job_id,
        attempt: intent.attempt,
        checkpoint: CheckpointIdentityV1 {
            finalized_block_number: spec.summary.cursor
                + u64::from(damage == Some("checkpoint_height")),
            finalized_block_hash: spec.summary.finalized_block_hash,
            finalized_state_root: spec.summary.finalized_state_root,
            finalized_ce_root: intent.ce_sealed_root,
            ce_schema_version: u16::try_from(
                outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION,
            )
            .unwrap()
                + u16::from(damage == Some("checkpoint_schema")),
        },
        wwd: intent.wwd,
        sealed_tribute_collection_key: intent.sealed_tribute_collection_key,
        sealed_tribute_collection_root: intent.sealed_tribute_collection_root,
        tribute_count: intent.authenticated_day_count,
        tribute_nominal_total: intent.authenticated_day_nominal,
        input_chunk_count: 1,
        input_chunk_list_root: outbe_ocomp_protocol::ordered_list_root(
            ListKind::InputChunkReferences,
            &[input_ref.encode_canonical_record(&limits).unwrap()],
            list_limits,
        )
        .unwrap(),
        fidelity_opening_root: B256::repeat_byte(201),
        oracle_opening_root: B256::repeat_byte(202),
        exact_encoded_bytes: input_ref.encoded_bytes,
        exact_record_count: input_ref.record_count,
        body_codec_id: bundle.tribute_body_codec_id,
        opening_codec_registry_hash: bundle.opening_codec_registry_hash().unwrap(),
        compression: Compression::None,
    };
    let mut manifest_ref = cas
        .publish_bytes(&manifest.encode_canonical(&limits).unwrap())
        .unwrap();
    manifest_ref.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
    let mut catalog =
        VerifiedInputChunkRefCatalog::open(&catalog_root, &cas, &manifest_ref, limits, list_limits)
            .unwrap();
    catalog.admit(&input_ref).unwrap();
    let committed = SnapshotExportCommittedV1 {
        job_id: spec.summary.job_id,
        pin_generation: 12,
        record_hash: B256::repeat_byte(203),
    };
    // Only the native producer uses the legacy discovery record. The offline
    // consumer below retains the authenticated immutable spec, not this record.
    let discovery = DiscoveryRecord {
        generation: 7,
        cursor: spec.summary.cursor,
        spec: spec.clone(),
    };
    let binding_ref = {
        let mut store = ExportedManifestBindingStore::open(&binding_root, limits).unwrap();
        store
            .seal(
                &cas,
                &reader,
                ExportBindingCandidate {
                    discovery: &discovery,
                    job_id: spec.summary.job_id,
                    source_pin_generation: 11,
                    lease_generation: 17,
                    checkpoint: &manifest.checkpoint,
                    manifest_ref: &manifest_ref,
                    committed: &committed,
                    bundle: &bundle,
                    input_refs: &catalog,
                },
            )
            .unwrap()
            .1
            .binding_ref()
    };
    // Bind a native receipt independently so mismatch cases remain well-formed
    // in each owner and fail only when their public authorities are composed.
    let receipt_source = if damage == Some("receipt_source") {
        21
    } else {
        11
    };
    let receipt_lease = if damage == Some("receipt_lease") {
        18
    } else {
        17
    };
    let receipt_committed = SnapshotExportCommittedV1 {
        job_id: spec.summary.job_id,
        pin_generation: receipt_source + 1,
        record_hash: if damage == Some("receipt_record") {
            B256::repeat_byte(204)
        } else {
            committed.record_hash
        },
    };
    let mut receipt_manifest = manifest.clone();
    let receipt_manifest_ref = if damage == Some("receipt_manifest") {
        receipt_manifest.fidelity_opening_root = B256::repeat_byte(205);
        let mut reference = cas
            .publish_bytes(&receipt_manifest.encode_canonical(&limits).unwrap())
            .unwrap();
        reference.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
        reference
    } else {
        manifest_ref.clone()
    };
    let handoff = SnapshotHandoffV1 {
        job_id: spec.summary.job_id,
        input_lease_id: intent.input_lease_id().unwrap(),
        pin_generation: receipt_source,
        lease_generation: receipt_lease,
        checkpoint: receipt_manifest.checkpoint.clone(),
        canonical_lease_offer: BoundedBytes(vec![1]),
    };
    let receipt_ref = {
        let mut store =
            ExportReceiptStore::open(&receipt_base, spec.summary.job_id, limits).unwrap();
        store
            .record(
                &cas,
                &reader,
                ExportReceiptCandidate {
                    handoff: &handoff,
                    manifest_ref: &receipt_manifest_ref,
                    manifest_hash: receipt_manifest.manifest_hash(&limits).unwrap(),
                    committed: &receipt_committed,
                },
            )
            .unwrap()
            .1
            .receipt_ref()
    };
    // Native seal/load closes the actual catalog. This minimal Tribute-only
    // fixture does not claim opening-proof or full worker-pipeline E2E coverage.
    let job = job_from_spec(&spec);
    let manifest_hash = manifest.manifest_hash(&limits).unwrap();
    drop(catalog);
    drop(reader);
    drop(cas);
    Fixture {
        directory,
        binding_root,
        receipt_root,
        catalog_root,
        cas_root,
        job,
        binding_ref,
        receipt_ref,
        manifest_ref,
        chunk_ref,
        manifest_hash,
        committed,
    }
}

impl Fixture {
    fn expected(&self) -> ExportAuthorityV1 {
        ExportAuthorityV1 {
            source_generation: 11,
            lease_generation: 17,
            manifest_hash: self.manifest_hash,
        }
    }
    fn cas_path(&self, reference: &CasObjectRefV1) -> PathBuf {
        let digest = hex::encode(reference.transport_digest);
        self.cas_root
            .join("objects")
            .join(&digest[..2])
            .join(&digest[2..])
    }
    fn check(&self, expected: Option<ExportAuthorityV1>) -> eyre::Result<(B256, u64)> {
        let before = fingerprint(self.directory.path());
        let result = verify_export_inputs(self.directory.path(), &self.job, expected, CAS_LIMITS)
            .map(|audit| (audit.receipt.manifest_hash(), audit.input_chunks));
        assert_eq!(fingerprint(self.directory.path()), before);
        result
    }
}

#[test]
fn complete_native_export_returns_owned_receipt_binding_without_discovery_or_private_journals() {
    let f = fixture(20, None);
    assert!(!f.directory.path().join("node-v1").exists());
    assert!(!f.directory.path().join("supervisor-v1/discovery").exists());
    for expected in [None, Some(f.expected())] {
        let before = fingerprint(f.directory.path());
        let audit = verify_export_inputs(f.directory.path(), &f.job, expected, CAS_LIMITS).unwrap();
        assert_eq!(audit.receipt.manifest_hash(), f.manifest_hash);
        assert_eq!(audit.input_chunks, 1);
        assert_eq!(audit.receipt.receipt_ref(), f.receipt_ref);
        assert_eq!(audit.binding.binding_ref(), f.binding_ref);
        assert_eq!(audit.receipt.manifest_ref(), f.manifest_ref);
        assert_eq!(audit.binding.manifest_ref(), f.manifest_ref);
        assert_eq!(audit.receipt.committed(), f.committed);
        assert_eq!(
            audit.binding.commit_replay_request(),
            audit.receipt.commit_replay_request()
        );
        assert_eq!(fingerprint(f.directory.path()), before);
    }
}

#[test]
fn absent_required_receipt_binding_catalog_or_cas_is_incomplete() {
    for missing in [
        "receipt_directory",
        "binding_directory",
        "catalog_directory",
        "prepared_locator",
        "receipt_locator",
        "binding_locator",
        "catalog_header",
        "input_ref",
        "receipt_cas",
        "binding_cas",
        "manifest_cas",
        "chunk_cas",
    ] {
        let f = fixture(20, None);
        let path = match missing {
            "receipt_directory" => f.receipt_root.clone(),
            "binding_directory" => f.binding_root.clone(),
            "catalog_directory" => f.catalog_root.clone(),
            "prepared_locator" => f.receipt_root.join("prepared.ref"),
            "receipt_locator" => f.receipt_root.join("receipt.ref"),
            "binding_locator" => f.binding_root.join("binding.ref"),
            "catalog_header" => f.catalog_root.join("catalog.header"),
            "input_ref" => f.catalog_root.join("0000000000.input-ref"),
            "receipt_cas" => f.cas_path(&f.receipt_ref),
            "binding_cas" => f.cas_path(&f.binding_ref),
            "manifest_cas" => f.cas_path(&f.manifest_ref),
            "chunk_cas" => f.cas_path(&f.chunk_ref),
            _ => unreachable!(),
        };
        if path.is_dir() {
            fs::remove_dir_all(&path).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        let error = f.check(Some(f.expected())).unwrap_err();
        assert!(
            error.downcast_ref::<Incomplete>().is_some(),
            "{missing}: {error:#}"
        );
        assert!(!path.exists(), "reader must not recreate {missing}");
    }
}

#[test]
fn changed_cas_bytes_are_failed_and_never_missing_input() {
    for object in ["chunk", "receipt", "binding", "manifest"] {
        let f = fixture(20, None);
        let reference = match object {
            "chunk" => &f.chunk_ref,
            "receipt" => &f.receipt_ref,
            "binding" => &f.binding_ref,
            "manifest" => &f.manifest_ref,
            _ => unreachable!(),
        };
        let path = f.cas_path(reference);
        let mut bytes = fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        fs::write(&path, bytes).unwrap();
        let error = f.check(None).unwrap_err();
        assert!(
            error.downcast_ref::<Incomplete>().is_none(),
            "{object}: {error:#}"
        );
    }
}

#[test]
fn native_consistent_checkpoint_height_and_schema_still_bind_to_canonical_request() {
    for damage in ["checkpoint_height", "checkpoint_schema"] {
        // Native writers accept internally consistent checkpoint descriptors.
        // The local composition must compare height/schema with B/native CE.
        let f = fixture(20, Some(damage));
        let error = f.check(None).unwrap_err();
        assert!(
            error.downcast_ref::<Incomplete>().is_none(),
            "{damage}: {error:#}"
        );
    }
}

#[test]
fn independently_valid_receipt_and_binding_must_describe_same_export() {
    for damage in [
        "receipt_source",
        "receipt_lease",
        "receipt_record",
        "receipt_manifest",
    ] {
        let f = fixture(20, Some(damage));
        // No pin export supplied: the two local native authorities must
        // still agree on request generations, manifest and committed record.
        let error = f.check(None).unwrap_err();
        assert!(
            error.downcast_ref::<Incomplete>().is_none(),
            "{damage}: {error:#}"
        );
    }
}

#[test]
fn canonical_export_source_lease_and_manifest_must_match_receipt() {
    let f = fixture(20, None);
    let expected = f.expected();
    for changed in [
        ExportAuthorityV1 {
            source_generation: 12,
            ..expected
        },
        ExportAuthorityV1 {
            lease_generation: 18,
            ..expected
        },
        ExportAuthorityV1 {
            manifest_hash: hash(0xee),
            ..expected
        },
    ] {
        let error = f.check(Some(changed)).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    }
    assert_eq!(f.check(Some(expected)).unwrap(), (f.manifest_hash, 1));
}

#[test]
fn existing_foreign_job_artifacts_cannot_satisfy_another_canonical_job() {
    let mut f = fixture(20, None);
    let mut other = finalized_job_spec(21, 90, 1, B256::repeat_byte(250));
    other.summary.open_height = 94;
    let foreign = job_from_spec(&other);
    let job_hex = hex::encode(other.summary.job_id);
    // Every required path and CAS object still exists. This must be a binding
    // failure, not a missing-directory classification under the new job key.
    for old in [&f.receipt_root, &f.binding_root, &f.catalog_root] {
        fs::rename(old, old.parent().unwrap().join(&job_hex)).unwrap();
    }
    f.job = foreign;
    let error = f.check(None).unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
}
