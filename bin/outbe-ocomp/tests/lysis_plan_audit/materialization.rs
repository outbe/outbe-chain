use super::*;

#[test]
fn one_chunk_nod_generation_builds_exact_first_adaptive_and_final_batch_paths() {
    let fixture = synthetic_fixture_with_tribute_count(10);
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
    let audit = outbe_ocomp::lysis_plan_audit::open_local_plan_audit(
        &admissions,
        &input_refs,
        &reader,
        &fixture.bundle,
        &fixture.limits,
    )
    .unwrap();

    let mut root = StreamingOrderedListRoot::new(ListKind::NodActions, 10).unwrap();
    for step in ExactLysisResultCatalogCursorV1::open(&audit).unwrap() {
        if let LysisResultCatalogStepV1::Chunk(chunk) = step.unwrap() {
            for action in &chunk.chunk().ordered_nod_actions {
                root.push(
                    &action.encode_canonical_record(&fixture.limits).unwrap(),
                    fixture.limits.max_bounded_bytes,
                )
                .unwrap();
            }
        }
    }
    let mut head = NodMaterializationHeadV1 {
        queue_sequence: 9,
        job_id: audit.plan().job_id,
        program_semantics_hash: fixture.bundle.bundle().lysis_program_semantics_hash,
        worldwide_day: audit.plan().wwd,
        generation: 1,
        nod_root: root.finish().unwrap(),
        nod_count: 10,
        next_nod_ordinal: 0,
        last_progress_height: 100,
    };

    assert_invalid_materialization_heads_are_rejected(&audit, &head);

    let first = build_nod_materialization_batch(&audit, &head, 3).unwrap();
    assert_eq!(first.actions.len(), 8);
    assert_eq!(first.root_path.len(), 1);
    outbe_ocomp_protocol::nod_materialization::verify_nod_materialization_batch(
        &first,
        &head,
        3,
        &fixture.limits,
    )
    .expect("first compact batch verifies");

    for (cursor, count, path_length) in [(1, 1, 4), (2, 2, 3), (4, 4, 2), (6, 2, 3)] {
        head.next_nod_ordinal = cursor;
        let aligned = build_nod_materialization_batch(&audit, &head, 3).unwrap();
        assert_eq!(aligned.actions.len(), count);
        assert_eq!(aligned.root_path.len(), path_length);
        outbe_ocomp_protocol::nod_materialization::verify_nod_materialization_batch(
            &aligned,
            &head,
            3,
            &fixture.limits,
        )
        .expect("a cursor after a smaller batch has an aligned certified subtree");
    }

    head.next_nod_ordinal = 8;
    let final_batch = build_nod_materialization_batch(&audit, &head, 3).unwrap();
    assert_eq!(final_batch.actions.len(), 2);
    assert_eq!(final_batch.root_path.len(), 1);
    outbe_ocomp_protocol::nod_materialization::verify_nod_materialization_batch(
        &final_batch,
        &head,
        3,
        &fixture.limits,
    )
    .expect("padded final compact batch verifies");
}

#[test]
fn cold_reloaded_result_catalog_builds_a_verified_nod_proof_across_shards() {
    let fixture = synthetic_fixture(false, false);
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
    let audit = outbe_ocomp::lysis_plan_audit::open_local_plan_audit(
        &admissions,
        &input_refs,
        &reader,
        &fixture.bundle,
        &fixture.limits,
    )
    .unwrap();

    let mut root = StreamingOrderedListRoot::new(ListKind::NodActions, 257).unwrap();
    for step in ExactLysisResultCatalogCursorV1::open(&audit).unwrap() {
        if let LysisResultCatalogStepV1::Chunk(chunk) = step.unwrap() {
            for action in &chunk.chunk().ordered_nod_actions {
                root.push(
                    &action.encode_canonical_record(&fixture.limits).unwrap(),
                    fixture.limits.max_bounded_bytes,
                )
                .unwrap();
            }
        }
    }
    let authority = ActiveNodSetV1 {
        job_id: audit.plan().job_id,
        program_semantics_hash: fixture.bundle.bundle().lysis_program_semantics_hash,
        worldwide_day: audit.plan().wwd,
        generation: 1,
        nod_root: root.finish().unwrap(),
        nod_count: 257,
    };

    let proof = build_certified_nod_proof(&audit, &authority, 256).unwrap();

    let action = proof.verify_against(&authority, &fixture.limits).unwrap();
    assert_eq!(action.raw_ordinal, 256);
}

#[test]
fn addressable_chunk_and_batch_proof_do_not_scan_preceding_chunks() {
    let fixture = synthetic_fixture(false, false);
    let authority = fixture.active_nod_set();

    let first_digest = hex::encode(fixture.result_chunk_refs[0].transport_digest);
    std::fs::remove_file(
        fixture
            .cas_root
            .join("objects")
            .join(&first_digest[..2])
            .join(&first_digest[2..]),
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
    let audit = outbe_ocomp::lysis_plan_audit::open_local_plan_audit(
        &admissions,
        &input_refs,
        &reader,
        &fixture.bundle,
        &fixture.limits,
    )
    .unwrap();

    assert_eq!(
        verified_result_chunk_at(&audit, 1)
            .unwrap()
            .chunk()
            .chunk_ordinal,
        1
    );
    let head = NodMaterializationHeadV1 {
        queue_sequence: 9,
        job_id: authority.job_id,
        program_semantics_hash: authority.program_semantics_hash,
        worldwide_day: authority.worldwide_day,
        generation: authority.generation,
        nod_root: authority.nod_root,
        nod_count: authority.nod_count,
        next_nod_ordinal: 256,
        last_progress_height: 100,
    };
    let batch = build_nod_materialization_batch(&audit, &head, 3).unwrap();
    assert_eq!(batch.first_nod_ordinal, 256);
    assert_eq!(batch.actions.len(), 1);
    outbe_ocomp_protocol::nod_materialization::verify_nod_materialization_batch(
        &batch,
        &head,
        3,
        &fixture.limits,
    )
    .expect("final partial batch with canonical empty upper siblings");
}

#[test]
fn missing_result_chunk_makes_the_nod_read_unavailable() {
    let fixture = synthetic_fixture(false, false);
    let authority = fixture.active_nod_set();

    let digest_hex = hex::encode(fixture.result_chunk_refs[1].transport_digest);
    let missing_path = fixture
        .cas_root
        .join("objects")
        .join(&digest_hex[..2])
        .join(&digest_hex[2..]);
    std::fs::remove_file(missing_path).unwrap();

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
    let audit = outbe_ocomp::lysis_plan_audit::open_local_plan_audit(
        &admissions,
        &input_refs,
        &reader,
        &fixture.bundle,
        &fixture.limits,
    )
    .unwrap();

    assert!(matches!(
        build_certified_nod_proof(&audit, &authority, 256),
        Err(NodProofBuildError::Catalog(_))
    ));
}
