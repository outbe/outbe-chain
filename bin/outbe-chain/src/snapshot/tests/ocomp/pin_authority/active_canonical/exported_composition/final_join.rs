use crate::snapshot::tests::projection_fixture::PartitionFixtureStore as RocksDbStorage;
fn install_active_result_request_frame(
    layout: &crate::snapshot::config::RequestedLayout,
) -> OutbeHeader {
    use alloy_consensus::{SignableTransaction, TxLegacy};
    use alloy_primitives::{Log, Signature};
    use alloy_sol_types::SolEvent;
    use outbe_metadosis::precompile::IMetadosis;
    use outbe_offchain_data::{ProjectionState, STORAGE_SCHEMA_VERSION};
    use outbe_offchain_storage::{Key, Namespace, StorageWriter, Value};
    use outbe_primitives::{
        addresses::METADOSIS_ADDRESS, projection::ProjectionCheckpoint, OutbePrimitives,
        OutbeReceipt, OutbeTxEnvelope,
    };
    use reth_ethereum::provider::db::{models::StoredBlockBodyIndices, transaction::DbTxMut};
    use reth_provider::{
        providers::StaticFileProviderBuilder, StaticFileSegment, StaticFileWriter,
    };
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    let mut header = tx
        .get::<tables::Headers<OutbeHeader>>(100)
        .unwrap()
        .unwrap();
    // The native planner requires a nonzero frozen logical time.
    // Set the request header before deriving the event and canonical owner.
    header.inner.timestamp = 1_000;
    let prepared = fixture_for_identity(
        &header,
        Phase::VotingOpen,
        layout.chain.chain().id(),
        layout.chain.genesis_hash(),
        bind_source,
    );
    let intent = &prepared.job.intent;
    let event = IMetadosis::OffchainJobRequested {
        intentId: intent.intent_id(&poc_schema_limits()).unwrap(),
        wwd: intent.wwd,
        pendingNonce: intent.pending_nonce,
        attempt: intent.attempt,
        activationPreconditionsHash: intent
            .activation_preconditions
            .activation_preconditions_hash(&poc_schema_limits())
            .unwrap(),
    };
    let receipts = [OutbeReceipt {
        success: true,
        cumulative_gas_used: 21_000,
        logs: vec![Log {
            address: METADOSIS_ADDRESS,
            data: event.encode_log_data(),
        }],
        ..Default::default()
    }];
    let transactions: Vec<OutbeTxEnvelope> = vec![TxLegacy {
        gas_limit: 21_000,
        ..Default::default()
    }
    .into_signed(Signature::new(U256::ONE, U256::ONE, false))
    .into()];
    header.inner.gas_limit = 30_000_000;
    header.inner.gas_used = 21_000;
    header.inner.transactions_root =
        alloy_consensus::proofs::calculate_transaction_root(&transactions);
    header.inner.receipts_root = alloy_consensus::proofs::calculate_receipt_root(
        &receipts
            .iter()
            .map(alloy_consensus::TxReceipt::with_bloom_ref)
            .collect::<Vec<_>>(),
    );
    let hash = header.hash_slow();
    tx.put::<tables::Headers<OutbeHeader>>(100, header.clone())
        .unwrap();
    tx.put::<tables::CanonicalHeaders>(100, hash).unwrap();
    let mut next = tx
        .get::<tables::Headers<OutbeHeader>>(101)
        .unwrap()
        .unwrap();
    next.inner.parent_hash = hash;
    next.inner.timestamp = 1_001;
    tx.put::<tables::CanonicalHeaders>(101, next.hash_slow())
        .unwrap();
    tx.put::<tables::Headers<OutbeHeader>>(101, next).unwrap();
    tx.put::<tables::BlockBodyIndices>(
        100,
        StoredBlockBodyIndices {
            first_tx_num: 0,
            tx_count: 1,
        },
    )
    .unwrap();
    tx.put::<tables::Receipts<OutbeReceipt>>(0, receipts[0].clone())
        .unwrap();
    tx.commit().unwrap();
    drop(db);
    let files = StaticFileProviderBuilder::read_write(&layout.static_files_root)
        .with_blocks_per_file(1_000)
        .build::<OutbePrimitives>()
        .unwrap();
    {
        let mut writer = files
            .get_writer(0, StaticFileSegment::Transactions)
            .unwrap();
        for height in 0..=100 {
            writer.increment_block(height).unwrap();
        }
        writer.append_transaction(0, &transactions[0]).unwrap();
    }
    files.commit().unwrap();
    drop(files);
    // Keep native setup frontiers attached to the now-complete request header.
    let location = layout.projection.as_ref().unwrap();
    let projection = RocksDbStorage::open(&location.root).unwrap();
    let point = ProjectionCheckpoint {
        block_number: 100,
        block_hash: hash,
    };
    let state = ProjectionState {
        chain_id: layout.chain.chain().id(),
        genesis_hash: layout.chain.genesis_hash(),
        storage_schema_version: STORAGE_SCHEMA_VERSION,
        start_block: location.start_block,
        checkpoint: Some(point),
    };
    projection
        .put(
            Namespace::new("projection_state").unwrap(),
            &Key::new(b"offchain_data".to_vec()).unwrap(),
            &Value::new(postcard::to_stdvec(&state).unwrap()).unwrap(),
        )
        .unwrap();
    drop(projection);
    let closure_root = layout
        .ocomp_root
        .join("exporter-v1/discovery/closure-checkpoint-v1");
    fs::remove_dir_all(&closure_root).unwrap();
    let baseline = ProjectionCheckpoint {
        block_number: 0,
        block_hash: layout.chain.genesis_hash(),
    };
    let closure =
        outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1::open(&closure_root, baseline)
            .unwrap();
    closure.compare_and_advance_to(baseline, point).unwrap();
    drop(closure);
    header
}

