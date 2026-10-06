// OCOMP-TEST-ID: OCM-APL-001
mod support;

use alloy_primitives::U256;
use outbe_lysis::activation_v1::{
    verify_receipts, verify_result, LysisApplyPlanV1, LysisOwnerReceiptsV1, LysisResultInputsV1,
};
use outbe_ocomp_protocol::{
    intent::DayType,
    receipts::{
        carry_over_state_event_digest, contributor_state_event_digest, nod_state_event_digest,
        tribute_state_event_digest, CarryOverReceiptV1, CarryOverStateEventProjectionV1,
        ContributorReceiptV1, ContributorStateEventProjectionV1, NodBatchReceiptV1,
        NodStateEventProjectionV1, TributeReceiptV1, TributeStateEventProjectionV1,
    },
};

use support::{activation_fixture, hash, recommit_result};

#[test]
fn structural_verifier_produces_one_closed_four_owner_plan() {
    for day_type in [DayType::Green, DayType::Red] {
        let fixture = activation_fixture(day_type);
        let plan = verify_result(inputs(&fixture, &fixture.payload, &fixture.result)).unwrap();

        assert_eq!(plan.binding().job_id, fixture.job_id);
        assert_eq!(plan.binding().attempt, fixture.intent.attempt);
        assert_eq!(
            plan.request_limit_split_receipt_hash(),
            fixture
                .intent
                .frozen_metadosis_values
                .request_limit_split_receipt_hash
        );
        assert_eq!(
            fixture
                .request_receipt
                .receipt_hash(&fixture.limits)
                .unwrap(),
            plan.request_limit_split_receipt_hash()
        );
        assert_ne!(
            fixture.nod_issued_at,
            fixture.intent.logical_evaluation_time
        );
        assert_eq!(plan.nod().issued_at(), fixture.nod_issued_at);
        assert_eq!(plan.nod().nod_root(), fixture.result.roots.nod_root);
        assert_eq!(plan.nod().bucket_root(), fixture.result.roots.bucket_root);
        assert_eq!(
            plan.nod().output_manifest_root(),
            fixture.result.roots.output_manifest_root
        );
        assert_eq!(
            plan.contributors().contributor_root(),
            fixture.result.roots.contributor_root
        );
        assert_eq!(
            plan.tribute().input_binding(),
            &fixture.intent.activation_preconditions.tribute
        );
        assert_eq!(
            plan.carry_over().credited_unused_lysis_limit_minor(),
            U256::from(15)
        );
    }
}

#[test]
fn structural_verifier_rejects_result_and_completion_rebinding() {
    let fixture = activation_fixture(DayType::Green);

    let mut wrong_job = fixture.result.clone();
    wrong_job.job_id = hash(200);
    assert!(verify_result(inputs(
        &fixture,
        &wrong_job.activation_payload(&fixture.limits).unwrap(),
        &wrong_job
    ))
    .is_err());

    let mut wrong_completion = fixture.result.clone();
    wrong_completion
        .metadosis_completion_summary
        .logical_evaluation_time += 1;
    assert!(verify_result(inputs(
        &fixture,
        &wrong_completion
            .activation_payload(&fixture.limits)
            .unwrap(),
        &wrong_completion
    ))
    .is_err());

    let mut wrong_contributor_total = fixture.result.clone();
    wrong_contributor_total.conservation.eligible_nominal_total = U256::from(1_001);
    recommit_result(&mut wrong_contributor_total, &fixture.limits);
    let payload = wrong_contributor_total
        .activation_payload(&fixture.limits)
        .unwrap();
    assert!(verify_result(inputs(&fixture, &payload, &wrong_contributor_total)).is_err());

    let mut payload_rebinding = fixture.payload.clone();
    payload_rebinding.roots.nod_root = hash(201);
    assert!(verify_result(inputs(&fixture, &payload_rebinding, &fixture.result)).is_err());
}

