//! CL-2: telemetry-label charset guard for consensus-crate spawn labels.
//!
//! commonware 2026.5.0's `validate_label` panics if a span/metric label is not
//! `[a-zA-Z][a-zA-Z0-9_]*` (the BUG-B class that crashed DKG rotation ~block 90).
//! This test feeds the labels the consensus crate passes to `Context::child(...)`
//! through the REAL commonware validator. That validator is the same function the
//! runtime invokes when it builds a child-context label. Thus an invalid label
//! fails here instead of panicking in production. It asserts actual label values
//! via the real validator. It does NOT scan source text.

/// Static labels the consensus crate passes to `Context::child(...)` on its
/// spawn / metric paths. Add new labels here when introducing a labeled child
/// context. New labels are also caught at run time: commonware panics in
/// `validate_label`. The actor/handler behavioral tests and the localnet
/// harness exercise that function when they actually spawn these contexts.
const CONSENSUS_SPAWN_LABELS: &[&str] = &[
    "ancestry",
    "broadcast",
    "cert_mux",
    "dkg_ceremony",
    "driver",
    "engine",
    "exec",
    "executor",
    "genesis",
    "marshal",
    "marshal_blocks",
    "marshal_finalizations",
    "marshal_node",
    "marshal_resolver",
    "network",
    "node",
    "propose",
    "res_mux",
    "resolver_handler",
    "verify",
    "vote_mux",
    "writer",
];

/// Commonware's real `validate_label` (which `panic!`s on an invalid charset)
/// accepts every consensus spawn label. A regression that renames a label to
/// an invalid form fails this test instead of crashing the node at runtime.
#[test]
fn consensus_spawn_labels_pass_commonware_validate_label() {
    for label in CONSENSUS_SPAWN_LABELS {
        commonware_runtime::telemetry::metrics::validate_label(label);
    }
}

/// Guard the guard: prove `validate_label` actually rejects the dotted form that
/// caused BUG-B, so the test above is meaningful (not a no-op validator).
#[test]
#[should_panic]
fn dotted_label_is_rejected_by_commonware_validate_label() {
    commonware_runtime::telemetry::metrics::validate_label("dkg.live");
}
