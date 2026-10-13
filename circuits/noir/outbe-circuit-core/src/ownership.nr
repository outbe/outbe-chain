// Ownership proof helpers, per the privacy-preserving L2 architecture
// (PDF sec.2.2.3, sec.2.2.4, sec.4.2).
//
// Two layers:
//
//   1. Building blocks -- `compute_ownership`, `verify_schnorr_grumpkin`.
//      Pure helpers; reusable from any circuit.
//
//   2. `verify_ownership_per_su` -- the sec.4.2 ownership-proof
//      constraint set, factored out so the standalone
//      `outbe-ownership-circuit` and Demo Tribute circuit can call it
//      without duplication.
//
// ## Curve choice
//
// The public-key curve for in-circuit ownership is **Grumpkin** (Noir's
// "embedded curve" when the proving curve is BN254). Native arithmetic
// via the `multi_scalar_mul` foreign call -- no field emulation.
// Replaces the previous secp256k1 ECDSA flow, which cost ~42k
// constraints per signature verify due to non-native EC arithmetic.
// Grumpkin Schnorr is ~5-6k constraints per verify -- a ~7x reduction.
//
// The user's `sk_consent` stays on secp256k1 (unchanged user key
// material). ECDH off-chain is still secp256k1 between `sk_consent`
// and the SRA's ephemeral `sk_cu`. The HKDF output is then
// interpreted as a Grumpkin scalar (`shared_sk` mod `q_Grumpkin`)
// for in-circuit signing. See Appendix A of the spec.

use crate::hash;
use std::embedded_curve_ops::EmbeddedCurvePoint;

/// Compute the Poseidon3 owner commitment from a Grumpkin public key
/// and a nonce -- `Poseidon3(pk.x, pk.y, nonce)`. See PDF sec.2.2.3.
///
/// Grumpkin coordinates fit in a single Fr each (the base field is
/// BN254's scalar field), so the legacy 128-bit-limb decomposition
/// from the secp256k1 variant is gone.
pub fn compute_ownership(pk: EmbeddedCurvePoint, nonce: Field) -> Field {
    hash::hash_3([pk.x, pk.y, nonce])
}

/// Verify a Grumpkin Schnorr signature over `message`. Thin wrapper
/// around the `noir-lang/schnorr` crate (the same scheme Aztec uses
/// for its Schnorr account contracts).
///
/// The signature is 64 bytes: `s` (32-byte BE scalar) followed by
/// `e` (32-byte blake2s digest). Off-chain the wallet produces this
/// via `barretenberg-rs::api::schnorr_construct_signature`, which
/// uses the same Pedersen + blake2s challenge as the in-circuit
/// verifier here.
pub fn verify_schnorr_grumpkin(
    pk: EmbeddedCurvePoint,
    signature: [u8; 64],
    message: [u8; 32],
) -> bool {
    schnorr::verify_signature(pk, signature, message)
}

/// `verify_ownership_per_su` -- single-shot ownership-proof constraint
/// per sec.4.2 of the privacy-preserving L2 architecture.
///
/// ## Constraints
///
/// 0. `pk` is a valid, non-identity Grumpkin point -- rejects the
///    point at infinity and off-curve `(x, y)` witnesses before they
///    feed the owner commitment or the signature verify.
/// 1. `owner == Poseidon3(pk.x, pk.y, nonce)` -- proves the prover
///    knows a Grumpkin `(pk, nonce)` pair whose Poseidon commitment
///    equals the claimed `owner`.
/// 2. `Schnorr-verify(pk, signature, Poseidon3(nft_hash, nonce, binding_hash).to_be_bytes())`
///    -- proves the prover knows the Grumpkin secret key matching
///    `pk` and has signed a per-NFT message that binds the proof to a
///    specific `(nft_hash, nonce)` triple and the caller-supplied public
///    `binding_hash`. Its construction is intentionally outside this generic
///    ownership circuit; the runtime that owns the statement must recompute
///    and validate it.
///
/// ## Inputs
///
/// All private to the prover except `owner`, `nft_hash`, and
/// `binding_hash`, which are public so the verifier (on-chain or
/// otherwise) can supply the expected values:
///
/// - `pk`: Grumpkin public key. For SUs this is the wallet's per-SU
///   `shared_pk` derived via App. A; for TDs this is a fresh per-TD
///   Grumpkin public key.
/// - `signature`: 64-byte Grumpkin Schnorr signature (`s || e`).
/// - `nonce`: per-NFT nonce.
/// - `owner` (public): claimed Poseidon3 commitment from the NFT's
///   on-chain `derivedOwner` field.
/// - `nft_hash` (public): per-NFT entity hash.
/// - `binding_hash` (public): opaque application context. The verifier must
///   recompute it from the runtime context it intends to bind.
pub fn verify_ownership_per_su(
    pk: EmbeddedCurvePoint,
    signature: [u8; 64],
    nonce: Field,
    owner: Field,
    nft_hash: Field,
    binding_hash: Field,
) {
    // (0) Point validity. Reject the identity and any off-curve (x, y)
    //     witness before it reaches the Poseidon commitment or the
    //     scalar mul inside the Schnorr verify. Grumpkin's affine
    //     equation is `y^2 = x^3 - 17`.
    assert(!pk.is_infinite(), "pk is the Grumpkin identity point");
    let x_cubed = pk.x * pk.x * pk.x;
    assert(
        pk.y * pk.y == x_cubed - 17,
        "pk is not on the Grumpkin curve (y^2 != x^3 - 17)",
    );

    // (1) Owner commitment. Binds `pk`+`nonce` to the SU's stored
    //     `derivedOwner`; deliberately independent of `binding_hash`
    //     (the owner is fixed at SU mint, before any TD exists).
    let computed_owner = compute_ownership(pk, nonce);
    assert(
        computed_owner == owner,
        "owner mismatch: Poseidon3(pk, nonce) != public owner",
    );

    // (2) Signature binding: Schnorr/Grumpkin over Poseidon3(nft_hash,
    //     nonce, binding_hash).to_be_bytes(). The pre-hash binds the
    //     proof to the NFT data, the per-NFT nonce, AND the submission
    //     binding; a replay across NFTs *or* across a different
    //     (sender, tdId, chainId) produces a different pre-hash and
    //     fails verification.
    let pre_hash_fr: Field = hash::hash_3([nft_hash, nonce, binding_hash]);
    let pre_hash_bytes: [u8; 32] = pre_hash_fr.to_be_bytes();

    let signature_ok = verify_schnorr_grumpkin(pk, signature, pre_hash_bytes);
    assert(
        signature_ok,
        "signature mismatch: Schnorr/Grumpkin over Poseidon3(nft_hash, nonce, binding_hash) failed",
    );
}