fn write_empty_native_plan(root: &Path, prepared: &ActiveFixture) -> (B256, B256) {
    use outbe_lysis::program_v1::planner::{LysisPlannerBindingsV1, LysisPlannerV1};
    use outbe_ocomp::{
        admission_catalog::VerifiedAdmissionCatalog, export_receipt::ExportReceiptReader,
    };
    let limits = poc_schema_limits();
    let job = &prepared.job;
    let id = job.finalized.as_ref().unwrap().job_id;
    let cas =
        FilesystemCas::open(root.join("cas-v1"), CasWriterRole::Supervisor, CAS_LIMITS).unwrap();
    let reader = FilesystemCasReader::open(root.join("cas-v1"), CAS_LIMITS).unwrap();
    let receipt = ExportReceiptReader::open(root.join("exporter-v1/receipts"), id, limits)
        .unwrap()
        .load_exact(&reader)
        .unwrap();
    let manifest = receipt.manifest();
    let manifest_ref = receipt.manifest_ref();
    let inputs = VerifiedInputChunkRefCatalog::reopen(
        root.join("exporter-v1/input-refs").join(hex::encode(id)),
        &reader,
        limits,
        OrderedListLimits::new(16, 4096, 4096),
    )
    .unwrap();
    let refs = inputs
        .exact_cursor()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let bundle = &prepared.bundle;
    let planner = LysisPlannerV1::new(LysisPlannerBindingsV1 {
        protocol_bundle_hash: job.intent.protocol_bundle_hash,
        job_id: id,
        attempt: job.intent.attempt,
        input_manifest_hash: receipt.manifest_hash(),
        input_manifest_encoded_bytes: manifest_ref.encoded_bytes,
        fidelity_opening_root: manifest.fidelity_opening_root,
        oracle_opening_root: manifest.oracle_opening_root,
        wwd: job.intent.wwd,
        lysis_limit_minor: job.intent.frozen_metadosis_values.lysis_limit_minor,
        logical_evaluation_time: job.intent.logical_evaluation_time,
        tribute_count: manifest.tribute_count,
        lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
        planner_spec_version: bundle.planner_spec_version,
        reducer_spec_version: bundle.reducer_spec_version,
    })
    .unwrap();
    let plan = planner.commit_primary_catalog(refs, &limits).unwrap();
    let plan_ref = cas
        .publish_bytes(&plan.encode_canonical_record(&limits).unwrap())
        .unwrap();
    let admission_root = root
        .join("supervisor-v1/jobs")
        .join(hex::encode(id))
        .join("admissions");
    drop(
        VerifiedAdmissionCatalog::open(&admission_root, &cas, &plan_ref, &manifest_ref, limits)
            .unwrap(),
    );
    (receipt.manifest_hash(), plan.plan_hash(&limits).unwrap())
}

