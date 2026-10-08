//! Exercises request finality and both terminal outcomes through production execution.
#[path = "ocomp_request_lifecycle/mod.rs"]
mod lifecycle;

#[test]
fn real_payload_builder_commits_atomic_request_expiry_without_retry() {
    lifecycle::expiry::run();
}

#[test]
fn real_payload_builder_commits_atomic_request_quorum_under_saturation() {
    lifecycle::quorum::run();
}

#[test]
fn real_payload_builder_continues_after_rejected_user_precompile_call() {
    lifecycle::rejection::run();
}

#[test]
fn real_payload_builder_includes_child_halt_and_next_tx_with_identical_replay() {
    lifecycle::rejection::run_child_halt();
}

#[test]
fn real_payload_builder_skips_carrier_with_invalid_inner_vote_signature() {
    lifecycle::carrier_skip::run();
}

#[test]
fn real_payload_builder_judges_a_carrier_on_the_state_left_by_earlier_transactions() {
    lifecycle::carrier_overlay::run();
}

#[test]
fn real_payload_builder_proposes_nothing_when_carrier_state_is_unreadable() {
    lifecycle::carrier_state_unavailable::run();
}

#[test]
fn real_payload_builder_output_near_the_transport_cap_passes_the_validator_size_check() {
    lifecycle::near_cap::run();
}

#[test]
fn authorized_vote_before_its_window_opens_is_early_not_invalid() {
    lifecycle::early_vote::run();
}
