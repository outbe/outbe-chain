//! Decode proof identity before building the canonical vote and seed messages.
use super::{
    verify_v2_certificate_low_level, CommitteeSnapshotView, V2VerifyError, VerifiedProof,
    VoteBinding, VoteSubject,
};
use crate::{
    digest::Digest as OutbeDigest,
    hybrid::HybridScheme,
    proof::{
        committee_keys::committee_ordered_set,
        constants::{finalize_namespace, notarize_namespace},
    },
};
use commonware_codec::{Encode, Read};
use commonware_consensus::simplex::types::{Finalization, Notarization, Proposal};
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use outbe_primitives::consensus_metadata::{
    CertifiedParentAccountingMetadata, ParentParticipationProof,
};

struct DecodedProof {
    subject: VoteSubject,
    proposal: Proposal<OutbeDigest>,
    cert: super::HybridCertificate<MinSig>,
}

pub(super) fn verify(
    view: &CommitteeSnapshotView<'_>,
    metadata: &CertifiedParentAccountingMetadata,
    participants_len: usize,
) -> Result<VerifiedProof, V2VerifyError> {
    let DecodedProof {
        subject,
        proposal,
        cert,
    } = decode_proof(metadata, participants_len)?;
    // Vote namespaces bind the ordered committee (same sorted/deduped order as
    // the signer's participant set). Thus these bytes equal what the signer used.
    let committee_set = committee_ordered_set(view.participants);
    let namespace = match subject {
        VoteSubject::Finalize => finalize_namespace(&committee_set),
        VoteSubject::Notarize => notarize_namespace(&committee_set),
    };

    let round = proposal.round;
    let seed_message = round.encode().to_vec();
    let message = proposal.encode().to_vec();

    let binding = VoteBinding {
        subject,
        namespace: &namespace,
        message: &message,
        seed_message: &seed_message,
    };

    // Rule 9 - delegate to the low-level structural + crypto verifier.
    let inner = verify_v2_certificate_low_level(view, &binding, &cert)?;

    Ok(inner)
}

fn decode_proof(
    metadata: &CertifiedParentAccountingMetadata,
    participants_len: usize,
) -> Result<DecodedProof, V2VerifyError> {
    let mut proof_reader = metadata.proof.as_ref();
    let (subject, proposal, cert) = match metadata.proof_kind {
        ParentParticipationProof::Finalization => {
            let proof: Finalization<HybridScheme<MinSig>, OutbeDigest> =
                read_proof(&mut proof_reader, participants_len)?;
            (VoteSubject::Finalize, proof.proposal, proof.certificate)
        }
        ParentParticipationProof::CertifiedNotarization => {
            let proof: Notarization<HybridScheme<MinSig>, OutbeDigest> =
                read_proof(&mut proof_reader, participants_len)?;
            (VoteSubject::Notarize, proof.proposal, proof.certificate)
        }
    };
    if !proof_reader.is_empty() {
        return Err(V2VerifyError::TrailingBytes);
    }

    if !proposal_matches_metadata(&proposal, metadata) {
        return Err(V2VerifyError::WrongProofDomain {
            expected: metadata.finalized_block_hash,
            actual: proposal.payload.0,
        });
    }

    Ok(DecodedProof {
        subject,
        proposal,
        cert,
    })
}

fn proposal_matches_metadata(
    proposal: &Proposal<OutbeDigest>,
    metadata: &CertifiedParentAccountingMetadata,
) -> bool {
    proposal.round.epoch().get() == metadata.finalized_epoch
        && proposal.round.view().get() == metadata.finalized_view
        && proposal.parent.get() == metadata.parent_view
        && proposal.payload.0 == metadata.finalized_block_hash
}

fn read_proof<P: Read<Cfg = usize>>(
    reader: &mut &[u8],
    participants_len: usize,
) -> Result<P, V2VerifyError> {
    P::read_cfg(reader, &participants_len).map_err(V2VerifyError::Decode)
}

/// Decode randomness from the selected local parent-proof record for proposal
/// construction. This is not proof verification: the shared EVM preflight
/// authenticates the certificate and checks the resulting header value.
pub(crate) fn decode_parent_prev_randao(
    metadata: &CertifiedParentAccountingMetadata,
) -> Result<alloy_primitives::B256, V2VerifyError> {
    let proof = decode_proof(metadata, metadata.ordered_committee.len())?;
    Ok(proof.cert.vrf_proof.prev_randao())
}