#[test]
fn present_local_result_matches_surviving_manifest_and_plan_without_requiring_retired_plan() {
    use crate::snapshot::tests::ocomp::pin_authority::local_result::{
        refresh_arithmetic, result_for, write_result,
    };
    for version in [1, 2] {
        for damage in ["none", "manifest", "plan", "plan_absent"] {
            let identity = std::cell::Cell::new(None);
            crate::snapshot::tests::ocomp::with_canonical_frontiers(
                version,
                |layout| {
                    identity.set(Some((
                        layout.chain.chain().id(),
                        layout.chain.genesis_hash(),
                    )));
                    write_source(layout);
                    let request = install_active_result_request_frame(layout);
                    let prepared = fixture_for_identity(
                        &request,
                        Phase::VotingOpen,
                        layout.chain.chain().id(),
                        layout.chain.genesis_hash(),
                        bind_source,
                    );
                    let export = write_export(&layout.ocomp_root, &prepared);
                    write_exported_pin(
                        &layout.consensus_root.join("ocomp_retention"),
                        &request,
                        &prepared.job,
                        export,
                    );
                    let (manifest_hash, plan_hash) =
                        write_empty_native_plan(&layout.ocomp_root, &prepared);
                    let mut result = result_for(&prepared.job);
                    result.input_manifest_hash = manifest_hash;
                    result.plan_hash = plan_hash;
                    match damage {
                        "manifest" => result.input_manifest_hash = B256::repeat_byte(0xf1),
                        "plan" | "plan_absent" => result.plan_hash = B256::repeat_byte(0xf2),
                        _ => {}
                    }
                    refresh_arithmetic(&mut result);
                    write_result(&layout.ocomp_root, &result);
                    if damage == "plan_absent" {
                        fs::remove_dir_all(
                            layout
                                .ocomp_root
                                .join("supervisor-v1/jobs")
                                .join(hex::encode(result.job_id)),
                        )
                        .unwrap();
                    }
                },
                |request| {
                    let (chain_id, genesis_hash) = identity.get().unwrap();
                    fixture_for_identity(
                        request,
                        Phase::VotingOpen,
                        chain_id,
                        genesis_hash,
                        bind_source,
                    )
                    .owner
                },
                |state, source, layout, scratch| {
                    let mut report = ValidationReport::new([CheckName::Ocomp]);
                    let result =
                        verify_ocomp_relations(state, source, layout, scratch, &mut report);
                    if matches!(damage, "none" | "plan_absent") {
                        result.unwrap();
                    } else {
                        let error = result
                            .expect_err("surviving result/manifest/plan disagreement cannot pass");
                        assert!(
                            error.downcast_ref::<Incomplete>().is_none(),
                            "{damage}: {error:#}"
                        );
                        assert!(
                            format!("{error:#}").contains("surviving local result"),
                            "{damage}: {error:#}"
                        );
                    }
                },
            );
        }
    }
}

