//! Executable contract between production emitters, Reth exposition and monitoring.

mod stage2;

use std::{collections::HashMap, time::Duration};

use alloy_primitives::Address;
use outbe_consensus::{forfeit::ProposerForfeitReason, metrics::*};
use outbe_radicle::integration::{RadicleMetrics, RadicleStatusSnapshot, RadicleVotingGate};
use outbe_runtime::metrics_recorder::{validate_outbe_metric_taxonomy, OUTBE_METRIC_SUBSYSTEMS};
use outbe_slashindicator::metrics::*;
use outbe_validatorset::metrics::*;
use serde_json::Value;

const LATENCIES: &[&str] = &[
    "reth_outbe_consensus_block_verify_execution_ms",
    "reth_outbe_consensus_block_verify_decision_ms",
    "reth_outbe_consensus_candidate_timestamp_ahead_ms",
    "reth_outbe_consensus_block_build_resolve_pad_ms",
    "reth_outbe_consensus_executor_queue_wait_ms",
    "reth_outbe_txpool_validation_duration_ms",
    "reth_outbe_evm_payload_build_duration_ms",
    "reth_outbe_evm_payload_state_root_duration_ms",
    "reth_outbe_tee_client_execution_lock_wait_ms",
    "reth_outbe_tee_canary_latency_ms",
    "reth_outbe_tee_client_request_duration_ms",
    "reth_outbe_consensus_block_build_duration_ms",
    "reth_outbe_consensus_phase1_finalization_wait_ms",
    "reth_outbe_consensus_phase1_finalization_after_cn_ms",
];

#[tokio::test]
async fn prometheus_recorder_contract() {
    let recorder = outbe_runtime::install_outbe_prometheus_recorder()
        .expect("install outbe prometheus recorder");
    assert!(std::ptr::eq(
        recorder,
        outbe_runtime::install_outbe_prometheus_recorder().expect("repeat installation")
    ));
    emit_consensus();
    emit_validatorset();
    emit_slashing();
    emit_radicle();
    stage2::emit().await;
    metrics_process::Collector::default().collect();
    let rendered = recorder.handle().render();
    assert_eq!(validate_outbe_metric_taxonomy(&rendered), Ok(()));
    let types = metric_types(&rendered);
    assert_eq!(
        types.get("reth_outbe_consensus_metrics_exporter_up"),
        Some(&"gauge")
    );
    stage2::assert_stage2(&rendered, &types);
    assert_subsystems(&rendered, &types);
    assert_latency_buckets(&rendered, &types);
    assert_monitoring(&rendered, &types);
    assert_expression_regressions(&rendered, &types);
    assert_eq!(types["reth_process_resident_memory_bytes"], "gauge");
    assert!(has_sample(&rendered, "reth_process_resident_memory_bytes"));
}

fn emit_consensus() {
    record_metrics_exporter_up(true);
    record_block_proposed(10);
    record_block_finalized(9, 3, 4);
    record_views_skipped(1);
    record_view_nullified();
    record_current_view(10);
    record_epoch(1);
    record_commonware_p2p_active_peers(3);
    record_consensus_reth_state(10, 9, false);
    record_parent_cert_store_size(1);
    record_parent_cert_included();
    record_parent_cert_missing();
    record_block_cache_size(1);
    record_finalize_verify_admission("accepted");
    record_finalize_verify_verification("valid");
    record_finalize_verify_queue_depth(1, 32);
    record_finalize_verify_observed(1, 32);
    record_parent_cert_record_pruned(1);
    record_parent_cert_retained_depth(10);
    record_phase1_used_cn_fallback(1, 10);
    emit_consensus_events();
    emit_consensus_drop_reasons();
    emit_consensus_evidence();
    emit_dkg_vrf();
    emit_consensus_latencies();
}

