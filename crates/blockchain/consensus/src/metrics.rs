//! Prometheus metrics for Outbe consensus.
//!
//! These metrics are exposed via reth's `--metrics` endpoint and use the global
//! `metrics` crate recorder. No additional setup is needed beyond reth's built-in
//! Prometheus exporter.

use metrics::{counter, gauge, histogram};
use std::time::Duration;

/// Record a block proposed by this node.
pub fn record_block_proposed(block_number: u64) {
    counter!("outbe_blocks_proposed_total").increment(1);
    gauge!("outbe_proposed_height").set(block_number as f64);
}

/// Record a finalized Simplex view.
pub fn record_block_finalized(view: u64, signers: usize, total: usize) {
    gauge!("outbe_finalized_view").set(view as f64);
    gauge!("outbe_validator_count").set(total as f64);

    if total > 0 {
        let rate = signers as f64 / total as f64;
        gauge!("outbe_participation_rate").set(rate);
    }
}

/// Record skipped views (missed proposers).
pub fn record_views_skipped(count: u64) {
    counter!("outbe_views_skipped_total").increment(count);
}

/// Record a finalized event dropped before block resolution.
pub fn record_finalization_dropped(reason: &str) {
    counter!("outbe_finalization_dropped_total", "reason" => reason.to_string()).increment(1);
}

/// Record a pre-finalization canonical-head reorg at the same height.
///
/// Expected on view timeout / leader rotation; a sustained non-zero rate
/// indicates network turbulence (frequent view changes near the tip).
pub fn record_executor_head_flip() {
    counter!("outbe_executor_head_flip_total").increment(1);
}

/// Record a rejected `update_head` at finalized height with a hash that
/// conflicts with the committed finalized hash. Non-zero is a protocol alarm.
pub fn record_executor_head_finalized_conflict() {
    counter!("outbe_executor_head_finalized_conflict_total").increment(1);
}

/// Record a canonical-head move to a strictly lower height
/// (parent switch onto a shorter pre-finalization branch).
pub fn record_executor_head_rollback() {
    counter!("outbe_executor_head_rollback_total").increment(1);
}

/// Record an attempted finalized rewrite at the current finalized height
/// with a different digest. Always ignored by the executor's monotonic
/// guard; a non-zero value is a protocol-invariant alarm and must be
/// investigated.
pub fn record_executor_finalized_conflict() {
    counter!("outbe_executor_finalized_conflict_total").increment(1);
}

/// Record a stale finalize delivery below the committed finalized height.
/// Always ignored; non-zero indicates duplicate or out-of-order marshal
/// delivery and is mostly informational.
pub fn record_executor_finalized_stale() {
    counter!("outbe_executor_finalized_stale_total").increment(1);
}

/// Record the current epoch number.
pub fn record_epoch(epoch: u64) {
    gauge!("outbe_epoch_number").set(epoch as f64);
}

/// Record the Commonware P2P peer-manager target size.
pub fn record_commonware_p2p_active_peers(count: usize) {
    gauge!("commonware_p2p_active_peers").set(count as f64);
}

/// Record consensus tip vs Reth provider canonical-state readiness.
pub fn record_consensus_reth_state(
    consensus_tip_height: u64,
    reth_head_height: u64,
    hash_match: bool,
) {
    let consensus_ahead = consensus_tip_height.saturating_sub(reth_head_height);
    let reth_ahead = reth_head_height.saturating_sub(consensus_tip_height);
    let readiness_gap =
        consensus_reth_readiness_gap_blocks(consensus_tip_height, reth_head_height, hash_match);

    gauge!("outbe_consensus_tip_block_height").set(consensus_tip_height as f64);
    gauge!("outbe_reth_provider_head_block_height").set(reth_head_height as f64);
    gauge!("outbe_consensus_ahead_blocks").set(consensus_ahead as f64);
    gauge!("outbe_reth_ahead_blocks").set(reth_ahead as f64);
    gauge!("outbe_consensus_reth_height_diff")
        .set(signed_height_diff(consensus_tip_height, reth_head_height) as f64);
    gauge!("outbe_consensus_reth_tip_hash_match").set(if hash_match { 1.0 } else { 0.0 });
    gauge!("outbe_consensus_tip_available_in_reth_provider").set(if hash_match {
        1.0
    } else {
        0.0
    });
    gauge!("outbe_consensus_reth_readiness_gap_blocks").set(readiness_gap as f64);
}