#[test]
fn structural_verifier_rejects_catalog_completion_and_semantic_event_mutations() {
    let fixture = activation_fixture(DayType::Green);

    let mut catalog_count = fixture.result.clone();
    catalog_count.result_chunk_count = 0;
    assert_result_rejected(&fixture, &catalog_count, &fixture.payload);

    let mut catalog_root = fixture.result.clone();
    catalog_root.result_chunk_list_root = alloy_primitives::B256::ZERO;
    assert_result_rejected(&fixture, &catalog_root, &fixture.payload);

    let mut exact_count = fixture.result.clone();
    exact_count.counts.contributor_count += 1;
    recommit_result(&mut exact_count, &fixture.limits);
    assert_result_rejected(&fixture, &exact_count, &fixture.payload);

    let mut semantic_count = fixture.result.clone();
    semantic_count.counts.semantic_event_count = 1;
    recommit_result(&mut semantic_count, &fixture.limits);
    assert_result_rejected(&fixture, &semantic_count, &fixture.payload);

    let mut semantic_root = fixture.result.clone();
    semantic_root.event_summary_hash = hash(202);
    assert_result_rejected(&fixture, &semantic_root, &fixture.payload);

    let mut completion_mutations = Vec::new();
    let mut mutation = fixture.result.clone();
    mutation.metadosis_completion_summary.wwd += 1;
    completion_mutations.push(mutation);
    let mut mutation = fixture.result.clone();
    mutation.metadosis_completion_summary.pending_nonce += 1;
    completion_mutations.push(mutation);
    let mut mutation = fixture.result.clone();
    mutation.metadosis_completion_summary.day_type = DayType::Red;
    completion_mutations.push(mutation);
    let mut mutation = fixture.result.clone();
    mutation.metadosis_completion_summary.day_limit += U256::from(1);
    completion_mutations.push(mutation);
    let mut mutation = fixture.result.clone();
    mutation.metadosis_completion_summary.gratis_demand += U256::from(1);
    completion_mutations.push(mutation);
    let mut mutation = fixture.result.clone();
    mutation.metadosis_completion_summary.day_gratis_limit_minor += U256::from(1);
    completion_mutations.push(mutation);
    let mut mutation = fixture.result.clone();
    mutation.metadosis_completion_summary.lysis_limit_minor += U256::from(1);
    completion_mutations.push(mutation);
    let mut mutation = fixture.result.clone();
    mutation.metadosis_completion_summary.desis_limit_minor += U256::from(1);
    completion_mutations.push(mutation);
    let mut mutation = fixture.result.clone();
    mutation
        .metadosis_completion_summary
        .logical_evaluation_height += 1;
    completion_mutations.push(mutation);
    let mut mutation = fixture.result.clone();
    mutation
        .metadosis_completion_summary
        .logical_evaluation_time += 1;
    completion_mutations.push(mutation);

    for mutation in completion_mutations {
        assert_result_rejected(&fixture, &mutation, &fixture.payload);
    }
}

#[test]
fn receipt_verifier_closes_green_and_red_conservation_equations() {
    for day_type in [DayType::Green, DayType::Red] {
        let fixture = activation_fixture(day_type);
        let plan = verify_result(inputs(&fixture, &fixture.payload, &fixture.result)).unwrap();
        let receipts = owner_receipts(&plan, &fixture.limits);
        let verified =
            verify_receipts(&plan, &fixture.request_receipt, &receipts, &fixture.limits).unwrap();

        assert_eq!(verified.binding(), plan.binding());
        assert_eq!(
            verified.request_limit_split_receipt_hash(),
            plan.request_limit_split_receipt_hash()
        );
        assert!(!verified.effect_commitment().is_zero());
        assert!(!verified.event_summary_hash().is_zero());
    }
}

#[test]
fn structural_verifier_rejects_nonzero_attempt_or_pending_nonce() {
    for (attempt, pending_nonce) in [(1, 0), (0, 1), (1, 1)] {
        let mut fixture = activation_fixture(DayType::Green);
        fixture.intent.attempt = attempt;
        fixture.intent.pending_nonce = pending_nonce;
        fixture
            .intent
            .activation_preconditions
            .metadosis
            .pending_nonce = pending_nonce;
        let result = verify_result(inputs(&fixture, &fixture.payload, &fixture.result));
        assert!(matches!(
            result,
            Err(outbe_ocomp_protocol::ProtocolError::InvalidInvariant(
                "single-attempt OCOMP identity"
            ))
        ));
    }
}