fn emit_consensus_events() {
    record_finalization_resolution_stalled(1);
    for emit in [
        record_finalization_divergence_suspected,
        record_finalization_divergence_cleared,
        record_finalization_snapshot_build_degraded,
        record_finalization_persist_degraded,
        record_finalization_persist_recovered,
        record_executor_head_flip,
        record_executor_head_finalized_conflict,
        record_executor_head_rollback,
        record_executor_finalized_conflict,
        record_executor_finalized_stale,
        record_finalized_cert_invalid_vrf_proof,
        record_certification_persisted,
        record_parent_proof_unavailable_forfeit,
        record_phase1_parent_proof_unavailable,
        record_parent_proof_recovered_from_marshal,
        record_dkg_boundary_duplicate_rejected,
        record_genesis_dkg_boundary_not_ready_forfeit,
    ] {
        emit();
    }
}

fn emit_consensus_drop_reasons() {
    [
        FinalizationDropReason::MailboxClosed,
        FinalizationDropReason::StaleRound,
        FinalizationDropReason::SameRoundInconsistency,
        FinalizationDropReason::DuplicateRound,
    ]
    .into_iter()
    .for_each(record_finalization_dropped);
    [
        CertificationDropReason::QueueFull,
        CertificationDropReason::VerifyFailed,
        CertificationDropReason::SnapshotBuildFailed,
        CertificationDropReason::MailboxClosed,
        CertificationDropReason::StoreError,
    ]
    .into_iter()
    .for_each(record_certification_dropped);
}

fn emit_consensus_evidence() {
    [
        EquivocationKind::ConflictingNotarize,
        EquivocationKind::ConflictingFinalize,
        EquivocationKind::NullifyFinalize,
    ]
    .into_iter()
    .for_each(record_byzantine_evidence);
    [
        ProposerForfeitReason::ParentProofUnavailable,
        ProposerForfeitReason::GenesisDkgBoundaryNotReady,
        ProposerForfeitReason::VrfRecoverFailedUnderQuorum,
    ]
    .into_iter()
    .for_each(record_proposer_forfeit);
}

fn emit_dkg_vrf() {
    record_dkg_status(2);
    record_reshare_completed();
    record_dkg_revealed_shares(1);
    record_vrf_partial_drop("invalid", 1);
    [
        DkgBoundaryDecision::AlreadyCommitted,
        DkgBoundaryDecision::MustEmit,
        DkgBoundaryDecision::NoPending,
    ]
    .into_iter()
    .for_each(record_dkg_boundary_requirement);
    [
        DkgBoundaryUnavailableReason::GenesisBoundaryNotReady,
        DkgBoundaryUnavailableReason::AncestryUnavailable,
    ]
    .into_iter()
    .for_each(record_dkg_boundary_unavailable);
    for emit in [
        record_invalid_vrf_partial,
        record_forged_seed_partial,
        record_vrf_recover_failed_under_quorum,
        record_vrf_degraded_leader_selection,
        record_vrf_randomness_expired,
    ] {
        emit();
    }
}

fn emit_consensus_latencies() {
    for emit in [
        record_block_build_time,
        record_phase1_finalization_wait_ms,
        record_phase1_finalization_record_arrived_after_cn,
    ] {
        emit(Duration::from_millis(5));
    }
}

fn emit_validatorset() {
    let addr = Address::repeat_byte(1);
    record_validator_status(addr, 2);
    record_validator_force_exit(addr);
    record_validator_deactivate(addr);
    for reregister in [false, true] {
        record_validator_register(addr, reregister);
    }
    record_reshared_set_activated(4, 1);
    record_aggregate_status_counts(4, 1, 1);
    record_pending_set_change(true);
    record_validator_tee_expiry(addr, "active_demoted");
    record_tee_expiry_exclusions(1, 1);
}

fn emit_slashing() {
    let addr = Address::repeat_byte(1);
    record_proposer_miss_count(addr, 1);
    record_proposer_miss_event(addr);
    record_voter_miss_count(addr, 1);
    record_voter_miss_event(addr);
    record_felony_count(addr, 1);
    record_validator_slashed(addr, "byzantine");
    record_epoch_counters_reset(4);
}

