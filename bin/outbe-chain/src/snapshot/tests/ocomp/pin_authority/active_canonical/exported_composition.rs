mod all_native;

mod final_join;
use super::*;
use crate::snapshot::validation::{
    ocomp::{verify_canonical_obligations, CanonicalLocalPinStage},
    Incomplete,
};
use alloy_primitives::{keccak256, B256};
use outbe_compressed_entities::encode_tribute_v1;
use outbe_node::ocomp::retention::{
    inspect_retention_journal, CandidatePinV1, ExportAuthorityV1, PinRecordV1, PinStateV1,
};
use outbe_ocomp::{
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
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
    ListKind, ObjectKind, OrderedListLimits, SnapshotExportCommittedV1,
};
use std::{fs, path::Path};
const CAS_LIMITS: CasLimits = CasLimits {
    max_object_bytes: 1_048_576,
    max_total_bytes: u64::MAX,
};

// Actual native writers close the manifest, reference catalog, binding and receipt.
// This fixture exercises a Tribute-only closure, not worker opening-proof E2E.
fn write_export(root: &Path, prepared: &ActiveFixture) -> ExportAuthorityV1 {
    let limits = poc_schema_limits();
    let list_limits = OrderedListLimits::new(16, 4096, 4096);
    let job = &prepared.job;
    let intent = &job.intent;
    let bundle = &prepared.bundle;
    let finalized = job.finalized.as_ref().unwrap();
    let spec = FinalizedJobSpecV1 {
        summary: FinalizedJobSummaryV1 {
            cursor: job.intent_height,
            job_id: finalized.job_id,
            intent_id: intent.intent_id(&limits).unwrap(),
            finalized_block_hash: finalized.finalized_request_block_hash,
            finalized_state_root: finalized.finalized_request_state_root,
            protocol_bundle_hash: intent.protocol_bundle_hash,
            open_height: finalized.open_height,
            deadline_height: finalized.deadline_height,
        },
        canonical_job_intent: BoundedBytes(intent.encode_canonical(&limits).unwrap()),
    };
    let cas_root = root.join("cas-v1");
    let job_hex = hex::encode(spec.summary.job_id);
    let binding_root = root.join("supervisor-v1/export-bindings").join(&job_hex);
    let catalog_root = root.join("exporter-v1/input-refs").join(&job_hex);
    let receipt_base = root.join("exporter-v1/receipts");
    let bundles = root.join("protocol-bundles-v1");
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
    let tribute = outbe_tribute::canonical_body(&super::source_body());
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
        derive_input_chunk_ref(&reader.read_verified(&chunk_ref).unwrap(), bundle, &limits)
            .unwrap()
            .reference;
    let manifest = InputManifestV1 {
        protocol_bundle_hash: spec.summary.protocol_bundle_hash,
        job_id: spec.summary.job_id,
        attempt: intent.attempt,
        checkpoint: CheckpointIdentityV1 {
            finalized_block_number: spec.summary.cursor,
            finalized_block_hash: spec.summary.finalized_block_hash,
            finalized_state_root: spec.summary.finalized_state_root,
            finalized_ce_root: intent.ce_sealed_root,
            ce_schema_version: u16::try_from(
                outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION,
            )
            .unwrap(),
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
    let _binding_ref = {
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
                    bundle,
                    input_refs: &catalog,
                },
            )
            .unwrap()
            .1
            .binding_ref()
    };
    let receipt_source = 11;
    let receipt_lease = 17;
    let receipt_manifest = manifest.clone();
    let receipt_manifest_ref = manifest_ref.clone();
    let receipt_committed = committed.clone();
    let handoff = SnapshotHandoffV1 {
        job_id: spec.summary.job_id,
        input_lease_id: intent.input_lease_id().unwrap(),
        pin_generation: receipt_source,
        lease_generation: receipt_lease,
        checkpoint: receipt_manifest.checkpoint.clone(),
        canonical_lease_offer: BoundedBytes(vec![1]),
    };
    let _receipt_ref = {
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

    ExportAuthorityV1 {
        source_generation: 11,
        lease_generation: 17,
        manifest_hash: manifest.manifest_hash(&limits).unwrap(),
    }
}
// The owner does not expose a public journal writer independent of live frame
// ingestion. Confine the exact native v6 fixture codec to test setup and decode
// it immediately through the owner's public read-only inspector.
fn write_exported_pin(
    root: &Path,
    request: &OutbeHeader,
    job: &OcompJobRecordV1,
    export: ExportAuthorityV1,
) {
    let finalized = job.finalized.as_ref().unwrap();
    let candidate = CandidatePinV1 {
        block_number: request.inner.number,
        block_hash: request.hash_slow(),
        state_root: request.inner.state_root,
        intent_id: job.intent.intent_id(&poc_schema_limits()).unwrap(),
        wwd: job.intent.wwd,
        ce_sealed_root: job.intent.ce_sealed_root,
        protocol_bundle_hash: job.intent.protocol_bundle_hash,
        input_lease_id: job.intent.input_lease_id().unwrap(),
    };
    let generation = 12_u64;
    let record = PinRecordV1 {
        generation,
        state: PinStateV1::Exported {
            candidate,
            job_id: finalized.job_id,
            finality_recorded_height: finalized.finality_recorded_height,
            open_height: finalized.open_height,
            deadline_height: finalized.deadline_height,
            export,
        },
    };
    let mut bytes = b"OUTBPIN1".to_vec();
    bytes.extend_from_slice(&6_u16.to_be_bytes());
    bytes.extend_from_slice(&generation.to_be_bytes());
    bytes.push(3);
    bytes.extend_from_slice(&candidate.block_number.to_be_bytes());
    bytes.extend_from_slice(candidate.block_hash.as_slice());
    bytes.extend_from_slice(candidate.state_root.as_slice());
    bytes.extend_from_slice(candidate.intent_id.as_slice());
    bytes.extend_from_slice(&candidate.wwd.to_be_bytes());
    bytes.extend_from_slice(candidate.ce_sealed_root.as_slice());
    bytes.extend_from_slice(candidate.protocol_bundle_hash.as_slice());
    bytes.extend_from_slice(candidate.input_lease_id.as_slice());
    bytes.extend_from_slice(finalized.job_id.as_slice());
    bytes.extend_from_slice(&finalized.finality_recorded_height.to_be_bytes());
    bytes.extend_from_slice(&finalized.open_height.to_be_bytes());
    bytes.extend_from_slice(&finalized.deadline_height.to_be_bytes());
    bytes.extend_from_slice(&export.source_generation.to_be_bytes());
    bytes.extend_from_slice(&export.lease_generation.to_be_bytes());
    bytes.extend_from_slice(export.manifest_hash.as_slice());
    bytes.extend_from_slice(keccak256(&bytes).as_slice());
    let mut registry = b"OUTBPIN1".to_vec();
    registry.extend_from_slice(&6_u16.to_be_bytes());
    registry.extend_from_slice(&generation.to_be_bytes());
    registry.extend_from_slice(candidate.block_hash.as_slice());
    registry.extend_from_slice(&1_u16.to_be_bytes());
    registry.extend_from_slice(candidate.block_hash.as_slice());
    registry.extend_from_slice(&u16::try_from(bytes.len()).unwrap().to_be_bytes());
    registry.extend_from_slice(&bytes);
    registry.extend_from_slice(keccak256(&registry).as_slice());
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("pin.v1"), registry).unwrap();
    let decoded = inspect_retention_journal(root).unwrap();
    assert_eq!(decoded.records, vec![(candidate.block_hash, record)]);
}

