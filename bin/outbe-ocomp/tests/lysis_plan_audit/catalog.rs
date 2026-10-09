use super::*;
use outbe_ocomp_protocol::list::OrderedListProofTarget;

#[test]
fn cold_restart_rejects_a_self_consistent_artifact_for_the_wrong_plan_spec() {
    let fixture = synthetic_fixture(true, false);
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
fn scheduler_prepares_manifest_bound_two_shard_worker_requests_after_cold_restart() {
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
    assert_eq!(audit.plan().tribute_count, 257);
    assert_eq!(audit.plan().primary_work_unit_count, 2);

    for (ordinal, expected_records) in [(0_u32, 256_usize), (1, 1)] {
        let request = audit.worker_request_at(ordinal).unwrap();
        let spec =
            UnitSpecV1::decode_canonical(&request.canonical_unit_spec.0, &fixture.limits).unwrap();
        assert_eq!(spec.phase, UnitPhase::Enumerate);
        verify_ordered_list_membership(
            OrderedListProofTarget::new(ListKind::UnitSpecificationsArtifacts, 2, ordinal),
            &request.canonical_unit_spec.0,
            &request.unit_membership_siblings,
            audit.plan().primary_work_unit_root,
        )
        .unwrap();
        assert_eq!(request.ordered_input_refs.len(), 1);
        let chunk = AuthenticatedInputChunkV1::decode_canonical(
            reader
                .read_verified(&request.ordered_input_refs[0])
                .unwrap()
                .bytes(),
            &fixture.limits,
        )
        .unwrap();
        assert_eq!(chunk.kind, InputChunkKind::Tribute);
        assert_eq!(chunk.canonical_records_or_openings.len(), expected_records);
    }

    let topology = LysisPlanTopologyV1::new(2).unwrap();
    let fidelity_request = audit
        .worker_request_at(topology.phase_offset(UnitPhase::FidelityMap).unwrap())
        .unwrap();
    assert!(fidelity_request.unit_membership_siblings.is_empty());
    assert_eq!(
        fidelity_request.ordered_input_refs[0].expected_ocb1_kind,
        Some(ObjectKind::UnitArtifactV1.tag())
    );
    assert!(fidelity_request.ordered_input_refs[1..]
        .iter()
        .all(|reference| reference.expected_ocb1_kind
            == Some(ObjectKind::AuthenticatedInputChunkV1.tag())));
    assert!(
        fidelity_request.ordered_input_refs.len() >= 3,
        "FidelityMap receives its producer, shard, bounded lookahead and Fidelity openings"
    );
    let fidelity_authenticated = fidelity_request.ordered_input_refs[1..]
        .iter()
        .map(|reference| {
            AuthenticatedInputChunkV1::decode_canonical(
                reader.read_verified(reference).unwrap().bytes(),
                &fixture.limits,
            )
            .unwrap()
            .kind
        })
        .collect::<Vec<_>>();
    assert_eq!(
        &fidelity_authenticated[..2],
        &[InputChunkKind::Tribute, InputChunkKind::Tribute]
    );

    let amount_request = audit
        .worker_request_at(topology.phase_offset(UnitPhase::AmountMap).unwrap())
        .unwrap();
    assert_eq!(
        amount_request.ordered_input_refs[..3]
            .iter()
            .filter(|reference| {
                reference.expected_ocb1_kind == Some(ObjectKind::UnitArtifactV1.tag())
            })
            .count(),
        3
    );
    let authenticated = amount_request.ordered_input_refs[3..]
        .iter()
        .map(|reference| {
            AuthenticatedInputChunkV1::decode_canonical(
                reader.read_verified(reference).unwrap().bytes(),
                &fixture.limits,
            )
            .unwrap()
            .kind
        })
        .collect::<Vec<_>>();
    assert_eq!(
        authenticated,
        vec![
            InputChunkKind::Tribute,
            InputChunkKind::Tribute,
            InputChunkKind::Oracle
        ]
    );
}

#[test]
fn fidelity_membership_search_reports_bounded_progress_before_the_next_input_chunk() {
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
    let fixture = synthetic_fixture(false, true);
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
    let fixture = synthetic_fixture(false, false);
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
    let audit = outbe_ocomp::lysis_plan_audit::open_local_plan_audit(
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
