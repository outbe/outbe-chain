//! Shared inputs and original-code diagnostics for the binding regression tests.
#![cfg(test)]
use super::primitives::{
    addresses::{OUTBE_SYSTEM_TX_ADDRESS, REWARDS_ADDRESS},
    consensus::{DkgBoundaryArtifact, ReshareResult},
    consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
    reshare_artifact::{
        encode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
    },
    system_tx::{SystemTx, SystemTxLayout},
    OutbeBlockBody, OutbeHeader,
};
use alloy_consensus::{Header, SignableTransaction, TxLegacy};
use alloy_primitives::{Address, Bytes, Signature, TxKind, B256, U256};
use reth_ethereum_primitives::TransactionSigned;

pub(super) struct Case {
    pub name: &'static str,
    pub header: OutbeHeader,
    pub body: OutbeBlockBody,
    pub artifacts: OutbeBlockArtifacts,
}

impl Case {
    pub fn layout(&self) -> SystemTxLayout<'_> {
        SystemTxLayout {
            begin: self.body.transactions.iter().collect(),
            user: vec![],
            end: vec![],
        }
    }

    fn new(name: &'static str, height: u64, inputs: Vec<Bytes>) -> Self {
        Self {
            name,
            header: OutbeHeader::new(Header {
                number: height,
                parent_hash: B256::repeat_byte(0x41),
                beneficiary: REWARDS_ADDRESS,
                ..Default::default()
            }),
            body: OutbeBlockBody {
                transactions: inputs.into_iter().map(transaction).collect(),
                ..Default::default()
            },
            artifacts: OutbeBlockArtifacts::default(),
        }
    }
}

fn transaction(input: Bytes) -> TransactionSigned {
    TxLegacy {
        chain_id: Some(2026),
        nonce: 0,
        gas_price: 0,
        gas_limit: 0,
        to: TxKind::Call(OUTBE_SYSTEM_TX_ADDRESS),
        value: U256::ZERO,
        input,
    }
    .into_signed(Signature::test_signature())
    .into()
}

fn encoded(input: SystemTx) -> Bytes {
    input.encode().expect("valid binding fixture encodes")
}

fn parent(hash: B256) -> Bytes {
    encoded(SystemTx::Consensus {
        step_index: 0,
        payload: (CertifiedParentAccountingMetadata {
            finalized_block_number: 41,
            finalized_block_hash: hash,
            finalized_epoch: 7,
            finalized_view: 42,
            parent_view: 41,
            ordered_committee: vec![Address::repeat_byte(0x11)],
            signer_bitmap: vec![1],
            proof: Bytes::from_static(b"cert"),
            committee_set_hash: B256::repeat_byte(0x77),
            vrf_material_version: 3,
            vrf_group_public_key_hash: B256::repeat_byte(0x88),
            proof_kind: ParentParticipationProof::Finalization,
            missed_proposers: Vec::new(),
        })
        .encode()
        .unwrap(),
    })
}

fn boundary() -> DkgBoundaryArtifact {
    DkgBoundaryArtifact {
        epoch: 8,
        dkg_cycle: 2,
        freeze_height: 40,
        planned_activation_height: 42,
        target_set_hash: B256::repeat_byte(0x33),
        vrf_material_version: 3,
        vrf_group_public_key: B256::repeat_byte(0x44),
        vrf_group_public_key_bytes: Bytes::from_static(&[0x44; 96]),
        committee_set_hash: B256::repeat_byte(0x66),
        is_validator_set_change: true,
        outcome: Bytes::from_static(b"boundary"),
        is_full_dkg: false,
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: ReshareResult {
            new_active_set: vec![Address::repeat_byte(0x33)],
            active_set_hash: B256::repeat_byte(0x55),
        },
    }
}

fn steady_inputs() -> Vec<Bytes> {
    let mut inputs = vec![
        parent(B256::repeat_byte(0x41)),
        encoded(SystemTx::Consensus {
            step_index: 1,
            payload: crate::reshare_artifact::encode_late_finalize_credits_artifact(
                &(Default::default()),
            )
            .unwrap(),
        }),
        encoded(SystemTx::Consensus {
            step_index: 4,
            payload: Bytes::new(),
        }),
    ];
    for input in super::super::fixtures::begin_stage_inputs()
        .into_iter()
        .chain(super::super::fixtures::end_stage_inputs())
    {
        inputs.push(encoded(input));
    }
    inputs
}

pub(super) fn cases() -> Vec<Case> {
    let cycle = encoded(SystemTx::BeginBlock { step_index: 8 });
    let malformed = Bytes::from_static(b"OSCN\x03\x00");
    let mut cases = vec![
        Case::new("genesis_empty", 0, vec![]),
        Case::new("block1_requires_boundary", 1, vec![]),
        Case::new("steady_disabled", 2, steady_inputs()),
        Case::new(
            "wrong_order",
            2,
            vec![cycle.clone(), parent(B256::repeat_byte(0x41))],
        ),
        Case::new("malformed_layout", 2, vec![Bytes::new()]),
        Case::new("genesis_skips_parent", 0, vec![malformed.clone()]),
        Case::new("block1_skips_parent", 1, vec![malformed.clone()]),
        Case::new("parent_missing", 2, vec![]),
        Case::new("parent_wrong_kind", 2, vec![cycle.clone()]),
        Case::new("parent_malformed", 2, vec![malformed]),
        Case::new(
            "parent_hash_mismatch",
            2,
            vec![parent(B256::repeat_byte(0x42))],
        ),
        Case::new("parent_matches", 2, vec![parent(B256::repeat_byte(0x41))]),
        Case::new(
            "maximum_height_parent_matches",
            u64::MAX,
            vec![parent(B256::repeat_byte(0x41))],
        ),
        Case::new("no_boundary_skips_malformed_inputs", 2, vec![Bytes::new()]),
    ];
    let mut bad_artifacts = Case::new("artifacts_before_layout", 2, vec![Bytes::new()]);
    bad_artifacts.header.inner.extra_data = Bytes::from_static(b"bad artifacts");
    cases.push(bad_artifacts);
    let artifact = boundary();
    let matching = encoded(SystemTx::Consensus {
        step_index: 2,
        payload: crate::reshare_artifact::encode_boundary_artifact(&(artifact.clone())).unwrap(),
    });
    let mut mismatched = artifact.clone();
    mismatched.epoch += 1;
    for (name, inputs) in [
        ("boundary_missing", vec![cycle]),
        ("boundary_matches", vec![matching.clone()]),
        (
            "boundary_mismatch",
            vec![encoded(SystemTx::Consensus {
                step_index: 2,
                payload: crate::reshare_artifact::encode_boundary_artifact(&(mismatched)).unwrap(),
            })],
        ),
        (
            "boundary_match_then_bad_input",
            vec![matching.clone(), Bytes::new()],
        ),
        (
            "bad_input_before_boundary_match",
            vec![Bytes::new(), matching],
        ),
    ] {
        let mut case = Case::new(name, 2, inputs);
        case.artifacts.consensus_header_artifact =
            Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact.clone()));
        cases.push(case);
    }
    let mut layout_mismatch = Case::new("boundary_header_missing_body", 2, steady_inputs());
    layout_mismatch.artifacts.consensus_header_artifact =
        Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact));
    layout_mismatch.header.inner.extra_data =
        encode_outbe_block_artifacts(&layout_mismatch.artifacts)
            .expect("valid binding header fixture encodes");
    cases.push(layout_mismatch);
    cases
}
