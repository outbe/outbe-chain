use super::*;

#[test]
fn cold_restart_streams_exact_result_chunks_only_from_root_reduce_leaves() {
    let fixture = synthetic_fixture(false, false);

    let first = fixture.result_catalog_steps();
    assert_eq!(
        first.first(),
        Some(&LysisResultCatalogStepV1::RootReduceLeafChecked { chunk_ordinal: 0 })
    );
    assert_eq!(first.last(), Some(&LysisResultCatalogStepV1::Complete));
    let first_chunks = first
        .iter()
        .filter_map(|step| match step {
            LysisResultCatalogStepV1::Chunk(item) => Some(item),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(first_chunks.len(), 2);
    assert_eq!(
        first_chunks
            .iter()
            .map(|item| item.chunk().ordered_eligible_contributors.len())
            .sum::<usize>(),
        257
    );
    let mut locally_repartitioned = false;
    for (ordinal, item) in first_chunks.iter().enumerate() {
        let ordinal = u32::try_from(ordinal).unwrap();
        assert_eq!(item.output_manifest_entry().chunk_ordinal, ordinal);
        assert_eq!(item.chunk().chunk_ordinal, ordinal);
        assert_eq!(item.summary().covered_primary_start, ordinal);
        assert_eq!(
            ResultChunkV1::decode_canonical(item.canonical_chunk_bytes(), &fixture.limits).unwrap(),
            *item.chunk()
        );
        let chunk_eligible = item
            .chunk()
            .ordered_eligible_contributors
            .iter()
            .fold(U256::ZERO, |total, contributor| {
                total.checked_add(contributor.nominal_amount_minor).unwrap()
            });
        locally_repartitioned |= chunk_eligible != item.summary().eligible_nominal_total;
    }
    assert!(locally_repartitioned);

    let restarted = fixture.result_catalog_steps();
    assert_eq!(restarted, first);
}

#[test]
fn result_catalog_latches_failure_when_a_chunk_changes_after_admission() {
    let fixture = synthetic_fixture(false, false);
    let reference = fixture.result_chunk_refs.first().unwrap();
    let encoded = format!("{:x}", reference.transport_digest);
    let digest = encoded.strip_prefix("0x").unwrap_or(&encoded);
    let path = fixture
        .cas_root
        .join("objects")
        .join(&digest[..2])
        .join(&digest[2..]);
    let mut changed = std::fs::read(&path).unwrap();
    *changed.last_mut().unwrap() ^= 1;
    std::fs::write(&path, changed).unwrap();

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
    let mut cursor = ExactLysisResultCatalogCursorV1::open(&audit).unwrap();
    assert_eq!(
        cursor.next().unwrap().unwrap(),
        LysisResultCatalogStepV1::RootReduceLeafChecked { chunk_ordinal: 0 }
    );
    assert!(matches!(
        cursor.next().unwrap(),
        Err(LysisResultCatalogError::Cas(_))
    ));
    assert!(cursor.next().is_none());
}

#[test]
fn result_catalog_rederives_leaf_carriers_from_exact_chunk_bytes() {
    let fixture =
        synthetic_fixture_with_result_fault(false, false, ResultCatalogFault::ContributorCarrier);
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
    let mut cursor = ExactLysisResultCatalogCursorV1::open(&audit).unwrap();
    assert!(matches!(
        cursor.next().unwrap().unwrap(),
        LysisResultCatalogStepV1::RootReduceLeafChecked { chunk_ordinal: 0 }
    ));
    assert!(matches!(
        cursor.next().unwrap().unwrap(),
        LysisResultCatalogStepV1::Chunk(_)
    ));
    assert!(matches!(
        cursor.next().unwrap().unwrap(),
        LysisResultCatalogStepV1::RootReduceLeafChecked { chunk_ordinal: 1 }
    ));
    assert!(matches!(
        cursor.next().unwrap(),
        Err(LysisResultCatalogError::Protocol(
            outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
                "result chunk ROOT_REDUCE carrier binding"
            )
        ))
    ));
    assert!(cursor.next().is_none());
}

#[test]
fn result_catalog_compares_repartitioned_eligible_totals_only_after_all_chunks() {
    let error = result_catalog_error_for_fault(ResultCatalogFault::EligibleTotal);
    assert!(matches!(
        error,
        LysisResultCatalogError::Protocol(outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
            "global eligible nominal total"
        ))
    ));
}

