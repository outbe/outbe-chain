//! Ordered metadata and verifier-result bindings.
use super::{CommitteeSnapshot, V2VerifyError, VerifiedProof};
use crate::proof::committee_set_hash_v2;
use alloy_primitives::{keccak256, B256};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use std::collections::BTreeSet;

pub(super) fn validate_metadata(
    metadata: &CertifiedParentAccountingMetadata,
    snapshot: &CommitteeSnapshot,
    proof_bytes: &[u8],
    header_parent_hash: B256,
) -> Result<(), V2VerifyError> {
    validate_parent_binding(metadata, header_parent_hash)?;
    validate_committee_binding(metadata, snapshot)?;
    validate_vrf_binding(metadata, snapshot)?;
    // Rule 8 - proof bytes equal metadata.proof byte-identically.
    if proof_bytes != metadata.proof.as_ref() {
        return Err(V2VerifyError::WrongProofDomain {
            expected: metadata.finalized_block_hash,
            actual: B256::ZERO,
        });
    }

    Ok(())
}

fn validate_parent_binding(
    metadata: &CertifiedParentAccountingMetadata,
    header_parent_hash: B256,
) -> Result<(), V2VerifyError> {
    // Rule 1 - missed_proposers MUST be empty in V2, ALWAYS, BEFORE any
    // other check. This rejects pre-mutation, applies to both proof kinds.
    if !metadata.missed_proposers.is_empty() {
        return Err(V2VerifyError::NonEmptyMissedProposers {
            count: metadata.missed_proposers.len(),
        });
    }

    // Rule 2 - exact-parent binding. The metadata MUST target the
    // immediate parent of the block under verification.
    if metadata.finalized_block_hash != header_parent_hash {
        return Err(V2VerifyError::WrongAccountedHash {
            expected: header_parent_hash,
            actual: metadata.finalized_block_hash,
        });
    }

    Ok(())
}

fn validate_committee_binding(
    metadata: &CertifiedParentAccountingMetadata,
    snapshot: &CommitteeSnapshot,
) -> Result<(), V2VerifyError> {
    // Rule 3 - committee shape: metadata vs snapshot must agree on size
    // AND per-position address. Disagreement is either a snapshot lookup error
    // by the caller or a malicious metadata.
    if snapshot.committee.is_empty() {
        return Err(V2VerifyError::CommitteeSnapshotMissing);
    }
    if metadata.ordered_committee.len() != snapshot.committee.len() {
        return Err(V2VerifyError::BitmapMismatch {
            reason: "metadata.ordered_committee length differs from snapshot.committee length",
        });
    }
    for (meta_addr, snap_entry) in metadata
        .ordered_committee
        .iter()
        .zip(snapshot.committee.iter())
    {
        if *meta_addr != snap_entry.address {
            // Metadata cannot override consensus pubkeys or participant ordering.
            return Err(V2VerifyError::BitmapMismatch {
                reason: "metadata.ordered_committee[i].address differs from snapshot.committee[i].address",
            });
        }
    }

    // Rule 4 - bitmap shape: length must match committee.
    if metadata.signer_bitmap.len() != metadata.ordered_committee.len() {
        return Err(V2VerifyError::BitmapMismatch {
            reason: "metadata.signer_bitmap length differs from ordered_committee length",
        });
    }

    Ok(())
}

fn validate_vrf_binding(
    metadata: &CertifiedParentAccountingMetadata,
    snapshot: &CommitteeSnapshot,
) -> Result<(), V2VerifyError> {
    // Rule 5 - VRF material version binding (metadata == snapshot).
    validate_material_version(snapshot.vrf_material_version, metadata.vrf_material_version)?;

    // Rule 6 - VRF group public key hash binding.
    let snapshot_group_pk_hash = keccak256(&snapshot.vrf_group_public_key_bytes);
    if metadata.vrf_group_public_key_hash != snapshot_group_pk_hash {
        return Err(V2VerifyError::WrongVrfGroupKeyHash {
            expected: snapshot_group_pk_hash,
            actual: metadata.vrf_group_public_key_hash,
        });
    }

    // Rule 7 - canonical committee_set_hash fingerprint binding.
    let canonical_committee_set_hash = committee_set_hash_v2(metadata.finalized_epoch, snapshot);
    if metadata.committee_set_hash != canonical_committee_set_hash {
        return Err(V2VerifyError::CommitteeSetHashMismatch {
            expected: canonical_committee_set_hash,
            actual: metadata.committee_set_hash,
        });
    }

    Ok(())
}

pub(super) fn validate_result(
    metadata: &CertifiedParentAccountingMetadata,
    inner: &VerifiedProof,
) -> Result<(), V2VerifyError> {
    // Rule 5 cross-check: cert's VRF material version must also equal the
    // metadata's (the inner verifier returns it directly from the proof).
    validate_material_version(metadata.vrf_material_version, inner.vrf_material_version)?;

    // Rule 8 cross-check: bitmap reconciliation - metadata's bitmap must
    // exactly equal the reconstructed bitmap from the certificate.
    if inner.signer_bitmap != metadata.signer_bitmap {
        return Err(V2VerifyError::BitmapMismatch {
            reason: "metadata.signer_bitmap differs from certificate-reconstructed bitmap",
        });
    }

    // Duplicate-signer / out-of-range checks are enforced by the inner
    // decoder and the bitmap reconstruction loop already; the BTreeSet check
    // here is defence in depth in case the inner decoder ever stops doing it.
    let mut seen = BTreeSet::new();
    for (idx, byte) in metadata.signer_bitmap.iter().enumerate() {
        if *byte == 0 {
            continue;
        }
        if *byte != 1 {
            return Err(V2VerifyError::BitmapMismatch {
                reason: "metadata.signer_bitmap has non-binary byte",
            });
        }
        let index = idx as u32;
        if !seen.insert(index) {
            return Err(V2VerifyError::DuplicateSigner { index });
        }
    }

    Ok(())
}

fn validate_material_version(expected: u64, actual: u64) -> Result<(), V2VerifyError> {
    if actual != expected {
        return Err(V2VerifyError::WrongVrfMaterialVersion { expected, actual });
    }
    Ok(())
}