fn signed_height_diff(consensus_tip_height: u64, reth_head_height: u64) -> i64 {
    if consensus_tip_height >= reth_head_height {
        i64::try_from(consensus_tip_height - reth_head_height).unwrap_or(i64::MAX)
    } else {
        i64::try_from(reth_head_height - consensus_tip_height).map_or(i64::MIN, |diff| -diff)
    }
}

pub(crate) fn consensus_reth_readiness_gap_blocks(
    consensus_tip_height: u64,
    reth_head_height: u64,
    hash_match: bool,
) -> u64 {
    if hash_match {
        0
    } else {
        consensus_tip_height.saturating_sub(reth_head_height).max(1)
    }
}

/// Record DKG/reshare status.
/// 0 = idle, 1 = in progress, 2 = completed (then resets to idle), 3 = expired.
pub fn record_dkg_status(status: u8) {
    gauge!("outbe_dkg_status").set(status as f64);
}

/// Record a fail-closed VRF expiry event.
pub fn record_vrf_randomness_expired() {
    counter!("outbe_vrf_randomness_expired_total").increment(1);
    record_dkg_status(3);
}

/// Record a DKG reshare completion event.
pub fn record_reshare_completed() {
    counter!("outbe_reshares_completed_total").increment(1);
}

/// Record deterministic degraded leader election due to missing or invalid VRF.
pub fn record_vrf_degraded_leader_selection() {
    counter!("outbe_vrf_degraded_leader_selection_total").increment(1);
}

/// Record a byzantine evidence detection event.
pub fn record_byzantine_evidence(evidence_type: &str) {
    counter!("outbe_byzantine_evidence_total", "type" => evidence_type.to_string()).increment(1);
}

/// Builder included exact-parent metadata in the Phase 1 system transaction.
pub fn record_parent_cert_included() {
    counter!("outbe_parent_cert_included_total").increment(1);
}

/// Builder produced a valid block without an eligible exact-parent certificate.
pub fn record_parent_cert_missing() {
    counter!("outbe_parent_cert_missing_total").increment(1);
}

/// Builder skipped a candidate because exact-parent validation did not accept it.
pub fn record_parent_cert_invalid_omitted(verdict: &str) {
    counter!("outbe_parent_cert_invalid_omitted_total", "verdict" => verdict.to_string())
        .increment(1);
}

/// Current exact-parent certificate handoff store entry count.
pub fn record_parent_cert_store_size(size: usize) {
    gauge!("outbe_parent_cert_store_size").set(size as f64);
}

/// Current `block_cache` entry count. Sampled on every bounded insert
/// so operators can verify the cap is enforced and alert on cache
/// growth that approaches `BLOCK_CACHE_MAX_ENTRIES`.
pub fn record_block_cache_size(size: usize) {
    gauge!("outbe_block_cache_size").set(size as f64);
}

/// Exact-parent certificate records dropped by age prune.
pub fn record_parent_cert_record_pruned(count: usize) {
    if count > 0 {
        counter!("outbe_parent_cert_record_pruned_total").increment(count as u64);
    }
}

/// Current height minus oldest retained exact-parent certificate record.
pub fn record_parent_cert_retained_depth(depth: u64) {
    gauge!("outbe_parent_cert_retained_depth").set(depth as f64);
}

/// Verifier-side exact-parent certificate rejection.
pub fn record_parent_cert_verify_rejected(verdict: &str) {
    counter!("outbe_parent_cert_verify_rejected_total", "verdict" => verdict.to_string())
        .increment(1);
}

/// `Activity::Certification` admitted by the reporter and persisted to the
/// certified-parent proof store.
pub fn record_certification_persisted() {
    counter!("outbe_certification_persisted_total").increment(1);
}

/// `Activity::Certification` dropped by the reporter before persistence.
/// Reasons:
/// - `"verify_failed"` — Notarization signature did not verify.
/// - `"store_error"` — durable proof-store write returned an error.
pub fn record_certification_dropped(reason: &str) {
    counter!("outbe_certification_dropped_total", "reason" => reason.to_string()).increment(1);
}

/// deterministic proposer-forfeit metric. See
/// [`crate::forfeit::ProposerForfeitReason`] for the closed reason set.
pub fn record_proposer_forfeit(reason: crate::forfeit::ProposerForfeitReason) {
    counter!("outbe_proposer_forfeit_total", "reason" => reason.label().to_string()).increment(1);
}

