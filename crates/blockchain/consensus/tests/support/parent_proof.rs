use commonware_codec::Encode;
use commonware_consensus::simplex::types::{Finalization, Notarization, Proposal};
use commonware_cryptography::{
    bls12381::primitives::variant::MinSig, sha256::Digest as Sha256Digest,
};
use outbe_consensus::hybrid::HybridScheme;
use outbe_consensus::proof::HybridCertificate;
use outbe_primitives::consensus_metadata::ParentParticipationProof;

pub(crate) fn encode_parent_proof(
    proposal: Proposal<Sha256Digest>,
    cert: &HybridCertificate<MinSig>,
    proof_kind: ParentParticipationProof,
) -> Vec<u8> {
    match proof_kind {
        ParentParticipationProof::Finalization => {
            Finalization::<HybridScheme<MinSig>, Sha256Digest> {
                proposal,
                certificate: cert.clone(),
            }
            .encode()
            .to_vec()
        }
        ParentParticipationProof::CertifiedNotarization => {
            Notarization::<HybridScheme<MinSig>, Sha256Digest> {
                proposal,
                certificate: cert.clone(),
            }
            .encode()
            .to_vec()
        }
    }
}
