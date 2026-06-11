//! V2 protocol constants used by the Hybrid proof verifier.
//!
//! Single source of truth for the application namespace + its derived Simplex
//! sub-namespaces (`_NOTARIZE`, `_FINALIZE`, `_SEED`, `_SEEDATTEST`).
//!
//! **Chain binding.** The application namespace is
//! `b"outbe" || chain_id_be`, so every signed consensus message and every
//! verification (vote/nullify/finalize/seed/seed-attest, the P2P handshake, and
//! SlashIndicator evidence) is bound to this chain. A validator that reuses its
//! BLS key on another Outbe deployment produces signatures under a different
//! namespace, so they no longer cross-verify or replay as fabricated
//! equivocation evidence. The chain id is genesis-fixed and constant for the
//! process; it is injected once at startup via [`init_consensus_chain_id`] and
//! every namespace accessor reads it, so the signer (`HybridScheme`) and the
//! deterministic verifier (this crate, run in the EVM executor — same process,
//! same chain) can never drift.

use commonware_consensus::simplex::scheme::Namespace;
use commonware_cryptography::bls12381;
use commonware_utils::ordered::Set;
use std::sync::OnceLock;

/// Unbound base of the Outbe application namespace. The full namespace appends
/// the consensus chain id (see [`outbe_app_namespace`]).
const OUTBE_APP_NAMESPACE_BASE: &[u8] = b"outbe";

/// Domain tag for the ordered validator-set commitment. Versioned so the
/// commitment scheme can evolve under a coordinated fork without colliding with
/// the previous one.
const COMMITTEE_COMMITMENT_DOMAIN: &[u8] = b"OUTBE_COMMITTEE_V1";

/// Process-wide consensus chain id, folded into every consensus namespace.
static CONSENSUS_CHAIN_ID: OnceLock<u64> = OnceLock::new();

/// Chain id used before [`init_consensus_chain_id`] runs (unit tests that do not
/// install one). Production always installs the real chain id at startup before
/// any signing or verification.
const DEFAULT_CONSENSUS_CHAIN_ID: u64 = 0;

/// Install the consensus chain id, once, at node startup — before any consensus
/// signing or block verification runs. Idempotent: the first value wins (the
/// chain id is genesis-fixed and constant for the process), so a duplicate call
/// with the same id is a no-op and a different id is ignored. MUST be called
/// before the first [`simplex_namespace`] access so the cached `Namespace`
/// singleton binds the real chain.
pub fn init_consensus_chain_id(chain_id: u64) {
    let _ = CONSENSUS_CHAIN_ID.set(chain_id);
}

/// The installed consensus chain id (or the default in unit tests).
pub fn consensus_chain_id() -> u64 {
    *CONSENSUS_CHAIN_ID
        .get()
        .unwrap_or(&DEFAULT_CONSENSUS_CHAIN_ID)
}

/// Chain-bound application namespace bytes: `b"outbe" || chain_id_be`.
pub fn outbe_app_namespace() -> Vec<u8> {
    let mut v = Vec::with_capacity(OUTBE_APP_NAMESPACE_BASE.len() + 8);
    v.extend_from_slice(OUTBE_APP_NAMESPACE_BASE);
    v.extend_from_slice(&consensus_chain_id().to_be_bytes());
    v
}

/// Ordered validator-set commitment: a 32-byte keccak over the committee's
/// BLS MinPk public keys in **canonical commonware `Set` order** (sorted,
/// deduplicated), domain-tagged and length-prefixed.
///
/// This is the "ordered validator-set commitment" the consensus-signature
/// invariant requires. It is folded into the INDIVIDUAL vote sub-namespaces
/// (notarize/nullify/finalize), so a vote signature produced under committee A
/// cannot verify under committee B even within the same chain and epoch —
/// closing the residual that committee-scoped verification covered only
/// operationally. The threshold seed / seed-attest namespaces stay chain-only:
/// the seed is a threshold signature already bound to the committee by its group
/// key, so a participant-set commitment there would be redundant.
///
/// **Parity contract.** This is the single source of truth for the commitment.
/// Every party (the `HybridScheme` signer/verifier, the V2 proof verifier in the
/// executor, the late-finalize verifier, and the SlashIndicator evidence
/// verifier) computes it from the SAME ordered committee via THIS function. The
/// input is a `Set`, whose `Ord`-sorted, deduplicated order matches the scheme's
/// participant indexing exactly, so the bytes are identical across nodes,
/// components, and crates by construction. Any divergence is caught pre-merge by
/// the fingerprint test and the 4-node localnet lockstep.
pub fn participant_set_commitment(committee: &Set<bls12381::PublicKey>) -> [u8; 32] {
    let mut buf = Vec::with_capacity(
        COMMITTEE_COMMITMENT_DOMAIN.len() + 4 + committee.len().saturating_mul(48),
    );
    buf.extend_from_slice(COMMITTEE_COMMITMENT_DOMAIN);
    // Length prefix binds the cardinality so a prefix/superset of one committee
    // cannot collide with another.
    buf.extend_from_slice(&(committee.len() as u32).to_be_bytes());
    for pk in committee.iter() {
        buf.extend_from_slice(commonware_codec::Encode::encode(pk).as_ref());
    }
    alloy_primitives::keccak256(&buf).0
}

