//! Witness builder for the externally owned FullProof circuit.

use ark_bn254::Fr;
use ark_ff::Zero;
use outbe_protocol::error::Error;
use outbe_protocol::primitive::hash::FieldHasher;
use outbe_protocol::protocol::key::NftSigner;
use outbe_protocol::{OutbeV1, Suite};
use outbe_zk_canonical::noir::full_proof::{PublicInputs, Witness};
use outbe_zk_canonical::ownership::Provable;
use rand::Rng;

const FULL_PROOF_TREE_DEPTH: usize = 32;

/// Build the external FullProof witness for one entity at leaf zero of an
/// otherwise empty depth-32 tree.
///
/// This is the only FullProof shape used by Tribute offer construction and its
/// chain-side fixtures. Path index `1` means the running node is the left child,
/// matching the external circuit ABI.
pub fn derive_single_leaf_full_proof_witness<T, R, K>(
    entity: &T,
    rng: &mut R,
    signer: &K,
    binding: Fr,
) -> Result<(Witness, PublicInputs), Error>
where
    T: Provable<OutbeV1>,
    R: Rng,
    K: NftSigner<OutbeV1>,
{
    let (ownership_witness, ownership_public) =
        entity.derive_ownership_witness(rng, signer, binding)?;

    let mut merkle_path_siblings = [Fr::zero(); FULL_PROOF_TREE_DEPTH];
    let merkle_path_indices = [1u8; FULL_PROOF_TREE_DEPTH];
    let mut zero = Fr::zero();
    let mut expected_merkle_root = ownership_public.nft_hash;
    for sibling in &mut merkle_path_siblings {
        *sibling = zero;
        expected_merkle_root =
            <<OutbeV1 as Suite>::Hash as FieldHasher<Fr>>::hash(&[expected_merkle_root, zero])?;
        zero = <<OutbeV1 as Suite>::Hash as FieldHasher<Fr>>::hash(&[zero, zero])?;
    }

    Ok((
        Witness {
            pk: ownership_witness.pk,
            signature: ownership_witness.signature,
            nonce: ownership_witness.nonce,
            merkle_path_siblings,
            merkle_path_indices,
        },
        PublicInputs {
            owner: ownership_public.owner,
            nft_hash: ownership_public.nft_hash,
            binding_hash: ownership_public.binding_hash,
            expected_merkle_root,
        },
    ))
}
