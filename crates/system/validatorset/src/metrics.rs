//! Prometheus metrics for ValidatorSet state transitions.
//!
//! Emitted from the corresponding mutation paths in `runtime.rs` so
//! operators have realtime visibility into validator lifecycle without
//! having to poll on-chain state.
//!
//! Per-validator labels: `addr` is the validator address rendered as
//! `0x{40-char-hex}`. Cardinality is bounded by the configured maximum
//! validator count (`config_max_validators`, default 128).

use alloy_primitives::Address;
use metrics::{counter, gauge};

fn addr_label(addr: Address) -> String {
    format!("{addr:?}")
}

/// Per-validator current status, one of the values from
/// [`crate::runtime::status`]:
/// `0=UNINIT`, `1=REGISTERED`, `2=ACTIVE`, `3=EXITING`, `4=UNBONDING`, `5=INACTIVE`.
pub fn record_validator_status(addr: Address, status: u8) {
    gauge!("outbe_validator_status", "addr" => addr_label(addr)).set(f64::from(status));
}

/// Cumulative force-exit events per validator.
pub fn record_validator_force_exit(addr: Address) {
    counter!("outbe_validator_force_exit_total", "addr" => addr_label(addr)).increment(1);
}

/// Cumulative voluntary deactivations per validator.
pub fn record_validator_deactivate(addr: Address) {
    counter!("outbe_validator_deactivate_total", "addr" => addr_label(addr)).increment(1);
}

/// One registration event (first-time or re-register).
pub fn record_validator_register(addr: Address, reregister: bool) {
    counter!(
        "outbe_validator_register_total",
        "addr" => addr_label(addr),
        "kind" => if reregister { "reregister" } else { "first" },
    )
    .increment(1);
}

/// One DKG reshare activation; `transitioned_to_unbonding` is the
/// number of validators transitioned EXITING→UNBONDING this round.
pub fn record_reshared_set_activated(active_count: u32, transitioned_to_unbonding: usize) {
    counter!("outbe_reshared_set_activated_total").increment(1);
    gauge!("outbe_validator_active_set_size").set(f64::from(active_count));
    gauge!("outbe_last_reshare_unbonding_count").set(transitioned_to_unbonding as f64);
}

/// Aggregate validator-status counts. Sample once per relevant
/// transition; cheap because validator-set size is bounded.
pub fn record_aggregate_status_counts(active: usize, exiting: usize, unbonding: usize) {
    gauge!("outbe_validator_active_count").set(active as f64);
    gauge!("outbe_validator_exiting_count").set(exiting as f64);
    gauge!("outbe_validator_unbonding_count").set(unbonding as f64);
}

/// Pending-set-change flag. 0/1.
pub fn record_pending_set_change(pending: bool) {
    gauge!("outbe_validator_pending_set_change").set(if pending { 1.0 } else { 0.0 });
}