/// Chain-only sub-namespace (`outbe_app_namespace() || suffix`), matching
/// commonware's `union(base, suffix)`. Used by the seed paths, which are already
/// committee-bound by the threshold polynomial.
fn sub_namespace(suffix: &[u8]) -> Vec<u8> {
    let mut v = outbe_app_namespace();
    v.extend_from_slice(suffix);
    v
}

/// Committee-bound vote sub-namespace:
/// `outbe_app_namespace() || suffix || participant_set_commitment(committee)`.
///
/// THE single derivation for the individual vote namespaces, used on both the
/// signing side (the `HybridScheme` `Namespace` vote fields are overridden with
/// these fns) and every verifying side (V2 proof verifier, late-finalize,
/// SlashIndicator evidence), so they agree by construction.
fn vote_sub_namespace(suffix: &[u8], committee: &Set<bls12381::PublicKey>) -> Vec<u8> {
    let mut v = outbe_app_namespace();
    v.extend_from_slice(suffix);
    v.extend_from_slice(&participant_set_commitment(committee));
    v
}

/// Simplex notarize sub-namespace, committee-bound.
pub fn notarize_namespace(committee: &Set<bls12381::PublicKey>) -> Vec<u8> {
    vote_sub_namespace(b"_NOTARIZE", committee)
}

/// Simplex nullify sub-namespace, committee-bound.
pub fn nullify_namespace(committee: &Set<bls12381::PublicKey>) -> Vec<u8> {
    vote_sub_namespace(b"_NULLIFY", committee)
}

/// Simplex finalize sub-namespace, committee-bound.
pub fn finalize_namespace(committee: &Set<bls12381::PublicKey>) -> Vec<u8> {
    vote_sub_namespace(b"_FINALIZE", committee)
}

/// Simplex VRF-seed sub-namespace: `outbe_app_namespace() || b"_SEED"`. Equals
/// `simplex_namespace().seed.as_slice()` byte-for-byte. Chain-only: the seed is a
/// threshold signature already bound to the committee via its group key.
pub fn hybrid_seed_namespace() -> Vec<u8> {
    sub_namespace(b"_SEED")
}

/// Seed-partial identity-attestation sub-namespace:
/// `outbe_app_namespace() || b"_SEEDATTEST"`. Chain-only (the VRF partial it
/// attributes is already committee-bound via the threshold polynomial).
///
/// Distinct from the four Simplex sub-namespaces so a seed-partial identity
/// signature can never be confused with a vote, nullify, finalize, or the
/// threshold-seed signature itself. Used by [`crate::proof::seed_partial`] — the
/// signer (`HybridScheme::sign`) and the SlashIndicator evidence verifier both
/// bind a validator's `bls_seed_partial` to its MinPk identity key under this
/// namespace so the partial becomes non-repudiably attributable.
pub fn seed_attest_namespace() -> Vec<u8> {
    sub_namespace(b"_SEEDATTEST")
}

/// Process-wide singleton of `Namespace::new(outbe_app_namespace())`.
///
/// Both signer (`outbe_consensus::config::simplex_namespace` re-exports this)
/// and the V2 verifier (this crate) read from the same `OnceLock`, so the four
/// `Vec<u8>` sub-namespaces are heap-allocated exactly once and signer/verifier
/// can never drift. [`init_consensus_chain_id`] MUST run before the first call
/// so the cached namespace binds the real chain.
pub fn simplex_namespace() -> &'static Namespace {
    static NAMESPACE_CELL: OnceLock<Namespace> = OnceLock::new();
    NAMESPACE_CELL.get_or_init(|| Namespace::new(&outbe_app_namespace()))
}