#[test]
fn result_catalog_binds_root_reduce_output_header_to_the_leaf_summary() {
    let error = result_catalog_error_for_fault(ResultCatalogFault::HeaderCoverage);
    assert!(matches!(
        error,
        LysisResultCatalogError::Protocol(outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
            "ROOT_REDUCE leaf output header"
        ))
    ));
}

#[test]
fn result_catalog_rejects_contributor_reordering_across_chunk_boundaries() {
    let error = result_catalog_error_for_fault(ResultCatalogFault::ContributorBoundary);
    assert!(matches!(
        error,
        LysisResultCatalogError::Protocol(outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
            "global result contributor order"
        ))
    ));
}

#[test]
fn result_catalog_rejects_nod_tribute_reordering_across_chunk_boundaries() {
    let error = result_catalog_error_for_fault(ResultCatalogFault::NodBoundary);
    assert!(matches!(
        error,
        LysisResultCatalogError::Protocol(outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
            "global result Nod TributeId order"
        ))
    ));
}

#[test]
fn result_catalog_decodes_every_admission_before_directory_eof() {
    let fixture = synthetic_fixture(false, false);
    let ordinal = fixture.expected_count - 1;
    let path = fixture
        .admission_root
        .join(format!("{ordinal:010}.admission"));
    let mut changed = std::fs::read(&path).unwrap();
    *changed.last_mut().unwrap() ^= 1;
    std::fs::write(&path, changed).unwrap();

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
    let mut chunks = 0_u32;
    let mut cursor = ExactLysisResultCatalogCursorV1::open(&audit).unwrap();
    let error = loop {
        match cursor.next() {
            Some(Ok(LysisResultCatalogStepV1::Chunk(_))) => chunks += 1,
            Some(Ok(LysisResultCatalogStepV1::Complete)) => {
                panic!("corrupt non-leaf admission must prevent closure")
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("corrupt non-leaf admission must fail"),
        }
    };
    assert_eq!(chunks, 2);
    assert!(matches!(error, LysisResultCatalogError::Plan(_)));
    assert!(cursor.next().is_none());
}

#[test]
fn result_catalog_rejects_valid_same_ordinal_entry_substitution_between_passes() {
    let fixture = synthetic_fixture(false, false);
    let alternate =
        synthetic_fixture_with_result_fault(false, false, ResultCatalogFault::AlternateResultChunk);
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
    let mut cursor = ExactLysisResultCatalogCursorV1::open(&audit).unwrap();
    let mut first_leaf_plan_ordinal = None;
    let mut chunk_count = 0_u32;
    while chunk_count < 2 {
        match cursor.next().unwrap().unwrap() {
            LysisResultCatalogStepV1::Chunk(item) => {
                first_leaf_plan_ordinal.get_or_insert(item.plan_ordinal());
                chunk_count += 1;
            }
            LysisResultCatalogStepV1::Complete => {
                panic!("admission replay must follow exact chunks")
            }
            _ => {}
        }
    }

    let ordinal = first_leaf_plan_ordinal.unwrap();
    let name = format!("{ordinal:010}.admission");
    std::fs::copy(
        alternate.admission_root.join(&name),
        fixture.admission_root.join(&name),
    )
    .unwrap();
    let error = loop {
        match cursor.next() {
            Some(Ok(LysisResultCatalogStepV1::Complete)) => {
                panic!("same-ordinal entry substitution must prevent closure")
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("same-ordinal entry substitution must fail"),
        }
    };
    assert!(matches!(
        error,
        LysisResultCatalogError::Protocol(outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
            "reloaded result entry content closure"
        ))
    ));
    assert!(cursor.next().is_none());
}

#[test]
fn result_catalog_requires_admission_directory_eof_after_the_last_chunk() {
    let fixture = synthetic_fixture(false, false);
    std::fs::write(
        fixture.admission_root.join("4294967295.admission"),
        b"late extra admission",
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
    let mut chunks = 0_u32;
    let mut cursor = ExactLysisResultCatalogCursorV1::open(&audit).unwrap();
    let error = loop {
        match cursor.next() {
            Some(Ok(LysisResultCatalogStepV1::Chunk(_))) => chunks += 1,
            Some(Ok(LysisResultCatalogStepV1::Complete)) => {
                panic!("late admission must prevent result catalog closure")
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("late admission must fail the result catalog cursor"),
        }
    };
    assert_eq!(chunks, 2);
    assert!(
        matches!(
            error,
            LysisResultCatalogError::Admission(
                outbe_ocomp::admission_catalog::AdmissionCatalogError::UnexpectedAdmission { .. }
            )
        ),
        "unexpected EOF error: {error:?}"
    );
    assert!(cursor.next().is_none());
}
