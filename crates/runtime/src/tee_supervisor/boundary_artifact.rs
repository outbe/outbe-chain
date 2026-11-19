use alloy_primitives::{Bytes, B256};
use outbe_consensus::finalized_admission_test_utils::FinalityCommitteeFixture;
use outbe_primitives::{
    consensus::{DkgBoundaryArtifact, ReshareResult},
    reshare_artifact::{
        encode_outbe_block_artifacts, tee_expired_target_exclusions_hash, ConsensusHeaderArtifact,
        OutbeBlockArtifacts,
    },
};

pub(super) fn boundary_extra(committee: &FinalityCommitteeFixture) -> Vec<u8> {
    let artifact = DkgBoundaryArtifact {
        epoch: 0,
        dkg_cycle: 0,
        freeze_height: 0,
        planned_activation_height: 1,
        target_set_hash: B256::ZERO,
        vrf_material_version: 0,
        vrf_group_public_key: B256::ZERO,
        vrf_group_public_key_bytes: Bytes::new(),
        committee_set_hash: B256::ZERO,
        is_validator_set_change: false,
        outcome: Bytes::from(committee.outcome(commonware_consensus::types::Epoch::new(0))),
        is_full_dkg: true,
        reshare: ReshareResult {
            new_active_set: Vec::new(),
            active_set_hash: B256::ZERO,
        },
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: tee_expired_target_exclusions_hash(&[]).unwrap(),
    };
    encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)),
        ..Default::default()
    })
    .unwrap()
    .to_vec()
}