#[test]
fn receipt_verifier_rejects_a_budget_effect_with_a_future_nonce_or_anchor() {
    let mut nonce_fixture = activation_fixture(DayType::Green);
    nonce_fixture.request_receipt.pending_nonce = nonce_fixture.intent.pending_nonce + 1;
    nonce_fixture
        .intent
        .frozen_metadosis_values
        .request_limit_split_receipt_hash = nonce_fixture
        .request_receipt
        .receipt_hash(&nonce_fixture.limits)
        .unwrap();
    nonce_fixture.intent_id = nonce_fixture
        .intent
        .intent_id(&nonce_fixture.limits)
        .unwrap();
    let nonce_plan = verify_result(inputs(
        &nonce_fixture,
        &nonce_fixture.payload,
        &nonce_fixture.result,
    ))
    .unwrap();
    assert!(verify_receipts(
        &nonce_plan,
        &nonce_fixture.request_receipt,
        &owner_receipts(&nonce_plan, &nonce_fixture.limits),
        &nonce_fixture.limits,
    )
    .is_err());

    let mut anchor_fixture = activation_fixture(DayType::Green);
    anchor_fixture.request_receipt.logical_anchor =
        anchor_fixture.intent.logical_evaluation_time + 1;
    let desis_limit_minor = anchor_fixture.request_receipt.desis_limit_minor;
    anchor_fixture.request_receipt.desis_brief_hash = Some(
        outbe_ocomp_protocol::receipts::desis_request_brief_hash(
            anchor_fixture.request_receipt.protocol_bundle_hash,
            anchor_fixture.request_receipt.wwd,
            desis_limit_minor,
            anchor_fixture.request_receipt.logical_anchor,
        )
        .unwrap(),
    );
    anchor_fixture
        .intent
        .frozen_metadosis_values
        .request_limit_split_receipt_hash = anchor_fixture
        .request_receipt
        .receipt_hash(&anchor_fixture.limits)
        .unwrap();
    anchor_fixture.intent_id = anchor_fixture
        .intent
        .intent_id(&anchor_fixture.limits)
        .unwrap();
    let anchor_plan = verify_result(inputs(
        &anchor_fixture,
        &anchor_fixture.payload,
        &anchor_fixture.result,
    ))
    .unwrap();
    assert!(verify_receipts(
        &anchor_plan,
        &anchor_fixture.request_receipt,
        &owner_receipts(&anchor_plan, &anchor_fixture.limits),
        &anchor_fixture.limits,
    )
    .is_err());
}

