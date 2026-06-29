//! UltraHonkKeccak proof verification for the gratis-pool circuit.
//!
//! The runtime is the source of truth for the proof's public inputs. Callers
//! transmit only the bare proof body in `SpendArgs.proof`; this module
//! prepends the runtime-known public inputs in the exact format
//! `Barretenberg::verify_combined` expects (Aztec's "combined proof":
//! `[u32-BE num_public_inputs | pub_in_0:32B | … | pub_in_N:32B | proof_body]`)
//! before forwarding to the FFI. That makes the binding atomic — a proof
//! cannot verify against any public inputs other than the ones the runtime
//! already gated, which closes the recycle-a-valid-proof-against-arbitrary-
//! `args` class of attacks.
//!
//! ## Verification key artefact
//!
//! The verification key is sourced from the canonical-circuit registry in
//! `outbe-zk-canonical` (`noir::commitment_nullifier_proof::VK_BYTES`). The
//! bytes are frozen in the upstream `outbe-circuits` repo via `cargo xtask
//! freeze-circuits`; bumping the `outbe-circuits` git ref here picks up any
//! VK update without changes to this crate.

use crate::errors::GratisPoolError;
use alloy_primitives::U256;
#[cfg(not(any(test, feature = "test-helpers")))]
use outbe_zk_canonical::noir::commitment_nullifier_proof;

/// Number of public inputs the gratis-pool circuit declares.
///
/// Must match the public-input arity of `fn main` in the
/// `outbe-commitment-nullifier-circuit` Noir program shipped by
/// `outbe-circuits`. Used both to size the combined-proof prefix here and
/// to type the `verify` callsite in `runtime.rs`.
pub const NUM_PUBLIC_INPUTS: usize = 7;

/// Verify an UltraHonkKeccak proof against the canonical commitment-nullifier
/// VK, binding the seven runtime-known public inputs to the proof.
///
/// `public_inputs` are field elements in **circuit declaration order**:
/// `[merkle_root, nullifier_hash, denom_id, receiver_binding,
///   tag_commit, tag_nullifier, tag_merkle]` — the order
/// `fn main(...)` in the upstream Noir circuit declares them and the order
/// Noir / bb emit them in the proof. Each is encoded as a 32-byte
/// big-endian field element; `denom_id` and the three `tag_*` values are the
/// BN254 Fr embedding of a small integer, i.e. the 32-byte BE encoding with
/// leading zeros.
///
/// The three domain-separator tags are public inputs (rather than circuit
/// constants) so the verifier pins them to the protocol-fixed
/// `TAG_COMMIT_GRATIS` / `TAG_NULLIFIER_GRATIS` / `TAG_MERKLE_GRATIS`
/// declared in `constants.rs` — this lets the same Noir bytecode be reused
/// across deployments while still gating each verification on the local
/// ceremony's tag triple.
///
/// `proof_body` is the bare proof bytes — the `bb prove` output with the
/// `[u32-BE count | N×32B public inputs]` prefix stripped off. This module
/// prepends a fresh prefix from `public_inputs` so the verifier sees exactly
/// what the runtime intends to gate.
///
/// Returns `Ok(())` when the proof verifies. On failure the returned
/// [`GratisPoolError::ProofInvalid`] wraps the root cause: either the
/// verifier's own error string (malformed proof, VK mismatch, FFI failure) or
/// the fact that the verifier ran cleanly but rejected the proof.
#[cfg(not(any(test, feature = "test-helpers")))]
pub fn verify(
    public_inputs: &[U256; NUM_PUBLIC_INPUTS],
    proof_body: &[u8],
) -> Result<(), GratisPoolError> {
    use outbe_zk_backend::barretenberg::{Barretenberg, RawVerifier};
    let combined = build_combined(public_inputs, proof_body);
    // `Barretenberg::default()` keeps `disable_zk = false`, which must match the
    // prover's setting — commitment-nullifier proofs are produced with ZK enabled.
    match Barretenberg::default().verify_combined(commitment_nullifier_proof::VK_BYTES, &combined) {
        Ok(true) => Ok(()),
        Ok(false) => Err(GratisPoolError::ProofInvalid(
            "verifier rejected the proof".to_string(),
        )),
        Err(e) => Err(GratisPoolError::ProofInvalid(e.to_string())),
    }
}

/// Build the Aztec "combined proof" blob: `[u32-BE num_public_inputs |
/// pub_in_0:32B | … | pub_in_{N-1}:32B | proof_body]`.
///
/// Mirrors the combined-proof layout `outbe_zk_backend`'s
/// `RawVerifier::verify_combined` parses. Exposed inside the crate so the
/// encoding-round-trip parity test can re-parse it with the same algorithm.
//
// Under the `test-helpers` feature the production `verify` isn't compiled
// (the mock takes over) so `build_combined` looks dead from the linker's
// perspective. The in-crate parity test still uses it under `cfg(test)`.
#[cfg_attr(feature = "test-helpers", allow(dead_code))]
pub(crate) fn build_combined(
    public_inputs: &[U256; NUM_PUBLIC_INPUTS],
    proof_body: &[u8],
) -> Vec<u8> {
    let mut combined = Vec::with_capacity(4 + NUM_PUBLIC_INPUTS * 32 + proof_body.len());
    combined.extend_from_slice(&(NUM_PUBLIC_INPUTS as u32).to_be_bytes());
    for pi in public_inputs {
        combined.extend_from_slice(&pi.to_be_bytes::<32>());
    }
    combined.extend_from_slice(proof_body);
    combined
}

// -------------------------------------------------------------------------
// Test-only verifier override
// -------------------------------------------------------------------------
//
// Real proofs are expensive to generate (Barretenberg prover ≈ 3–8 s per
// proof). Tests for schema / state / runtime control flow use this override
// to set the verifier's return value per-test, exercising the runtime's
// success and failure paths without invoking the FFI verifier.
//
// The override is gated by `cfg(test)` for in-crate tests and by the
// `test-helpers` Cargo feature for cross-crate test consumers (e.g. the
// credisfactory e2e tests). Production builds never see this code.

#[cfg(any(test, feature = "test-helpers"))]
mod test_override {
    use std::cell::Cell;

    thread_local! {
        /// Per-thread override of the verifier's return value. `None` means
        /// "fall back to the real verifier"; `Some(b)` forces `b`.
        pub(super) static OUTCOME: Cell<Option<bool>> = const { Cell::new(None) };
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub fn verify(
    _public_inputs: &[U256; NUM_PUBLIC_INPUTS],
    _proof_body: &[u8],
) -> Result<(), GratisPoolError> {
    if test_override::OUTCOME.with(|c| c.get().unwrap_or(false)) {
        Ok(())
    } else {
        Err(GratisPoolError::ProofInvalid(
            "verifier rejected the proof".to_string(),
        ))
    }
}

/// Force the verifier to return `outcome` for the duration of `f`.
///
/// Available under `cfg(test)` and under the `test-helpers` Cargo feature
/// (so neighbouring crates' integration tests can drive end-to-end flows
/// without a real prover). Restores the previous override after `f` returns.
#[cfg(any(test, feature = "test-helpers"))]
pub fn with_verifier_outcome<R>(outcome: bool, f: impl FnOnce() -> R) -> R {
    let prev = test_override::OUTCOME.with(|c| c.replace(Some(outcome)));
    let result = f();
    test_override::OUTCOME.with(|c| c.set(prev));
    result
}
