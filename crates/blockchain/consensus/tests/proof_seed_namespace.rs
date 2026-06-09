//! Pin V2 sub-namespace bytes byte-exact. Each constant must equal the
//! corresponding sub-namespace of `Namespace::new(b"outbe")` on the signer
//! side (`outbe_consensus::config::simplex_namespace()`). Drift here is a
//! hard-fork-equivalent break of consensus verification.

use outbe_consensus::proof::{
    OUTBE_FINALIZE_NAMESPACE_V2, OUTBE_HYBRID_SEED_NAMESPACE_V2, OUTBE_NOTARIZE_NAMESPACE_V2,
};

#[test]
fn vrf_seed_namespace_bytes_match_chainspec_constant() {
    assert_eq!(
        OUTBE_HYBRID_SEED_NAMESPACE_V2, b"outbe_SEED",
        "V2 VRF seed namespace bytes drifted",
    );
    assert_eq!(OUTBE_HYBRID_SEED_NAMESPACE_V2.len(), 10);
}

#[test]
fn notarize_namespace_bytes_pinned() {
    assert_eq!(OUTBE_NOTARIZE_NAMESPACE_V2, b"outbe_NOTARIZE");
}

#[test]
fn finalize_namespace_bytes_pinned() {
    assert_eq!(OUTBE_FINALIZE_NAMESPACE_V2, b"outbe_FINALIZE");
}