#[test]
fn retained_discovery_ack_must_match_surviving_export_but_retired_records_stay_optional() {
    use outbe_ocomp::{
        discovery_spool::{DiscoverySpoolReaderV1, DiscoverySpoolRecordV1, DiscoverySpoolV1},
        export_receipt::ExportReceiptReader,
    };
    for version in [1, 2] {
        for damage in [
            "none",
            "lease",
            "manifest",
            "record",
            "receipt_digest",
            "retired",
        ] {
            let identity = std::cell::Cell::new(None);
            crate::snapshot::tests::ocomp::with_canonical_frontiers(
                version,
                |layout| {
                    identity.set(Some((
                        layout.chain.chain().id(),
                        layout.chain.genesis_hash(),
                    )));
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
                    let prepared = fixture_for_identity(
                        &request,
                        Phase::VotingOpen,
                        layout.chain.chain().id(),
                        layout.chain.genesis_hash(),
                        bind_source,
                    );
                    let export = write_export(&layout.ocomp_root, &prepared);
                    write_exported_pin(
                        &layout.consensus_root.join("ocomp_retention"),
                        &request,
                        &prepared.job,
                        export,
                    );
                    let job = &prepared.job;
                    let finalized = job.finalized.as_ref().unwrap();
                    let limits = poc_schema_limits();
                    let spec = FinalizedJobSpecV1 {
                        summary: FinalizedJobSummaryV1 {
                            cursor: job.intent_height,
                            job_id: finalized.job_id,
                            intent_id: job.intent.intent_id(&limits).unwrap(),
                            finalized_block_hash: finalized.finalized_request_block_hash,
                            finalized_state_root: finalized.finalized_request_state_root,
                            protocol_bundle_hash: job.intent.protocol_bundle_hash,
                            open_height: finalized.open_height,
                            deadline_height: finalized.deadline_height,
                        },
                        canonical_job_intent: BoundedBytes(
                            job.intent.encode_canonical(&limits).unwrap(),
                        ),
                    };
                    let spool_root = layout
                        .ocomp_root
                        .join("exporter-v1/discovery")
                        .join(hex::encode(job.intent.protocol_bundle_hash));
                    let spool = DiscoverySpoolV1::open(
                        &spool_root,
                        job.intent.chain_id,
                        job.intent.genesis_hash,
                        limits,
                    )
                    .unwrap();
                    let (offer, _) = spool.put_offer(export.source_generation, &spec).unwrap();
                    let cas =
                        FilesystemCasReader::open(layout.ocomp_root.join("cas-v1"), CAS_LIMITS)
                            .unwrap();
                    let receipt = ExportReceiptReader::open(
                        layout.ocomp_root.join("exporter-v1/receipts"),
                        finalized.job_id,
                        limits,
                    )
                    .unwrap()
                    .load_exact(&cas)
                    .unwrap();
                    spool.put_ack(&offer, &receipt, &prepared.bundle).unwrap();
                    if damage == "retired" {
                        spool.prepare_retirement(&offer, 100).unwrap();
                        assert_eq!(
                            spool.complete_retirements_through(100).unwrap().completed,
                            1
                        );
                        assert!(spool.ack(&offer.observation_id).unwrap().is_none());
                    } else if damage != "none" {
                        let mut ack = spool.ack(&offer.observation_id).unwrap().unwrap();
                        match damage {
                            "lease" => ack.lease_generation += 1,
                            "manifest" => ack.manifest_hash = B256::repeat_byte(0xe1),
                            "record" => ack.committed.record_hash = B256::repeat_byte(0xe2),
                            "receipt_digest" => {
                                ack.reference.export_receipt_digest = B256::repeat_byte(0xe3)
                            }
                            _ => unreachable!(),
                        }
                        // Exact native ACK envelope in test setup only. Recompute its
                        // checksum so the existing native decoder accepts the fixture.
                        // The failure must come from the missing cross-record relation.
                        let canonical = ack.committed.encode_body(&limits).unwrap();
                        let mut bytes = b"OUTBDSA2".to_vec();
                        bytes.extend_from_slice(&ack.reference.encode_fixed());
                        bytes.extend_from_slice(&ack.lease_generation.to_be_bytes());
                        bytes.extend_from_slice(ack.manifest_hash.as_slice());
                        bytes.extend_from_slice(
                            &u64::try_from(canonical.len()).unwrap().to_be_bytes(),
                        );
                        bytes.extend_from_slice(&canonical);
                        bytes.extend_from_slice(keccak256(&bytes).as_slice());
                        fs::write(
                            spool_root
                                .join("acks")
                                .join(format!("{}.ack", hex::encode(offer.observation_id))),
                            bytes,
                        )
                        .unwrap();
                    }
                    drop(spool);
                    let reader = DiscoverySpoolReaderV1::open_existing(
                        &spool_root,
                        job.intent.chain_id,
                        job.intent.genesis_hash,
                        limits,
                    )
                    .unwrap();
                    let mut acknowledgements = 0;
                    reader
                        .visit_records(&mut |record| {
                            if matches!(record, DiscoverySpoolRecordV1::Ack(_)) {
                                acknowledgements += 1;
                            }
                            Ok(())
                        })
                        .unwrap();
                    assert_eq!(acknowledgements, usize::from(damage != "retired"));
                },
                |request| {
                    let (chain_id, genesis_hash) = identity.get().unwrap();
                    fixture_for_identity(
                        request,
                        Phase::VotingOpen,
                        chain_id,
                        genesis_hash,
                        bind_source,
                    )
                    .owner
                },
                |state, source, layout, scratch| {
                    let mut report = ValidationReport::new([CheckName::Ocomp]);
                    let result =
                        verify_ocomp_relations(state, source, layout, scratch, &mut report);
                    if matches!(damage, "none" | "retired") {
                        result.unwrap();
                    } else {
                        let error = result
                            .expect_err("native-valid ACK disagreement cannot pass the final join");
                        assert!(
                            error.downcast_ref::<Incomplete>().is_none(),
                            "{damage}: {error:#}"
                        );
                        assert!(format!("{error:#}").contains("ACK"), "{damage}: {error:#}");
                    }
                },
            );
        }
    }
}

