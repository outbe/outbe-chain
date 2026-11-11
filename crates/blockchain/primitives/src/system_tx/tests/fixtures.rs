use super::*;

pub(super) const CHAIN_ID: u64 = 2026;

pub(super) fn sample_metadata() -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata {
        finalized_block_number: 41,
        finalized_block_hash: B256::repeat_byte(0x41),
        finalized_epoch: 7,
        finalized_view: 42,
        parent_view: 41,
        ordered_committee: vec![address!("0x1111111111111111111111111111111111111111")],
        signer_bitmap: vec![1],
        proof: Bytes::from_static(b"cert"),
        committee_set_hash: B256::repeat_byte(0x77),
        vrf_material_version: 3,
        vrf_group_public_key_hash: B256::repeat_byte(0x88),
        proof_kind: crate::consensus_metadata::ParentParticipationProof::Finalization,
        missed_proposers: Vec::new(),
    }
}

fn sample_boundary() -> DkgBoundaryArtifact {
    crate::test_utils::sample_system_tx_boundary(42)
}

pub(super) fn consensus(step_index: u8, payload: Bytes) -> SystemTx {
    SystemTx::Consensus {
        step_index,
        payload,
    }
}
pub(super) fn accounting() -> SystemTx {
    consensus(0, sample_metadata().encode().unwrap())
}
pub(super) fn late_credits() -> SystemTx {
    consensus(
        1,
        crate::reshare_artifact::encode_late_finalize_credits_artifact(
            &LateFinalizeCreditsArtifact::default(),
        )
        .unwrap(),
    )
}
pub(super) fn boundary() -> SystemTx {
    consensus(
        2,
        crate::reshare_artifact::encode_boundary_artifact(&sample_boundary()).unwrap(),
    )
}
pub(super) fn bootstrap() -> SystemTx {
    consensus(
        3,
        crate::tee_bootstrap_v2::codec::encode_canonical(
            &crate::test_utils::sample_system_tx_tee_bootstrap(),
        )
        .unwrap(),
    )
}
pub(super) fn publication() -> SystemTx {
    consensus(
        5,
        Bytes::from(
            crate::test_utils::sample_radicle_publication_batch(1)
                .to_bytes()
                .unwrap(),
        ),
    )
}
pub(super) fn begin_stage_inputs() -> [SystemTx; 9] {
    std::array::from_fn(|index| SystemTx::BeginBlock {
        step_index: u8::try_from(index).unwrap(),
    })
}
pub(super) fn end_stage_inputs() -> [SystemTx; 5] {
    std::array::from_fn(|index| SystemTx::EndBlock {
        step_index: u8::try_from(index).unwrap(),
    })
}
pub(super) fn inputs() -> Vec<SystemTx> {
    let mut inputs = vec![
        accounting(),
        late_credits(),
        boundary(),
        bootstrap(),
        consensus(4, Bytes::new()),
        publication(),
    ];
    inputs.extend(begin_stage_inputs());
    inputs.extend(end_stage_inputs());
    inputs
}
