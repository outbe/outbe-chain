//! Certified-parent accounting metadata fixtures over a committee snapshot.

use alloy_primitives::{keccak256, Address, Bytes, B256};
use outbe_primitives::consensus_metadata::{
    CertifiedParentAccountingMetadata, ParentParticipationProof,
};

use crate::proof::{committee_set_hash_v2, CommitteeSnapshot};

/// The finalized parent that a metadata fixture describes.
#[derive(Clone, Copy, Debug)]
pub struct TestFinalizedParent {
    pub block_number: u64,
    pub block_hash: B256,
    pub epoch: u64,
    pub view: u64,
    pub parent_view: u64,
    pub vrf_material_version: u64,
    pub proof_kind: ParentParticipationProof,
}

/// Metadata for `parent` in which every member of `snapshot` signed, in
/// committee order, with `proof` as the encoded certificate.
pub fn test_fully_signed_metadata(
    parent: &TestFinalizedParent,
    snapshot: &CommitteeSnapshot,
    proof: &[u8],
) -> CertifiedParentAccountingMetadata {
    let ordered_committee: Vec<Address> = snapshot
        .committee
        .iter()
        .map(|entry| entry.address)
        .collect();
    let signer_bitmap = vec![1u8; snapshot.committee.len()];
    let committee_set_hash = committee_set_hash_v2(parent.epoch, snapshot);
    let vrf_group_public_key_hash = keccak256(&snapshot.vrf_group_public_key_bytes);
    CertifiedParentAccountingMetadata {
        finalized_block_number: parent.block_number,
        finalized_block_hash: parent.block_hash,
        finalized_epoch: parent.epoch,
        finalized_view: parent.view,
        parent_view: parent.parent_view,
        ordered_committee,
        signer_bitmap,
        proof: Bytes::copy_from_slice(proof),
        committee_set_hash,
        vrf_material_version: parent.vrf_material_version,
        vrf_group_public_key_hash,
        proof_kind: parent.proof_kind,
        missed_proposers: Vec::new(),
    }
}