use super::*;
use crate::snapshot::validation::{
    ocomp::verify_ocomp_relations,
    report::{CheckName, ValidationReport},
};
use reth_ethereum::provider::db::{
    database::Database, init_db, mdbx::DatabaseArguments, tables, transaction::DbTx,
};

#[test]
fn active_complete_receipt_requires_export_closure_without_retention_pin() {
    use crate::snapshot::tests::headers::fingerprint;
    use outbe_ocomp::{discovery_spool::DiscoverySpoolV1, export_receipt::ExportReceiptReader};
    for version in [1, 2] {
        for damage in [
            "both",
            "catalog",
            "binding",
            "ack_only",
            "none",
            "unexported",
        ] {
            let identity = std::cell::Cell::new(None);
            crate::snapshot::tests::ocomp::with_canonical_frontiers(
                version,
                |layout| {
                    identity.set(Some((
                        layout.chain.chain().id(),
                        layout.chain.genesis_hash(),
                    )));
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
                    let prepared = fixture_for_identity(
                        &request,
                        Phase::VotingOpen,
                        layout.chain.chain().id(),
                        layout.chain.genesis_hash(),
                        bind_source,
                    );
                    if damage == "unexported" {
                        return;
                    }
                    let export = write_export(&layout.ocomp_root, &prepared);
                    write_exported_pin(
                        &layout.consensus_root.join("ocomp_retention"),
                        &request,
                        &prepared.job,
                        export,
                    );
                    crate::snapshot::validation::ocomp::verify_export_inputs(
                        &layout.ocomp_root,
                        &prepared.job,
                        Some(export),
                        CAS_LIMITS,
                    )
                    .unwrap();
                    fs::remove_dir_all(layout.consensus_root.join("ocomp_retention")).unwrap();
                    let job = &prepared.job;
                    let finalized = job.finalized.as_ref().unwrap();
                    let name = hex::encode(finalized.job_id);
                    if damage == "ack_only" {
                        let limits = poc_schema_limits();
                        let spec = FinalizedJobSpecV1 {
                            summary: FinalizedJobSummaryV1 {
                                cursor: job.intent_height,
                                job_id: finalized.job_id,
                                intent_id: job.intent.intent_id(&limits).unwrap(),
                                finalized_block_hash: finalized.finalized_request_block_hash,
                                finalized_state_root: finalized.finalized_request_state_root,
                                protocol_bundle_hash: job.intent.protocol_bundle_hash,
                                open_height: finalized.open_height,
                                deadline_height: finalized.deadline_height,
                            },
                            canonical_job_intent: BoundedBytes(
                                job.intent.encode_canonical(&limits).unwrap(),
                            ),
                        };
                        let spool = DiscoverySpoolV1::open(
                            layout
                                .ocomp_root
                                .join("exporter-v1/discovery")
                                .join(hex::encode(job.intent.protocol_bundle_hash)),
                            job.intent.chain_id,
                            job.intent.genesis_hash,
                            limits,
                        )
                        .unwrap();
                        let (offer, _) = spool.put_offer(export.source_generation, &spec).unwrap();
                        let cas =
                            FilesystemCasReader::open(layout.ocomp_root.join("cas-v1"), CAS_LIMITS)
                                .unwrap();
                        let receipt = ExportReceiptReader::open(
                            layout.ocomp_root.join("exporter-v1/receipts"),
                            finalized.job_id,
                            limits,
                        )
                        .unwrap()
                        .load_exact(&cas)
                        .unwrap();
                        spool.put_ack(&offer, &receipt, &prepared.bundle).unwrap();
                        drop(spool);
                    }
                    if matches!(damage, "catalog" | "both" | "ack_only") {
                        fs::remove_dir_all(
                            layout.ocomp_root.join("exporter-v1/input-refs").join(&name),
                        )
                        .unwrap();
                    }
                    if matches!(damage, "binding" | "both" | "ack_only") {
                        fs::remove_dir_all(
                            layout
                                .ocomp_root
                                .join("supervisor-v1/export-bindings")
                                .join(&name),
                        )
                        .unwrap();
                    }
                    if damage == "ack_only" {
                        fs::remove_dir_all(
                            layout.ocomp_root.join("exporter-v1/receipts").join(&name),
                        )
                        .unwrap();
                    }
                },
                |request| {
                    let (chain_id, genesis_hash) = identity.get().unwrap();
                    fixture_for_identity(
                        request,
                        Phase::VotingOpen,
                        chain_id,
                        genesis_hash,
                        bind_source,
                    )
                    .owner
                },
                |state, source, layout, scratch| {
                    assert!(!layout.consensus_root.join("ocomp_retention").exists());
                    assert!(!layout.ocomp_root.join("supervisor-v1/jobs").exists());
                    let before = fingerprint(&layout.ocomp_root);
                    let mut report = ValidationReport::new([CheckName::Ocomp]);
                    let result =
                        verify_ocomp_relations(state, source, layout, scratch, &mut report);
                    assert_eq!(fingerprint(&layout.ocomp_root), before);
                    if matches!(damage, "none" | "unexported") {
                        result.unwrap();
                        assert_eq!(report.active_ocomp.len(), 1);
                        assert_eq!(report.active_ocomp[0].export_verified, damage == "none");
                    } else {
                        let error = result.expect_err("active recorded export cannot lose required producer inputs with its pin");
                        assert!(
                            error.downcast_ref::<Incomplete>().is_some(),
                            "{damage}: {error:#}"
                        );
                        assert!(
                            format!("{error:#}").contains("export"),
                            "{damage}: {error:#}"
                        );
                    }
                },
            );
        }
    }
}

