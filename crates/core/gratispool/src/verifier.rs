//! UltraHonkKeccak proof verification for the gratis-pool circuit.
//!
//! The runtime is the source of truth for the proof's public inputs. Callers
//! transmit only the bare proof body in `SpendArgs.proof`; this module
//! prepends the runtime-known public inputs in the exact format
//! `verify_ultra_honk_keccak` expects (Aztec's "combined proof":
//! `[u32-BE num_public_inputs | pub_in_0:32B | … | pub_in_N:32B | proof_body]`)
//! before forwarding to the FFI. That makes the binding atomic — a proof
//! cannot verify against any public inputs other than the ones the runtime
//! already gated, which closes the recycle-a-valid-proof-against-arbitrary-
//! `args` class of attacks.
//!
//! ## Verification key artefact
//!
//! The verification key is sourced from the canonical-circuit table in
//! `outbe-zk-canonical` (`COMMITMENT_NULLIFIER.vk_bytes`). The bytes are
//! derived in the upstream `outbe-circuits` repo via `cargo xtask
//! regenerate-canonical`; bumping the `outbe-circuits` git tag here picks
//! up any VK update without changes to this crate.

use alloy_primitives::U256;
#[cfg(not(any(test, feature = "test-helpers")))]
use outbe_zk_canonical::COMMITMENT_NULLIFIER;

/// Number of public inputs the gratis-pool circuit declares.
///
/// Must match the public-input arity of `fn main` in the
/// `outbe-commitment-nullifier-circuit` Noir program shipped by
/// `outbe-circuits`. Used both to size the combined-proof prefix here and
/// to type the `verify` callsite in `runtime.rs`.
pub const NUM_PUBLIC_INPUTS: usize = 4;

/// Verify an UltraHonkKeccak proof against the canonical commitment-nullifier
/// VK, binding the four runtime-known public inputs to the proof.
///
/// `public_inputs` are field elements in **circuit declaration order**:
/// `[merkle_root, nullifier_hash, denom_id, receiver_binding]` — the order
/// `fn main(...)` in the upstream Noir circuit declares them and the order
/// Noir / bb emit them in the proof. Each is encoded as a 32-byte
/// big-endian field element; `denom_id` is the BN254 Fr embedding of a small
/// integer, i.e. the 32-byte BE encoding with leading zeros.
///
/// `proof_body` is the bare proof bytes — the `bb prove` output with the
/// `[u32-BE count | N×32B public inputs]` prefix stripped off. This module
/// prepends a fresh prefix from `public_inputs` so the verifier sees exactly
/// what the runtime intends to gate.
///
/// Returns `false` on any verifier error or proof mismatch (the `Result` is
/// consumed locally so the runtime path stays simple — proof failure is just
/// "invalid proof").
#[cfg(not(any(test, feature = "test-helpers")))]
pub fn verify(public_inputs: &[U256; NUM_PUBLIC_INPUTS], proof_body: &[u8]) -> bool {
    let combined = build_combined(public_inputs, proof_body);
    outbe_zk_circuit_noir::barretenberg::verify::verify_ultra_honk_keccak(
        combined,
        COMMITMENT_NULLIFIER.vk_bytes.to_vec(),
        /* is_recursive = */ false,
    )
    .unwrap_or(false)
}

/// Build the Aztec "combined proof" blob: `[u32-BE num_public_inputs |
/// pub_in_0:32B | … | pub_in_{N-1}:32B | proof_body]`.
///
/// Mirrors `outbe_zk_circuit_noir::combine_proof`. Exposed inside the crate so
/// the encoding-round-trip parity test can re-parse it with the same
/// algorithm as `outbe_zk_circuit_noir::split_proof`.
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
pub fn verify(_public_inputs: &[U256; NUM_PUBLIC_INPUTS], _proof_body: &[u8]) -> bool {
    test_override::OUTCOME.with(|c| c.get().unwrap_or(false))
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
