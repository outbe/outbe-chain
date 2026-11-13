//! VRF admission metrics shared with runtime consumers.
//!
//! Follow `crates/runtime/src/metrics_recorder.rs`: `outbe_<subsystem>_*`,
//! counters end in `_total`, millisecond histograms in `_ms`, and gauges
//! never end in `_total` or `_count`.

use metrics::{counter, gauge};

/// Record a complete vote+partial pair dropped before quorum admission.
pub fn record_vrf_partial_drop(verdict: &'static str, signer: u32) {
    counter!(
        "outbe_vrf_partial_dropped_total",
        "reason" => verdict,
        "signer" => signer.to_string(),
    )
    .increment(1);
}

/// Record an invalid threshold-VRF seed partial that attestation verification
/// excluded from recovery AND that is identity-attributable to its author (the
/// rider identity signature verified). A non-zero value means a committee member
/// deliberately emitted a garbage `bls_seed_partial` on the expected material
/// version (byzantine). The verifier dropped the complete attestation so it
/// cannot poison `recover_proof`. The verifier also buffered slashable evidence
/// for an external watcher to submit.
pub fn record_invalid_vrf_partial() {
    counter!("outbe_vrf_invalid_partial_total").increment(1);
}

/// Record an invalid threshold-VRF seed partial whose rider identity signature
/// did NOT verify, so it cannot be attributed to the claimed signer. It is a
/// probable in-transit relay forgery. Its complete attestation is dropped before
/// quorum admission but never slashed.
pub fn record_forged_seed_partial() {
    counter!("outbe_vrf_forged_seed_partial_total").increment(1);
}

/// `HybridScheme::recover_proof`
/// returned `None` while quorum was met. The proposer forfeits the slot instead
/// of stalling Simplex or emitting proof-less metadata.
pub fn record_vrf_recover_failed_under_quorum() {
    counter!("outbe_consensus_proposer_forfeit_total", "reason" => crate::forfeit::ProposerForfeitReason::VrfRecoverFailedUnderQuorum.label().to_string()).increment(1);
}

/// Record deterministic degraded leader election due to missing or invalid VRF.
pub fn record_vrf_degraded_leader_selection() {
    counter!("outbe_vrf_degraded_leader_selection_total").increment(1);
}

/// Record a fail-closed VRF expiry event.
pub fn record_vrf_randomness_expired() {
    counter!("outbe_vrf_randomness_expired_total").increment(1);
    gauge!("outbe_dkg_status").set(3.0);
}