#[test]
fn final_join_accepts_real_exported_active_job_and_detects_deleted_required_catalog() {
    for deleted in [false, true] {
        crate::snapshot::tests::ocomp::with_canonical_frontiers(
            2,
            |layout| {
                write_source(layout);
                let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
                let tx = db.tx().unwrap();
                let request = tx
                    .get::<tables::Headers<OutbeHeader>>(100)
                    .unwrap()
                    .unwrap();
                drop(tx);
                drop(db);
                let prepared = fixture(&request, Phase::VotingOpen, bind_source);
                let export = write_export(&layout.ocomp_root, &prepared);
                write_exported_pin(
                    &layout.consensus_root.join("ocomp_retention"),
                    &request,
                    &prepared.job,
                    export,
                );
                if deleted {
                    fs::remove_dir_all(
                        layout
                            .ocomp_root
                            .join("exporter-v1/input-refs")
                            .join(hex::encode(prepared.job.finalized.as_ref().unwrap().job_id)),
                    )
                    .unwrap();
                }
            },
            |request| fixture(request, Phase::VotingOpen, bind_source).owner,
            |state, source, layout, scratch| {
                let mut report = ValidationReport::new([CheckName::Ocomp]);
                let result = verify_ocomp_relations(state, source, layout, scratch, &mut report);
                if deleted {
                    assert!(result.unwrap_err().downcast_ref::<Incomplete>().is_some());
                } else {
                    result.unwrap();
                    assert_eq!(report.active_ocomp.len(), 1);
                    assert!(report.active_ocomp[0].export_verified);
                    assert!(report
                        .inventory_bounds
                        .iter()
                        .any(|bound| bound.name == "present_receipts" && bound.visited == 1));
                }
            },
        );
    }
}
