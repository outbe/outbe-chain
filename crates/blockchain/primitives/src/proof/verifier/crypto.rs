//! Structural quorum, aggregate vote, threshold VRF and signer bitmap rules.
use super::{
    simplex_n3f1_quorum, CommitteeSnapshotView, HybridCertificate, V2VerifyError, VerifiedProof,
    VoteBinding, VrfProof,
};
use commonware_cryptography::bls12381::primitives::{
    ops::aggregate,
    variant::{MinPk, MinSig, Variant},
};
use commonware_utils::Participant;

pub(super) fn verify_v2_certificate_low_level(
    snapshot: &CommitteeSnapshotView<'_>,
    binding: &VoteBinding<'_>,
    cert: &HybridCertificate<MinSig>,
) -> Result<VerifiedProof, V2VerifyError> {
    let participants_len = snapshot.participants.len();

    if cert.signers.len() != participants_len {
        return Err(V2VerifyError::BitmapMismatch {
            reason: "bitmap length does not match committee size",
        });
    }

    let quorum = simplex_n3f1_quorum(participants_len);
    let signer_count = cert.signers.count();
    if signer_count < quorum {
        return Err(V2VerifyError::BelowQuorum {
            signers: signer_count,
            quorum,
        });
    }

    verify_aggregate_vote(snapshot, binding, cert, signer_count)?;
    let proof = &cert.vrf_proof;
    verify_threshold_vrf_proof(&snapshot.vrf_group_public_key, binding.seed_message, proof)?;

    let signer_bitmap = reconstruct_signer_bitmap(cert, participants_len)?;
    let vrf_proof_hash = crate::proof::canonical_vrf_proof_hash_v2(proof);

    Ok(VerifiedProof {
        signer_bitmap,
        vrf_proof_hash,
        prev_randao: proof.prev_randao(),
        vrf_material_version: proof.material_version,
    })
}

fn verify_aggregate_vote(
    snapshot: &CommitteeSnapshotView<'_>,
    binding: &VoteBinding<'_>,
    cert: &HybridCertificate<MinSig>,
    signer_count: usize,
) -> Result<(), V2VerifyError> {
    let signer_pubkeys: Vec<&<MinPk as Variant>::Public> = cert
        .signers
        .iter()
        .filter_map(|signer: Participant| {
            let idx = signer.get() as usize;
            snapshot.participants.get(idx).map(AsRef::as_ref)
        })
        .collect();
    if signer_pubkeys.len() != signer_count {
        return Err(V2VerifyError::SignerIndexOutOfRange {
            index: 0,
            committee_size: snapshot.participants.len(),
        });
    }
    let aggregate_pk = aggregate::combine_public_keys::<MinPk, _>(
        commonware_utils::iter::NonEmpty::try_new(signer_pubkeys.into_iter())
            .ok_or(V2VerifyError::BlsAggregateInvalid)?,
    );
    aggregate::verify_same_message::<MinPk>(
        &aggregate_pk,
        binding.namespace,
        binding.message,
        &cert.bls_aggregated_vote,
    )
    .map_err(|_| V2VerifyError::BlsAggregateInvalid)?;

    Ok(())
}

fn reconstruct_signer_bitmap(
    cert: &HybridCertificate<MinSig>,
    participants_len: usize,
) -> Result<Vec<u8>, V2VerifyError> {
    let mut signer_bitmap = vec![0u8; participants_len];
    for signer in cert.signers.iter() {
        let signer: Participant = signer;
        let idx = signer.get() as usize;
        if idx >= participants_len {
            return Err(V2VerifyError::SignerIndexOutOfRange {
                index: idx as u32,
                committee_size: participants_len,
            });
        }
        signer_bitmap[idx] = 1;
    }

    Ok(signer_bitmap)
}

fn verify_threshold_vrf_proof(
    group_pk: &<MinSig as Variant>::Public,
    seed_message: &[u8],
    proof: &VrfProof<MinSig>,
) -> Result<(), V2VerifyError> {
    // Plain-pairing core shared with the slashing path (`seed_partial`). It uses
    // no RNG, so the gate's Result is byte-deterministic across every validator.
    if crate::proof::verify_seed_signature_plain(group_pk, seed_message, &proof.threshold_signature)
    {
        Ok(())
    } else {
        Err(V2VerifyError::InvalidVrfSignature)
    }
}
