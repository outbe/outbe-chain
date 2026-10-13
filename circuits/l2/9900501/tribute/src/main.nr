// Tribute claim circuit — chain 9900501 (Niflheim), claim `tribute`, version 1.0.0.
//
// This is pso-zk-circuits' `full_proof` 1.0.0, the circuit Niflheim wallets
// prove with, registered under the key L1 verifies with. Self-contained by
// registry rule: the `pso_circuit_core` helpers (`hash2`, `ownership`,
// `inclusion`) are inlined below unchanged, so this root compiles from its own
// source alone; the only dependency is the tag-pinned `noir-lang/schnorr`
// crate. The one edit is the name of the fourth public word,
// `expected_merkle_root` → `merkle_root`, which the claim ABI fixes; a
// parameter name is ABI only and does not move the ACIR or the key.
//
// The inclusion path is over the perpetual TributeDraft commitment tree: a
// depth-32 Poseidon2/BN254 IMT whose inner nodes are `Poseidon2(left, right)`
// with no domain separator. The leaf IS `nft_hash`.

use std::embedded_curve_ops::EmbeddedCurvePoint;

pub fn main(
    // Private witness.
    pk: EmbeddedCurvePoint,
    signature: [u8; 64],
    nonce: Field,
    // Depth-32 Merkle inclusion path for the perpetual TD commitment tree.
    merkle_path_siblings: [Field; 32],
    merkle_path_indices: [u8; 32],
    // Public: the (owner, nft_hash, binding_hash) ownership prefix plus the
    // Merkle root the inclusion check resolves to.
    owner: pub Field,
    nft_hash: pub Field,
    binding_hash: pub Field,
    merkle_root: pub Field,
) {
    // Part 1: sec.4.2 ownership proof. Owner commitment + Grumpkin Schnorr
    // over Poseidon3(nft_hash, nonce, binding_hash).
    verify_ownership_per_su(pk, signature, nonce, owner, nft_hash, binding_hash);

    // Part 2: Merkle inclusion of `nft_hash` against `merkle_root`.
    let computed_merkle_root =
        compute_merkle_root(nft_hash, merkle_path_siblings, merkle_path_indices);
    assert(
        computed_merkle_root == merkle_root,
        "merkle_root mismatch: inclusion proof failed",
    );
}

// ---- pso_circuit_core::hash2 ------------------------------------------------
// Poseidon2 fixed-arity hashes — a thin sponge over the stdlib
// `poseidon2_permutation`. `iv = len << 64` seeds the capacity, the rate is
// absorbed in blocks of 3, then a final permutation squeezes `state[0]`.

/// `Poseidon2(a, b)` — one rate block (2 < 3), single permutation.
fn hash_2(inputs: [Field; 2]) -> Field {
    let iv: Field = 36893488147419103232; // 2 << 64
    std::hash::poseidon2_permutation([inputs[0], inputs[1], 0, iv])[0]
}

/// `Poseidon2(a, b, c)` — a full rate block: absorb-permute, then squeeze-permute.
fn hash_3(inputs: [Field; 3]) -> Field {
    let iv: Field = 55340232221128654848; // 3 << 64
    let s1 = std::hash::poseidon2_permutation([inputs[0], inputs[1], inputs[2], iv]);
    std::hash::poseidon2_permutation(s1)[0]
}

// ---- pso_circuit_core::ownership --------------------------------------------

/// The Poseidon3 owner commitment from a Grumpkin public key and a nonce:
/// `Poseidon3(pk.x, pk.y, nonce)`.
fn compute_ownership(pk: EmbeddedCurvePoint, nonce: Field) -> Field {
    hash_3([pk.x, pk.y, nonce])
}

/// Grumpkin Schnorr over `message`; the signature is `s || e`, 64 bytes.
fn verify_schnorr_grumpkin(
    pk: EmbeddedCurvePoint,
    signature: [u8; 64],
    message: [u8; 32],
) -> bool {
    schnorr::verify_signature(pk, signature, message)
}

/// The sec.4.2 ownership-proof constraint set:
/// 0. `pk` is a valid, non-identity Grumpkin point;
/// 1. `owner == Poseidon3(pk.x, pk.y, nonce)`;
/// 2. Schnorr-verify(pk, signature, Poseidon3(nft_hash, nonce, binding_hash).to_be_bytes()).
///
/// `binding_hash` is folded as handed in and never recomputed here; the
/// verifier recomputes it from the caller, the draft id and both chain ids.
fn verify_ownership_per_su(
    pk: EmbeddedCurvePoint,
    signature: [u8; 64],
    nonce: Field,
    owner: Field,
    nft_hash: Field,
    binding_hash: Field,
) {
    // (0) Point validity: reject the identity and any off-curve (x, y)
    //     witness. Grumpkin's affine equation is `y^2 = x^3 - 17`.
    assert(!pk.is_infinite(), "pk is the Grumpkin identity point");
    let x_cubed = pk.x * pk.x * pk.x;
    assert(
        pk.y * pk.y == x_cubed - 17,
        "pk is not on the Grumpkin curve (y^2 != x^3 - 17)",
    );

    // (1) Owner commitment.
    let computed_owner = compute_ownership(pk, nonce);
    assert(
        computed_owner == owner,
        "owner mismatch: Poseidon3(pk, nonce) != public owner",
    );

    // (2) Signature binding over Poseidon3(nft_hash, nonce, binding_hash).
    let pre_hash_fr: Field = hash_3([nft_hash, nonce, binding_hash]);
    let pre_hash_bytes: [u8; 32] = pre_hash_fr.to_be_bytes();

    let signature_ok = verify_schnorr_grumpkin(pk, signature, pre_hash_bytes);
    assert(
        signature_ok,
        "signature mismatch: Schnorr/Grumpkin over Poseidon3(nft_hash, nonce, binding_hash) failed",
    );
}

// ---- pso_circuit_core::inclusion --------------------------------------------

/// Merkle root from a leaf and its path. `path_indices[i]` is 1 when the
/// current node is the left child at level `i`, 0 when it is the right one.
fn compute_merkle_root<let DEPTH: u32>(
    leaf: Field,
    path_siblings: [Field; DEPTH],
    path_indices: [u8; DEPTH],
) -> Field {
    let mut current_hash = leaf;

    for i in 0..DEPTH {
        let sibling = path_siblings[i];
        let is_left = path_indices[i];

        // Range-check the direction bit: any value other than 1 would fall
        // into the `else` branch and read as 0, a malleable path.
        assert((is_left == 0) | (is_left == 1), "path index must be 0 or 1");

        let (left, right) = if is_left == 1 {
            (current_hash, sibling)
        } else {
            (sibling, current_hash)
        };

        current_hash = hash_2([left, right])
    }

    current_hash
}
