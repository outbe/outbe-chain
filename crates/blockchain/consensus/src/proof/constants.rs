//! V2 protocol constants used by the Hybrid proof verifier.
//!
//! Single source of truth for the application namespace + its derived Simplex
//! sub-namespaces (`_NOTARIZE`, `_FINALIZE`, `_SEED`). Both the signer
//! (`outbe-consensus` re-exports [`simplex_namespace`]) and the verifier
//! (this crate) read from the same physical `OnceLock` — there is exactly one
//! [`Namespace`] instance in the process. The byte-pinned `OUTBE_*_NAMESPACE_V2`
//! constants are the same bytes pre-materialized at compile time so verifier
//! hot paths skip the Mutex lookup; a workspace test guards that they stay in
//! sync with the singleton's sub-namespaces.

use commonware_consensus::simplex::scheme::Namespace;
use std::sync::OnceLock;

/// Outbe application namespace bytes — the **only** namespace string defined
/// in the workspace. Every Simplex sub-namespace below is derived from this.
pub const OUTBE_APP_NAMESPACE: &[u8] = b"outbe";

/// Simplex notarize sub-namespace: `OUTBE_APP_NAMESPACE || b"_NOTARIZE"`.
pub const OUTBE_NOTARIZE_NAMESPACE_V2: &[u8] = b"outbe_NOTARIZE";

/// Simplex finalize sub-namespace: `OUTBE_APP_NAMESPACE || b"_FINALIZE"`.
pub const OUTBE_FINALIZE_NAMESPACE_V2: &[u8] = b"outbe_FINALIZE";

/// Simplex VRF-seed sub-namespace: `OUTBE_APP_NAMESPACE || b"_SEED"`.
///
/// Equals `simplex_namespace().seed.as_slice()` byte-for-byte. Signer
/// (`outbe-consensus`) uses the singleton; verifier (this crate) reads this
/// constant on hot paths. The `namespace_singleton` test guards drift.
pub const OUTBE_HYBRID_SEED_NAMESPACE_V2: &[u8] = b"outbe_SEED";

/// Process-wide singleton of `Namespace::new(OUTBE_APP_NAMESPACE)`.
///
/// Both signer (`outbe_consensus::config::simplex_namespace` re-exports this)
/// and the V2 verifier (this crate) read from the same `OnceLock`, so the four
/// `Vec<u8>` sub-namespaces are heap-allocated exactly once and signer/verifier
/// can never drift.
pub fn simplex_namespace() -> &'static Namespace {
    static NAMESPACE_CELL: OnceLock<Namespace> = OnceLock::new();
    NAMESPACE_CELL.get_or_init(|| Namespace::new(OUTBE_APP_NAMESPACE))
}
