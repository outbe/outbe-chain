//! Guards that the process-wide Simplex `Namespace` singleton in
//! `outbe_consensus::config::simplex_namespace()` produces byte-exact sub-
//! namespaces matching the verifier-side pins in `outbe-consensus-proof`.
//!
//! If a future refactor changes `NAMESPACE` or the Simplex `Namespace::new`
//! derivation, this test catches it before the live node diverges between
//! signer and verifier.

use outbe_consensus::config::{simplex_namespace, NAMESPACE};
use outbe_consensus::proof::{
    OUTBE_FINALIZE_NAMESPACE_V2, OUTBE_HYBRID_SEED_NAMESPACE_V2, OUTBE_NOTARIZE_NAMESPACE_V2,
};

#[test]
fn simplex_namespace_base_matches_outbe_app_namespace() {
    assert_eq!(NAMESPACE, b"outbe");
}

#[test]
fn simplex_namespace_returns_same_singleton_pointer() {
    let a = simplex_namespace();
    let b = simplex_namespace();
    assert!(
        std::ptr::eq(a, b),
        "simplex_namespace must return the same OnceLock instance",
    );
}

#[test]
fn simplex_namespace_sub_namespaces_match_consensus_proof_pins() {
    let ns = simplex_namespace();
    assert_eq!(
        ns.seed.as_slice(),
        OUTBE_HYBRID_SEED_NAMESPACE_V2,
        "signer seed namespace diverged from verifier pin",
    );
    assert_eq!(
        ns.notarize.as_slice(),
        OUTBE_NOTARIZE_NAMESPACE_V2,
        "signer notarize namespace diverged from verifier pin",
    );
    assert_eq!(
        ns.finalize.as_slice(),
        OUTBE_FINALIZE_NAMESPACE_V2,
        "signer finalize namespace diverged from verifier pin",
    );
}
