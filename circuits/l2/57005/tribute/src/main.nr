// Demo Tribute Circuit.
//
// Composes the same sec.4.2 ownership constraint as `outbe-ownership-circuit`
// with a Merkle-inclusion check against the perpetual TributeDraft commitment
// tree (the depth-32 Poseidon2/BN254 IMT defined by `CommitmentWindowBase` in
// outbe-chain-research). Every inner node prepends `DEMO_TRIBUTE_DOMAIN`; the
// leaf is `nft_hash` itself, so the inclusion proof binds the same entity hash
// checked by the ownership constraint.

use outbe_circuit_core::merkle_tree;
use outbe_circuit_core::ownership;
use std::embedded_curve_ops::EmbeddedCurvePoint;

// Preserve the historical domain bytes so renaming does not change circuit identity.
global DEMO_TRIBUTE_DOMAIN: Field = 0x4f555442455f46554c4c5f43495243554954; // "OUTBE_FULL_CIRCUIT"

pub fn main(
    // Private witness.
    pk: EmbeddedCurvePoint,
    signature: [u8; 64],
    nonce: Field,
    // Depth-32 Merkle inclusion path for the perpetual TD commitment tree.
    merkle_path_siblings: [Field; 32],
    merkle_path_indices: [bool; 32],
    // Public: the (derived_owner, nft_hash, binding_hash) ownership prefix plus the
    // Merkle root the inclusion check resolves to.
    derived_owner: pub Field,
    nft_hash: pub Field,
    binding_hash: pub Field,
    merkle_root: pub Field,
) {
    // Part 1: sec.4.2 ownership proof. Owner commitment + Grumpkin Schnorr
    // over Poseidon3(nft_hash, nonce, binding_hash). Same code path the
    // standalone ownership circuit uses.
    ownership::verify_ownership_per_su(pk, signature, nonce, derived_owner, nft_hash, binding_hash);

    // Part 2: Merkle inclusion of `nft_hash` against `merkle_root`.
    let computed_merkle_root = merkle_tree::merkle_root_from_indices(
        DEMO_TRIBUTE_DOMAIN,
        nft_hash,
        merkle_path_siblings,
        merkle_path_indices,
    );
    assert(
        computed_merkle_root == merkle_root,
        "merkle_root mismatch: inclusion proof failed",
    );
}