fn emit_radicle() {
    let mut snapshot = RadicleStatusSnapshot::default();
    snapshot.local_node_id = Some([1; 32]);
    snapshot.voting_gate = RadicleVotingGate::SignerAllowed;
    snapshot.manager.resolved_peer_count = 1;
    snapshot.manager.finality_regressions = 1;
    snapshot.manager.finality_conflicts = 1;
    snapshot.manager.provider_failures = 1;
    snapshot.manager.endpoint_failures = 1;
    snapshot.manager.uds_failures = 1;
    snapshot.manager.tcp_status_failures = 1;
    RadicleMetrics::default().record(&snapshot);
}

fn metric_types(rendered: &str) -> HashMap<&str, &str> {
    rendered
        .lines()
        .filter_map(|line| line.strip_prefix("# TYPE "))
        .map(|declaration| declaration.split_once(' ').expect("metric TYPE"))
        .collect()
}

fn has_sample(rendered: &str, name: &str) -> bool {
    rendered
        .lines()
        .any(|line| !line.starts_with('#') && line.split(['{', ' ']).next() == Some(name))
}

fn assert_subsystems(rendered: &str, types: &HashMap<&str, &str>) {
    for subsystem in OUTBE_METRIC_SUBSYSTEMS {
        let prefix = format!("reth_outbe_{subsystem}_");
        assert!(
            types
                .keys()
                .any(|name| { name.starts_with(&prefix) && has_sample(rendered, name) }),
            "missing subsystem {subsystem}"
        );
    }
    assert!(rendered.contains("reth_outbe_radicle_voting_gate_signer_allowed 1"));
    assert!(rendered.contains("reth_outbe_tee_canary_failures_total 1"));
    assert_eq!(types["reth_outbe_follower_upstream_tip_known"], "gauge");
    assert_eq!(types["reth_outbe_p2p_send_rate_limited_total"], "counter");
    assert_eq!(
        types["reth_outbe_evm_zero_fee_soft_failures_total"],
        "counter"
    );
    assert_eq!(
        types["reth_outbe_evm_zero_fee_cap_exceeded_total"],
        "counter"
    );
    assert!(rendered.contains("reth_outbe_follower_upstream_tip_known 1"));
    assert!(rendered.contains("reth_outbe_p2p_send_rate_limited_total{channel=\"dkg\"} 1"));
    assert!(rendered.contains("reth_outbe_evm_zero_fee_soft_failures_total 1"));
    assert!(rendered.contains("reth_outbe_evm_zero_fee_cap_exceeded_total 1"));
}

fn assert_latency_buckets(rendered: &str, types: &HashMap<&str, &str>) {
    for name in LATENCIES {
        assert_eq!(types.get(name), Some(&"histogram"), "{name}");
        for boundary in ["0.5", "250", "10000", "+Inf"] {
            let prefix = format!("{name}_bucket{{");
            let label = format!("le=\"{boundary}\"");
            assert!(
                rendered.lines().any(|line| {
                    line.strip_prefix(&prefix)
                        .and_then(|rest| rest.split_once('}'))
                        .is_some_and(|(labels, _)| labels.split(',').any(|item| item == label))
                }),
                "missing {name} bucket {boundary}"
            );
        }
    }
}

fn assert_monitoring(rendered: &str, types: &HashMap<&str, &str>) {
    let alerts: Value = serde_yaml_ng::from_str(include_str!(
        "../../../../deploy/monitoring/alerts.example.yaml"
    ))
    .expect("parse alert YAML");
    let dashboard: Value = serde_json::from_str(include_str!(
        "../../../../deploy/monitoring/grafana/dashboards/outbe-overview.json"
    ))
    .expect("parse dashboard JSON");
    for document in [&alerts, &dashboard] {
        let mut expressions = Vec::new();
        collect_expressions(document, &mut expressions);
        assert!(
            !expressions.is_empty(),
            "monitoring must contain expressions"
        );
        for expression in expressions {
            check_expression(expression, rendered, types).expect(expression);
        }
    }
}

fn collect_expressions<'a>(value: &'a Value, expressions: &mut Vec<&'a str>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if key == "expr" {
                    expressions.push(child.as_str().expect("string expression"));
                } else {
                    collect_expressions(child, expressions);
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_expressions(child, expressions);
            }
        }
        _ => {}
    }
}