/// Convenience: proposer forfeit because no direct
/// parent proof (finalization, certified-notarization, or bounded fetch)
/// was available within budget.
pub fn record_parent_proof_unavailable_forfeit() {
    record_proposer_forfeit(crate::forfeit::ProposerForfeitReason::ParentProofUnavailable);
}

/// Proposer could not find a usable Phase 1 parent proof for the exact parent.
pub fn record_phase1_parent_proof_unavailable() {
    counter!("outbe_phase1_parent_proof_unavailable_total").increment(1);
}

/// Proposer used a locally observed certified-notarization witness after the
/// bounded finalization wait expired.
pub fn record_phase1_used_cn_fallback(epoch: u64, view: u64) {
    counter!(
        "outbe_phase1_used_cn_fallback_total",
        "epoch" => epoch.to_string(),
        "view" => view.to_string()
    )
    .increment(1);
}

/// Time spent waiting for the finalization slot before either finalization won
/// or CN fallback was used.
pub fn record_phase1_finalization_wait_ms(duration: Duration) {
    histogram!("outbe_phase1_finalization_wait_ms").record(duration.as_secs_f64() * 1000.0);
}

/// Finalization arrived after a CN witness had already been observed.
pub fn record_phase1_finalization_record_arrived_after_cn(duration: Duration) {
    histogram!("outbe_phase1_finalization_record_arrived_after_cn_ms")
        .record(duration.as_secs_f64() * 1000.0);
}

/// DKG boundary requirement decision for proposer/verifier observability.
pub fn record_dkg_boundary_requirement(decision: &str) {
    counter!("outbe_dkg_boundary_requirement_total", "decision" => decision.to_string())
        .increment(1);
}

/// Boundary requirement could not be derived from the parent snapshot and
/// bounded ancestry.
pub fn record_dkg_boundary_unavailable(reason: &str) {
    counter!("outbe_dkg_boundary_unavailable_total", "reason" => reason.to_string()).increment(1);
}

/// A block carried a DKG boundary artifact after the same pending boundary had
/// already been committed in its parent ancestry.
pub fn record_dkg_boundary_duplicate_rejected() {
    counter!("outbe_dkg_boundary_duplicate_rejected_total").increment(1);
}

/// block-1 proposal cannot be built
/// because the DKG boundary artifact for epoch 0 is not ready.
pub fn record_genesis_dkg_boundary_not_ready_forfeit() {
    record_proposer_forfeit(crate::forfeit::ProposerForfeitReason::GenesisDkgBoundaryNotReady);
}

/// `HybridScheme::recover_proof`
/// returned `None` while quorum was met. Proposer forfeits the slot rather
/// than stalling Simplex or emitting proof-less metadata.
pub fn record_vrf_recover_failed_under_quorum() {
    record_proposer_forfeit(crate::forfeit::ProposerForfeitReason::VrfRecoverFailedUnderQuorum);
}

/// Real build + marshal cost of a proposal (elapsed since the proposer's
/// closure-level `propose_start`), before any min-block-time floor pad.
pub fn record_block_build_time(duration: Duration) {
    histogram!("outbe_block_build_time_ms").record(duration.as_secs_f64() * 1000.0);
}

/// Floor pad the proposer sleeps to reach `min_block_time` (the intended pad;
/// `0` on the case-C / floor-already-met no-wait path). Lets operators see, per
/// block, how much of the cadence is real build vs. liveness pacing.
pub fn record_block_wait_time(duration: Duration) {
    histogram!("outbe_block_wait_time_ms").record(duration.as_secs_f64() * 1000.0);
}

#[cfg(test)]
mod tests {
    use super::consensus_reth_readiness_gap_blocks;

    #[test]
    fn readiness_gap_is_zero_when_provider_has_consensus_tip_hash() {
        assert_eq!(consensus_reth_readiness_gap_blocks(203, 201, true), 0);
    }

    #[test]
    fn readiness_gap_tracks_consensus_ahead_when_tip_hash_is_missing() {
        assert_eq!(consensus_reth_readiness_gap_blocks(203, 201, false), 2);
    }

    #[test]
    fn readiness_gap_marks_same_height_hash_mismatch_as_nonzero() {
        assert_eq!(consensus_reth_readiness_gap_blocks(203, 203, false), 1);
    }
}