#[test]
fn receipt_verifier_rejects_owner_projection_and_request_mutations() {
    let fixture = activation_fixture(DayType::Green);
    let plan = verify_result(inputs(&fixture, &fixture.payload, &fixture.result)).unwrap();
    let receipts = owner_receipts(&plan, &fixture.limits);

    let mut wrong_nod = receipts.clone();
    wrong_nod.nod.nod_root = hash(210);
    assert!(verify_receipts(&plan, &fixture.request_receipt, &wrong_nod, &fixture.limits).is_err());

    let mut wrong_contributor = receipts.clone();
    wrong_contributor.contributor.contributor_count += 1;
    assert!(verify_receipts(
        &plan,
        &fixture.request_receipt,
        &wrong_contributor,
        &fixture.limits
    )
    .is_err());

    let mut wrong_contributor_root = receipts.clone();
    wrong_contributor_root.contributor.contributor_root = hash(213);
    assert!(verify_receipts(
        &plan,
        &fixture.request_receipt,
        &wrong_contributor_root,
        &fixture.limits
    )
    .is_err());

    let mut wrong_contributor_nominal = receipts.clone();
    wrong_contributor_nominal.contributor.eligible_nominal_total += U256::from(1);
    assert!(verify_receipts(
        &plan,
        &fixture.request_receipt,
        &wrong_contributor_nominal,
        &fixture.limits
    )
    .is_err());

    let mut wrong_tribute_generation = receipts.clone();
    wrong_tribute_generation.tribute.retired_generation += 1;
    assert!(verify_receipts(
        &plan,
        &fixture.request_receipt,
        &wrong_tribute_generation,
        &fixture.limits
    )
    .is_err());

    let mut wrong_tribute_root = receipts.clone();
    wrong_tribute_root.tribute.sealed_collection_root = hash(214);
    assert!(verify_receipts(
        &plan,
        &fixture.request_receipt,
        &wrong_tribute_root,
        &fixture.limits
    )
    .is_err());

    let mut wrong_tribute_count = receipts.clone();
    wrong_tribute_count.tribute.consumed_count += 1;
    assert!(verify_receipts(
        &plan,
        &fixture.request_receipt,
        &wrong_tribute_count,
        &fixture.limits
    )
    .is_err());

    let mut wrong_tribute_nominal = receipts.clone();
    wrong_tribute_nominal.tribute.consumed_nominal_total += U256::from(1);
    assert!(verify_receipts(
        &plan,
        &fixture.request_receipt,
        &wrong_tribute_nominal,
        &fixture.limits
    )
    .is_err());

    let mut wrong_carry = receipts.clone();
    wrong_carry.carry_over.credited_unused_lysis_limit_minor += U256::from(1);
    wrong_carry.carry_over.after_value += U256::from(1);
    assert!(verify_receipts(
        &plan,
        &fixture.request_receipt,
        &wrong_carry,
        &fixture.limits
    )
    .is_err());

    let mut wrong_event = receipts.clone();
    wrong_event.nod.state_event_digest = wrong_event.contributor.state_event_digest;
    assert!(verify_receipts(
        &plan,
        &fixture.request_receipt,
        &wrong_event,
        &fixture.limits
    )
    .is_err());

    let mut wrong_binding = receipts.clone();
    wrong_binding.contributor.binding.job_id = hash(212);
    assert!(verify_receipts(
        &plan,
        &fixture.request_receipt,
        &wrong_binding,
        &fixture.limits
    )
    .is_err());

    let mut wrong_request = fixture.request_receipt.clone();
    wrong_request.desis_brief_hash = Some(hash(211));
    assert!(verify_receipts(&plan, &wrong_request, &receipts, &fixture.limits).is_err());
}

fn assert_result_rejected(
    fixture: &support::ActivationFixtureV1,
    result: &outbe_ocomp_protocol::result::LysisResultV1,
    payload: &outbe_ocomp_protocol::result::ActivationPayloadV1,
) {
    assert!(verify_result(inputs(fixture, payload, result)).is_err());
}

fn inputs<'a>(
    fixture: &'a support::ActivationFixtureV1,
    payload: &'a outbe_ocomp_protocol::result::ActivationPayloadV1,
    result: &'a outbe_ocomp_protocol::result::LysisResultV1,
) -> LysisResultInputsV1<'a> {
    LysisResultInputsV1 {
        intent_id: fixture.intent_id,
        expected_job_id: fixture.job_id,
        intent: &fixture.intent,
        activation_payload: payload,
        result,
        limits: &fixture.limits,
        nod_issued_at: fixture.nod_issued_at,
    }
}