#[test]
fn recorded_exported_requires_complete_native_export_even_when_entire_job_directory_disappears() {
    use reth_ethereum::provider::db::{
        database::Database, init_db, mdbx::DatabaseArguments, tables, transaction::DbTx,
    };
    for version in [1, 2] {
        for deleted in [
            None,
            Some("supervisor-v1/export-bindings"),
            Some("exporter-v1/receipts"),
            Some("exporter-v1/input-refs"),
        ] {
            super::super::super::with_canonical_frontiers(
                version,
                |layout| {
                    write_source(layout);
                    let db =
                        init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
                    let tx = db.tx().unwrap();
                    let request = tx
                        .get::<tables::Headers<OutbeHeader>>(100)
                        .unwrap()
                        .unwrap();
                    drop(tx);
                    drop(db);
                    let prepared = fixture(&request, Phase::VotingOpen, bind_source);
                    let export = write_export(&layout.ocomp_root, &prepared);
                    // Validate the baseline through the native read-only composition
                    // before deleting any whole per-job public directory.
                    crate::snapshot::validation::ocomp::verify_export_inputs(
                        &layout.ocomp_root,
                        &prepared.job,
                        Some(export),
                        CAS_LIMITS,
                    )
                    .unwrap();
                    write_exported_pin(
                        &layout.consensus_root.join("ocomp_retention"),
                        &request,
                        &prepared.job,
                        export,
                    );
                    if let Some(prefix) = deleted {
                        let job_id = prepared.job.finalized.as_ref().unwrap().job_id;
                        fs::remove_dir_all(
                            layout.ocomp_root.join(prefix).join(hex::encode(job_id)),
                        )
                        .unwrap();
                    }
                },
                |request| fixture(request, Phase::VotingOpen, bind_source).owner,
                |state, source, layout, scratch| {
                    let result =
                        verify_canonical_obligations(state, source, layout, scratch, None, None);
                    if let Some(deleted) = deleted {
                        let error = result.err().expect("recorded exported obligation cannot disappear with its entire public directory");
                        assert!(
                            error.downcast_ref::<Incomplete>().is_some(),
                            "{deleted}: {error:#}"
                        );
                        assert!(format!("{error:#}").contains("export"), "must reach required export after complete source verification: {error:#}");
                    } else {
                        let audit = result.unwrap();
                        assert_eq!(audit.bounds.active_intents, 1);
                        assert_eq!(audit.pins.len(), 1);
                        assert!(matches!(
                            audit.pins[0].record.state,
                            PinStateV1::Exported { .. }
                        ));
                        assert!(audit.pins[0].authority.export.is_some());
                        assert_eq!(audit.active.len(), 1);
                        assert_eq!(audit.active[0].pin_stage, CanonicalLocalPinStage::Exported);
                        assert!(audit.active[0].source_verified);
                        assert!(audit.active[0].export_verified);
                        assert!(!audit.active[0].projection_before_request);
                        assert_eq!(
                            audit.source_leases, 1,
                            "pin and active authority share one source lease"
                        );
                        assert_eq!(
                            audit.complete_exports, 1,
                            "pin and active authority share one export"
                        );
                        assert_eq!(audit.input_chunks, 1);
                    }
                },
            );
        }
    }
}
