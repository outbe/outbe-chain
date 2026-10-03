//! Canonical local-result inputs shared by read-only and mutating scenarios.

use alloy_primitives::{B256, U256};
use eyre::WrapErr as _;
use outbe_ocomp_protocol::{
    hash::hash_framed,
    intent::DayType,
    profile::poc_schema_limits,
    registry::HashDomain,
    result::{
        lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
        CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisArithmeticSummaryV1,
        LysisResultV1, MetadosisCompletionSummaryV1, ResultRootsV1,
    },
};

pub(super) fn canonical_result() -> eyre::Result<(B256, LysisResultV1, Vec<u8>)> {
    let limits = poc_schema_limits();
    let roots = ResultRootsV1 {
        nod_root: B256::repeat_byte(0x31),
        bucket_root: B256::repeat_byte(0x32),
        contributor_root: B256::repeat_byte(0x33),
        output_manifest_root: B256::repeat_byte(0x34),
    };
    let counts = ExactCountsV1 {
        tribute_count: 1,
        nod_count: 1,
        bucket_count: 0,
        contributor_count: 0,
        semantic_event_count: 0,
    };
    let conservation = conservation_totals();
    let summary = arithmetic_summary(&roots, &counts, &conservation);
    let job_id = B256::repeat_byte(0x21);
    let result = LysisResultV1 {
        protocol_bundle_hash: B256::repeat_byte(0x20),
        job_id,
        attempt: 0,
        input_manifest_hash: summary.input_manifest_hash,
        plan_hash: summary.plan_hash,
        unit_artifact_root: summary.unit_artifact_root,
        fidelity_fraction_root: summary.fidelity_fraction_root,
        gratis_prefix_root: summary.gratis_prefix_root,
        result_chunk_count: 1,
        result_chunk_list_root: B256::repeat_byte(0x3a),
        carry_over_credit: CarryOverCreditActionV1 {
            source_wwd: 1,
            reason: CarryOverReason::UnusedLysis,
            amount: U256::ZERO,
        },
        metadosis_completion_summary: completion_summary(),
        tribute_count: 1,
        tribute_nominal_total: U256::ZERO,
        unused_lysis_limit_minor: U256::ZERO,
        roots,
        counts,
        conservation,
        arithmetic_commitment: hash_framed(
            HashDomain::LysisArithmetic,
            &summary.encode_canonical(&limits)?,
        )?,
        event_summary_hash: lysis_v1_empty_semantic_event_root()?,
    };
    let encoded = result
        .encode_canonical(&limits)
        .wrap_err("fixture result encodes canonically")?;
    Ok((job_id, result, encoded))
}

fn conservation_totals() -> ConservationTotalsV1 {
    ConservationTotalsV1 {
        tribute_nominal_total: U256::ZERO,
        eligible_nominal_total: U256::ZERO,
        day_limit: U256::ZERO,
        gratis_demand: U256::ZERO,
        day_gratis_limit_minor: U256::ZERO,
        lysis_limit_minor: U256::ZERO,
        desis_limit_minor: U256::ZERO,
        lysis_allocation_minor: U256::ZERO,
        unused_lysis_limit_minor: U256::ZERO,
        carry_over_credit: U256::ZERO,
        nod_cost_total: U256::ZERO,
    }
}

fn arithmetic_summary(
    roots: &ResultRootsV1,
    counts: &ExactCountsV1,
    conservation: &ConservationTotalsV1,
) -> LysisArithmeticSummaryV1 {
    LysisArithmeticSummaryV1 {
        input_manifest_hash: B256::repeat_byte(0x35),
        plan_hash: B256::repeat_byte(0x36),
        unit_artifact_root: B256::repeat_byte(0x37),
        fidelity_fraction_root: B256::repeat_byte(0x38),
        gratis_prefix_root: B256::repeat_byte(0x39),
        roots: roots.clone(),
        counts: counts.clone(),
        conservation: conservation.clone(),
        first_error_ordinal: None,
    }
}

fn completion_summary() -> MetadosisCompletionSummaryV1 {
    MetadosisCompletionSummaryV1 {
        wwd: 1,
        pending_nonce: 0,
        day_type: DayType::Green,
        tribute_nominal_total: U256::ZERO,
        day_limit: U256::ZERO,
        gratis_demand: U256::ZERO,
        day_gratis_limit_minor: U256::ZERO,
        lysis_limit_minor: U256::ZERO,
        desis_limit_minor: U256::ZERO,
        lysis_allocation_minor: U256::ZERO,
        unused_lysis_limit_minor: U256::ZERO,
        carry_over_credit: U256::ZERO,
        status: CompletionStatus::Completed,
        logical_evaluation_height: 1,
        logical_evaluation_time: 1,
    }
}