fn expression_tokens(expression: &str) -> Vec<&str> {
    // Quoted label values are not metric references; retain parentheses for rate scopes.
    let mut tokens = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut start = None;
    for (index, character) in expression.char_indices() {
        if quoted {
            if !escaped && character == '"' {
                quoted = false;
            }
            escaped = !escaped && character == '\\';
            continue;
        }
        if character.is_ascii_alphanumeric() || character == '_' || character == ':' {
            start.get_or_insert(index);
            continue;
        }
        if let Some(begin) = start.take() {
            tokens.push(&expression[begin..index]);
        }
        quoted = character == '"';
        if character == '(' || character == ')' {
            tokens.push(&expression[index..index + 1]);
        }
    }
    if let Some(begin) = start {
        tokens.push(&expression[begin..]);
    }
    tokens
}

fn is_wire_metric(name: &str) -> bool {
    name.starts_with("reth_outbe_") || name.starts_with("reth_process_")
}

fn series_type<'a>(name: &str, types: &HashMap<&str, &'a str>) -> Option<&'a str> {
    types.get(name).copied().or_else(|| {
        ["_bucket", "_sum", "_count"].iter().find_map(|suffix| {
            name.strip_suffix(suffix)
                .and_then(|base| types.get(base).copied())
                .filter(|kind| *kind == "histogram")
        })
    })
}

fn check_expression(
    expression: &str,
    rendered: &str,
    types: &HashMap<&str, &str>,
) -> Result<(), String> {
    let tokens = expression_tokens(expression);
    let mut rates = Vec::new();
    let mut depth = 0;
    for (index, name) in tokens.iter().copied().enumerate() {
        match name {
            "(" => depth += 1,
            ")" => {
                rates.retain(|scope| *scope < depth);
                depth -= 1;
            }
            "rate" | "increase" if tokens.get(index + 1) == Some(&"(") => rates.push(depth + 1),
            _ => {}
        }
        check_reference(name, rendered, types, !rates.is_empty())?;
    }
    Ok(())
}

fn check_reference(
    name: &str,
    rendered: &str,
    types: &HashMap<&str, &str>,
    in_rate: bool,
) -> Result<(), String> {
    if name.starts_with("outbe_")
        || name.starts_with("commonware_p2p_")
        || name == "process_resident_memory_bytes"
    {
        return Err(format!("bare metric {name}"));
    }
    if !is_wire_metric(name) {
        return Ok(());
    }
    let kind = series_type(name, types).ok_or_else(|| format!("missing TYPE {name}"))?;
    if !has_sample(rendered, name) {
        return Err(format!("missing sample {name}"));
    }
    if in_rate && kind == "gauge" {
        return Err(format!("rate/increase on gauge {name}"));
    }
    Ok(())
}

const EXPRESSION_CASES: &[(&str, bool)] = &[
        ("outbe_tee_canary_consecutive_failures", false),
        ("process_resident_memory_bytes", false),
        ("commonware_p2p_peers", false),
        ("reth_outbe_tee_missing", false),
        ("rate(reth_outbe_tee_canary_consecutive_failures[5m])", false),
        (
            "increase (reth_outbe_tee_canary_consecutive_failures[5m])",
            false,
        ),
        ("sum(rate(reth_outbe_tee_canary_consecutive_failures[5m]))", false),
        ("reth_outbe_tee_canary_consecutive_failures{kind=\"outbe_label_only\"}", true),
        ("delta(reth_outbe_tee_canary_consecutive_failures[5m])", true),
        (
            "rate(reth_outbe_tee_canary_latency_ms_bucket[5m]) + reth_outbe_tee_canary_consecutive_failures",
            true,
        ),
        ("increase(reth_outbe_tee_canary_latency_ms_count[5m])", true),
        ("rate(reth_outbe_tee_canary_latency_ms_sum[5m])", true),
];

fn assert_expression_regressions(rendered: &str, types: &HashMap<&str, &str>) {
    for &(expression, valid) in EXPRESSION_CASES {
        assert_eq!(
            check_expression(expression, rendered, types).is_ok(),
            valid,
            "{expression}"
        );
    }
}
