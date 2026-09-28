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