fn owner_receipts(
    plan: &LysisApplyPlanV1,
    limits: &outbe_ocomp_protocol::SchemaLimits,
) -> LysisOwnerReceiptsV1 {
    let binding = plan.binding().clone();
    let nod_projection = NodStateEventProjectionV1 {
        wwd: plan.nod().precondition().wwd,
        target_generation: plan.nod().precondition().target_generation,
        namespace_root_before: plan.nod().precondition().namespace_root_before,
        nod_count: plan.nod().exact_counts().nod_count,
        nod_root: plan.nod().nod_root(),
        nod_amount_total: plan.nod().nod_amount_total(),
        lysis_allocation_minor: plan.nod().lysis_allocation_minor(),
        issued_at: plan.nod().issued_at(),
    };
    let contributor_projection = ContributorStateEventProjectionV1 {
        worldwide_day: plan.contributors().precondition().worldwide_day,
        series_version_before: plan.contributors().precondition().expected_series_version,
        series_version_after: plan.contributors().precondition().expected_series_version + 1,
        contributor_count: plan.contributors().contributor_count(),
        contributor_root: plan.contributors().contributor_root(),
        eligible_nominal_total: plan.contributors().eligible_nominal_total(),
    };
    let tribute_projection = TributeStateEventProjectionV1 {
        wwd: plan.tribute().input_binding().wwd,
        source_generation: plan.tribute().input_binding().source_generation,
        sealed_collection_root: plan.tribute().input_binding().sealed_collection_root,
        consumed_count: plan.tribute().consumed_count(),
        consumed_nominal_total: plan.tribute().consumed_nominal_total(),
        retired_generation: plan.tribute().retired_generation(),
    };
    let carry_projection = CarryOverStateEventProjectionV1 {
        source_wwd: plan.carry_over().source_wwd(),
        before_value: U256::from(77),
        credited_unused_lysis_limit_minor: plan.carry_over().credited_unused_lysis_limit_minor(),
        after_value: U256::from(77) + plan.carry_over().credited_unused_lysis_limit_minor(),
    };
    LysisOwnerReceiptsV1 {
        nod: NodBatchReceiptV1 {
            binding: binding.clone(),
            nod_target_precondition: plan.nod().precondition().clone(),
            nod_count: plan.nod().exact_counts().nod_count,
            nod_root: plan.nod().nod_root(),
            nod_amount_total: plan.nod().nod_amount_total(),
            lysis_allocation_minor: plan.nod().lysis_allocation_minor(),
            issued_at: plan.nod().issued_at(),
            state_event_digest: nod_state_event_digest(&binding, &nod_projection, limits).unwrap(),
        },
        contributor: ContributorReceiptV1 {
            binding: binding.clone(),
            contributor_target_precondition: plan.contributors().precondition().clone(),
            contributor_count: plan.contributors().contributor_count(),
            contributor_root: plan.contributors().contributor_root(),
            eligible_nominal_total: plan.contributors().eligible_nominal_total(),
            state_event_digest: contributor_state_event_digest(
                &binding,
                &contributor_projection,
                limits,
            )
            .unwrap(),
        },
        tribute: TributeReceiptV1 {
            binding: binding.clone(),
            tribute_input_binding: plan.tribute().input_binding().clone(),
            sealed_collection_root: plan.tribute().input_binding().sealed_collection_root,
            consumed_count: plan.tribute().consumed_count(),
            consumed_nominal_total: plan.tribute().consumed_nominal_total(),
            retired_generation: plan.tribute().retired_generation(),
            state_event_digest: tribute_state_event_digest(&binding, &tribute_projection, limits)
                .unwrap(),
        },
        carry_over: CarryOverReceiptV1 {
            binding,
            source_wwd: carry_projection.source_wwd,
            before_value: carry_projection.before_value,
            credited_unused_lysis_limit_minor: carry_projection.credited_unused_lysis_limit_minor,
            after_value: carry_projection.after_value,
            state_event_digest: carry_over_state_event_digest(
                plan.binding(),
                &carry_projection,
                limits,
            )
            .unwrap(),
        },
    }
}

#[test]
fn nod_issuance_instant_rejects_zero() {
    let fixture = activation_fixture(DayType::Green);
    let error = verify_result(LysisResultInputsV1 {
        nod_issued_at: 0,
        ..inputs(&fixture, &fixture.payload, &fixture.result)
    })
    .unwrap_err();
    assert!(matches!(
        error,
        outbe_ocomp_protocol::ProtocolError::InvalidInvariant("Lysis Nod issuance instant")
    ));
}
