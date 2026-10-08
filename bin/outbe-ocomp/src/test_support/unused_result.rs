use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    intent::JobIntentV1,
    result::{
        lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
        ConservationTotalsV1, ExactCountsV1, LysisResultV1, MetadosisCompletionSummaryV1,
        ResultRootsV1,
    },
};

/// These references identify stored evidence. They do not prove worker execution.
pub struct FixtureResultCommitments {
    pub input_manifest_hash: B256,
    pub plan_hash: B256,
    pub unit_artifact_root: B256,
    pub fidelity_fraction_root: B256,
    pub gratis_prefix_root: B256,
    pub result_chunk_list_root: B256,
    pub roots: ResultRootsV1,
}

pub fn unused_lysis_result(
    intent: &JobIntentV1,
    job_id: B256,
    commitments: FixtureResultCommitments,
) -> LysisResultV1 {
    let completion = unused_completion(intent);
    let conservation = unused_conservation(&completion);
    LysisResultV1 {
        protocol_bundle_hash: intent.protocol_bundle_hash,
        job_id,
        attempt: intent.attempt,
        input_manifest_hash: commitments.input_manifest_hash,
        plan_hash: commitments.plan_hash,
        unit_artifact_root: commitments.unit_artifact_root,
        fidelity_fraction_root: commitments.fidelity_fraction_root,
        gratis_prefix_root: commitments.gratis_prefix_root,
        result_chunk_count: 1,
        result_chunk_list_root: commitments.result_chunk_list_root,
        carry_over_credit: CarryOverCreditActionV1 {
            source_wwd: intent.wwd,
            reason: CarryOverReason::UnusedLysis,
            amount: completion.unused_lysis_limit_minor,
        },
        tribute_count: intent.authenticated_day_count,
        tribute_nominal_total: intent.authenticated_day_nominal,
        unused_lysis_limit_minor: completion.unused_lysis_limit_minor,
        metadosis_completion_summary: completion,
        roots: commitments.roots,
        counts: fixture_result_counts(intent.authenticated_day_count),
        conservation,
        arithmetic_commitment: B256::ZERO,
        event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
    }
}

fn unused_completion(intent: &JobIntentV1) -> MetadosisCompletionSummaryV1 {
    let mut summary = super::zero_completion_summary(
        intent.wwd,
        intent.pending_nonce,
        intent.logical_evaluation_height,
        intent.logical_evaluation_time,
    );
    let frozen = &intent.frozen_metadosis_values;
    summary.day_type = frozen.day_type;
    summary.tribute_nominal_total = intent.authenticated_day_nominal;
    summary.day_limit = frozen.day_limit;
    summary.gratis_demand = frozen.gratis_demand;
    summary.day_gratis_limit_minor = frozen.day_gratis_limit_minor;
    summary.lysis_limit_minor = frozen.lysis_limit_minor;
    summary.desis_limit_minor = frozen.desis_limit_minor;
    summary.unused_lysis_limit_minor = frozen.lysis_limit_minor;
    summary.carry_over_credit = frozen.lysis_limit_minor;
    summary
}

fn unused_conservation(summary: &MetadosisCompletionSummaryV1) -> ConservationTotalsV1 {
    ConservationTotalsV1 {
        tribute_nominal_total: summary.tribute_nominal_total,
        eligible_nominal_total: U256::ZERO,
        day_limit: summary.day_limit,
        gratis_demand: summary.gratis_demand,
        day_gratis_limit_minor: summary.day_gratis_limit_minor,
        lysis_limit_minor: summary.lysis_limit_minor,
        desis_limit_minor: summary.desis_limit_minor,
        lysis_allocation_minor: summary.lysis_allocation_minor,
        unused_lysis_limit_minor: summary.unused_lysis_limit_minor,
        carry_over_credit: summary.carry_over_credit,
        nod_cost_total: U256::ZERO,
    }
}

fn fixture_result_counts(tribute_count: u32) -> ExactCountsV1 {
    ExactCountsV1 {
        tribute_count,
        nod_count: tribute_count,
        bucket_count: 0,
        contributor_count: 0,
        semantic_event_count: 0,
    }
}
