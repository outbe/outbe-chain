//! Demo offer (§4.2 ownership + Merkle inclusion) combined-proof layout,
//! public-input codecs, and witness builder, wired to the build-generated
//! `demo_offer` noir circuit.
//!
//! This is the former `full_proof` circuit renamed to Demo offer: the
//! witness/public-input ABI and every algorithm below are unchanged, so the
//! frozen artifacts, hashes and VKs still apply.
//!
//! The witness builder ([`DemoOfferProvable::derive_demo_offer_witness`])
//! extends the ownership witness with a depth-[`INCLUSION_DEPTH`]
//! inclusion path against the perpetual OfferDraft commitment tree (the
//! [`Imt`](crate::protocol::imt::Imt) in the core). The
//! leaf is the entity's `nft_hash` itself — matching the on-chain tree, which
//! stores the entity hash directly — so the inclusion is over the same value
//! the ownership constraint binds.

use ark_ff::PrimeField;
use ark_std::rand::Rng;

use crate::error::Error;
use crate::errors::MerkleError;
use crate::primitive::curve::coords;
use crate::primitive::hash;
use crate::protocol::entity::{Entity, Owned};
use crate::protocol::imt::InclusionPath;
use crate::protocol::key::NftSigner;
use crate::protocol::zk::ProofGenerator;
use crate::Fr;

#[cfg(feature = "alloy")]
pub use crate::noir::demo_offer::alloy;
pub use crate::noir::demo_offer::{
    decode_public_inputs, encode_combined_proof, PublicInputs, COMBINED_LEN, PROOF_WORDS,
    PUBLIC_INPUT_COUNT,
};

use crate::noir::demo_offer::{DemoOffer, Witness};
use crate::noir::EmbeddedCurvePoint;
use crate::INCLUSION_DEPTH;

/// Domain prepended to every Demo offer commitment-tree inner-node hash.
///
/// The bytes are deliberately the legacy `OUTBE_FULL_CIRCUIT` literal: they are
/// the field value baked into the frozen circuit
/// (`0x4f555442455f46554c4c5f43495243554954`), so
/// renaming the circuit renamed this symbol only. Editing the bytes would mint a
/// different circuit identity and invalidate every released VK and proof.
pub const DEMO_OFFER_DOMAIN: &[u8; 18] = b"OUTBE_FULL_CIRCUIT";

/// Demo offer commitment-tree domain as the canonical circuit field.
pub fn demo_offer_domain() -> Fr {
    Fr::from_be_bytes_mod_order(DEMO_OFFER_DOMAIN)
}

/// NFT extension: an owned entity can build the Demo offer witness (ownership
/// and inclusion) and, given a backend, a proof. Blanket-implemented for every
/// owned entity.
pub trait DemoOfferProvable: Entity + Owned {
    /// Build + sign the Demo offer witness: the ownership witness plus the
    /// Merkle inclusion `path` proving `nft_hash` sits in the commitment tree.
    /// The path must use [`DEMO_OFFER_DOMAIN`] and have
    /// [`INCLUSION_DEPTH`] levels. The
    /// `merkle_root` public input is recomputed from the path.
    fn derive_demo_offer_witness<R, K>(
        &self,
        rng: &mut R,
        signer: &K,
        binding: Fr,
        path: &InclusionPath,
    ) -> Result<(Witness, PublicInputs), Error>
    where
        R: Rng,
        K: NftSigner,
    {
        if path.depth() != INCLUSION_DEPTH {
            return Err(MerkleError::InclusionPathDepthMismatch {
                expected: INCLUSION_DEPTH,
                actual: path.depth(),
            }
            .into());
        }
        if path.domain != demo_offer_domain() {
            return Err(MerkleError::WrongInclusionPathDomain.into());
        }

        // Ownership: recompute owner from (pk, nonce), reject a mismatch, sign
        // the §4.2 payload — the same constraint the circuit enforces.
        let seed = signer.owner_seed();
        let derived_owner = hash::derive_owner(&seed.pk, seed.nonce)?;
        if derived_owner != self.owner()? {
            return Err(Error::OwnerMismatch);
        }
        let nft_hash = self.entity_hash()?;
        let payload = hash::signing_payload(nft_hash, seed.nonce, binding)?;
        let signature = signer.sign(rng, payload)?;
        let (x, y) = coords(&seed.pk)?;

        // Inclusion: the leaf IS `nft_hash` (the on-chain tree stores the entity
        // hash directly). Recompute the root the circuit will check against.
        let merkle_root = path.root(nft_hash)?;

        let merkle_path_siblings: [Fr; 32] = path
            .siblings
            .as_slice()
            .try_into()
            .map_err(|_| MerkleError::SiblingsLength)?;
        let merkle_path_indices: [bool; 32] = path
            .circuit_indices()?
            .try_into()
            .map_err(|_| MerkleError::IndicesLength)?;

        let witness = Witness {
            pk: EmbeddedCurvePoint { x, y },
            signature,
            nonce: seed.nonce,
            merkle_path_siblings,
            merkle_path_indices,
        };
        let public = PublicInputs {
            derived_owner,
            nft_hash,
            binding_hash: binding,
            merkle_root,
        };
        Ok((witness, public))
    }

    /// Build the Demo offer witness and hand it to a ZK proof-generation backend.
    fn generate_demo_offer_proof<R, K, G>(
        &self,
        rng: &mut R,
        signer: &K,
        binding: Fr,
        path: &InclusionPath,
        generator: &G,
    ) -> Result<G::Proof, Error>
    where
        R: Rng,
        K: NftSigner,
        G: ProofGenerator<DemoOffer>,
    {
        let (witness, public) = self.derive_demo_offer_witness(rng, signer, binding, path)?;
        generator.generate(&witness, &public)
    }
}

impl<E: Entity + Owned + ?Sized> DemoOfferProvable for E {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    #[test]
    fn generated_inputs_preserve_demo_offer_names_and_order() {
        let fields = [11u64, 22, 33, 44].map(Fr::from);
        let mut proof = (PUBLIC_INPUT_COUNT as u32).to_be_bytes().to_vec();
        for field in fields {
            proof.extend_from_slice(&codec::field_to_be_bytes(&field));
        }
        proof.resize(COMBINED_LEN, 0);
        let public = PublicInputs {
            derived_owner: fields[0],
            nft_hash: fields[1],
            binding_hash: fields[2],
            merkle_root: fields[3],
        };
        assert_eq!(decode_public_inputs(&proof).unwrap(), public);
        assert_eq!(
            encode_combined_proof(public, vec![vec![0; 32]; PROOF_WORDS]).unwrap(),
            proof
        );
    }
}
