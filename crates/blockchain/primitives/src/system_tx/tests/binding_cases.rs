//! Shared inputs and original-code diagnostics for the binding regression tests.
#![cfg(test)]
use super::primitives::{
    addresses::{OUTBE_SYSTEM_TX_ADDRESS, REWARDS_ADDRESS},
    consensus::{DkgBoundaryArtifact, ReshareResult},
    consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
    reshare_artifact::{
        encode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
    },
    system_tx::{OcompLifecycleActivation, SystemTxInputV2, SystemTxLayout},
    OutbeBlockBody, OutbeHeader,
};
use alloy_consensus::{Header, SignableTransaction, TxLegacy};
use alloy_primitives::{Address, Bytes, Signature, TxKind, B256, U256};
use reth_ethereum::TransactionSigned;

pub(super) enum Check {
    Layout(OcompLifecycleActivation),
    Parent,
    Boundary,
}

pub(super) struct Case {
    pub name: &'static str,
    pub check: Check,
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

    fn new(name: &'static str, check: Check, height: u64, inputs: Vec<Bytes>) -> Self {
        Self {
            name,
            check,
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
        gas_limit: 21_000,
        to: TxKind::Call(OUTBE_SYSTEM_TX_ADDRESS),
        value: U256::ZERO,
        input,
    }
    .into_signed(Signature::test_signature())
    .into()
}

fn encoded(input: SystemTxInputV2) -> Bytes {
    input.encode().expect("valid binding fixture encodes")
}

fn parent(hash: B256) -> Bytes {
    encoded(SystemTxInputV2::CertifiedParentAccounting {
        metadata: CertifiedParentAccountingMetadata {
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
        },
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
        tee_recipient_pubkeys: Vec::new(),
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: ReshareResult {
            new_active_set: vec![Address::repeat_byte(0x33)],
            active_set_hash: B256::repeat_byte(0x55),
        },
    }
}

fn steady_inputs(lifecycle: bool) -> Vec<Bytes> {
    let mut inputs = vec![
        parent(B256::repeat_byte(0x41)),
        encoded(SystemTxInputV2::LateFinalizeCredits {
            artifact: Default::default(),
        }),
    ];
    if lifecycle {
        inputs.push(encoded(SystemTxInputV2::OcompLifecycleBegin));
    }
    inputs.extend([
        encoded(SystemTxInputV2::CycleTick),
        encoded(SystemTxInputV2::RewardsGemDelivery),
        encoded(SystemTxInputV2::OracleSlashWindow),
        encoded(SystemTxInputV2::HookEvents),
    ]);
    if lifecycle {
        inputs.push(encoded(SystemTxInputV2::OcompTerminalRequest));
    }
    inputs
}

pub(super) fn cases() -> Vec<Case> {
    use OcompLifecycleActivation::{AtBlock, Disabled};
    let cycle = encoded(SystemTxInputV2::CycleTick);
    let malformed = Bytes::from_static(b"OSA3\x02");
    let mut cases = vec![
        Case::new("genesis_empty", Check::Layout(Disabled), 0, vec![]),
        Case::new(
            "block1_requires_boundary",
            Check::Layout(Disabled),
            1,
            vec![],
        ),
        Case::new(
            "steady_disabled",
            Check::Layout(Disabled),
            2,
            steady_inputs(false),
        ),
        Case::new(
            "steady_active",
            Check::Layout(AtBlock(2)),
            2,
            steady_inputs(true),
        ),
        Case::new(
            "lifecycle_before_activation",
            Check::Layout(Disabled),
            2,
            steady_inputs(true),
        ),
        Case::new(
            "missing_lifecycle_after_activation",
            Check::Layout(AtBlock(2)),
            2,
            steady_inputs(false),
        ),
        Case::new(
            "wrong_order",
            Check::Layout(Disabled),
            2,
            vec![cycle.clone(), parent(B256::repeat_byte(0x41))],
        ),
        Case::new(
            "malformed_layout",
            Check::Layout(Disabled),
            2,
            vec![Bytes::new()],
        ),
        Case::new(
            "genesis_skips_parent",
            Check::Parent,
            0,
            vec![malformed.clone()],
        ),
        Case::new(
            "block1_skips_parent",
            Check::Parent,
            1,
            vec![malformed.clone()],
        ),
        Case::new("parent_missing", Check::Parent, 2, vec![]),
        Case::new("parent_wrong_kind", Check::Parent, 2, vec![cycle.clone()]),
        Case::new("parent_malformed", Check::Parent, 2, vec![malformed]),
        Case::new(
            "parent_hash_mismatch",
            Check::Parent,
            2,
            vec![parent(B256::repeat_byte(0x42))],
        ),
        Case::new(
            "parent_matches",
            Check::Parent,
            2,
            vec![parent(B256::repeat_byte(0x41))],
        ),
        Case::new(
            "maximum_height_parent_matches",
            Check::Parent,
            u64::MAX,
            vec![parent(B256::repeat_byte(0x41))],
        ),
        Case::new(
            "no_boundary_skips_malformed_inputs",
            Check::Boundary,
            2,
            vec![Bytes::new()],
        ),
    ];
    let mut bad_artifacts = Case::new(
        "artifacts_before_layout",
        Check::Layout(Disabled),
        2,
        vec![Bytes::new()],
    );
    bad_artifacts.header.inner.extra_data = Bytes::from_static(b"bad artifacts");
    cases.push(bad_artifacts);
    let artifact = boundary();
    let matching = encoded(SystemTxInputV2::BoundaryOutcome {
        artifact: artifact.clone(),
    });
    let mut mismatched = artifact.clone();
    mismatched.epoch += 1;
    for (name, inputs) in [
        ("boundary_missing", vec![cycle]),
        ("boundary_matches", vec![matching.clone()]),
        (
            "boundary_mismatch",
            vec![encoded(SystemTxInputV2::BoundaryOutcome {
                artifact: mismatched,
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
        let mut case = Case::new(name, Check::Boundary, 2, inputs);
        case.artifacts.consensus_header_artifact =
            Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact.clone()));
        cases.push(case);
    }
    let mut layout_mismatch = Case::new(
        "boundary_header_missing_body",
        Check::Layout(Disabled),
        2,
        steady_inputs(false),
    );
    layout_mismatch.artifacts.consensus_header_artifact =
        Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact));
    layout_mismatch.header.inner.extra_data =
        encode_outbe_block_artifacts(&layout_mismatch.artifacts)
            .expect("valid binding header fixture encodes");
    cases.push(layout_mismatch);
    cases
}

/// Golden diagnostics captured from both layers before extracting the bindings.
pub(super) fn assert_diagnostics(
    node: bool,
    validate_layout: impl Fn(
        &OutbeBlockBody,
        &OutbeHeader,
        OcompLifecycleActivation,
    ) -> Result<(), String>,
    validate_parent: impl Fn(&SystemTxLayout<'_>, &OutbeHeader) -> Result<(), String>,
    validate_boundary: impl Fn(&SystemTxLayout<'_>, &OutbeBlockArtifacts) -> Result<(), String>,
) {
    let expected = [
        ("genesis_empty", None, None),
        ("block1_requires_boundary", Some("invalid system tx set: V2 genesis bootstrap: block 1 must carry a BoundaryOutcome system tx (got has_boundary_outcome = false)"), Some("invalid system tx set: V2 genesis bootstrap: block 1 must carry a BoundaryOutcome system tx (got has_boundary_outcome = false)")),
        ("steady_disabled", None, None),
        ("steady_active", None, None),
        ("lifecycle_before_activation", Some("invalid system tx set: active system tx set mismatch: expected begin [CertifiedParentAccounting, LateFinalizeCredits, CycleTick, RewardsGemDelivery, OracleSlashWindow, HookEvents], expected end [], actual begin [CertifiedParentAccounting, LateFinalizeCredits, OcompLifecycleBegin, CycleTick, RewardsGemDelivery, OracleSlashWindow, HookEvents], actual end [OcompTerminalRequest]"), Some("invalid system tx set: active system tx set mismatch: expected begin [CertifiedParentAccounting, LateFinalizeCredits, CycleTick, RewardsGemDelivery, OracleSlashWindow, HookEvents], expected end [], actual begin [CertifiedParentAccounting, LateFinalizeCredits, OcompLifecycleBegin, CycleTick, RewardsGemDelivery, OracleSlashWindow, HookEvents], actual end [OcompTerminalRequest]")),
        ("missing_lifecycle_after_activation", Some("invalid system tx set: active system tx set mismatch: expected begin [CertifiedParentAccounting, LateFinalizeCredits, OcompLifecycleBegin, CycleTick, RewardsGemDelivery, OracleSlashWindow, HookEvents], expected end [OcompTerminalRequest], actual begin [CertifiedParentAccounting, LateFinalizeCredits, CycleTick, RewardsGemDelivery, OracleSlashWindow, HookEvents], actual end []"), Some("invalid system tx set: active system tx set mismatch: expected begin [CertifiedParentAccounting, LateFinalizeCredits, OcompLifecycleBegin, CycleTick, RewardsGemDelivery, OracleSlashWindow, HookEvents], expected end [OcompTerminalRequest], actual begin [CertifiedParentAccounting, LateFinalizeCredits, CycleTick, RewardsGemDelivery, OracleSlashWindow, HookEvents], actual end []")),
        ("wrong_order", Some("invalid system tx layout: system tx kind order violation in BeginBlock: previous CycleTick, current CertifiedParentAccounting"), Some("invalid system tx layout for leader binding: system tx kind order violation in BeginBlock: previous CycleTick, current CertifiedParentAccounting")),
        ("malformed_layout", Some("invalid system tx layout: system tx input too short: 0 bytes"), Some("invalid system tx layout for leader binding: system tx input too short: 0 bytes")),
        ("genesis_skips_parent", None, None),
        ("block1_skips_parent", None, None),
        ("parent_missing", Some("missing CertifiedParentAccounting system tx for block 2"), Some("missing CertifiedParentAccounting system tx")),
        ("parent_wrong_kind", Some("expected CertifiedParentAccounting system tx at begin ordinal 0"), Some("expected CertifiedParentAccounting system tx at begin ordinal 0")),
        ("parent_malformed", Some("decode CertifiedParentAccounting input: system tx codec error: fatal: certified-parent accounting metadata too short"), Some("decode CertifiedParentAccounting system tx input: system tx codec error: fatal: certified-parent accounting metadata too short")),
        ("parent_hash_mismatch", Some("CertifiedParentAccounting metadata hash must match block parent: expected 0x4141414141414141414141414141414141414141414141414141414141414141, got 0x4242424242424242424242424242424242424242424242424242424242424242"), Some("CertifiedParentAccounting metadata hash must match block parent: expected 0x4141414141414141414141414141414141414141414141414141414141414141, got 0x4242424242424242424242424242424242424242424242424242424242424242")),
        ("parent_matches", None, None),
        ("maximum_height_parent_matches", None, None),
        ("no_boundary_skips_malformed_inputs", None, None),
        ("artifacts_before_layout", Some("decode Outbe block artifacts: fatal: unknown non-empty extra_data block artifact"), Some("decode Outbe block artifacts for system tx validation: fatal: unknown non-empty extra_data block artifact")),
        ("boundary_missing", Some("missing BoundaryOutcome system tx for header artifact"), Some("missing BoundaryOutcome system tx for header artifact")),
        ("boundary_matches", None, None),
        ("boundary_mismatch", Some("BoundaryOutcome system tx artifact mismatch"), Some("BoundaryOutcome system tx artifact mismatch")),
        ("boundary_match_then_bad_input", Some("decode system tx input: system tx input too short: 0 bytes"), Some("decode system transaction input: system tx input too short: 0 bytes")),
        ("bad_input_before_boundary_match", Some("decode system tx input: system tx input too short: 0 bytes"), Some("decode system transaction input: system tx input too short: 0 bytes")),
        ("boundary_header_missing_body", Some("invalid system tx set: active system tx set mismatch: expected begin [CertifiedParentAccounting, LateFinalizeCredits, CycleTick, RewardsGemDelivery, BoundaryOutcome, OracleSlashWindow, HookEvents], expected end [], actual begin [CertifiedParentAccounting, LateFinalizeCredits, CycleTick, RewardsGemDelivery, OracleSlashWindow, HookEvents], actual end []"), Some("invalid system tx set: active system tx set mismatch: expected begin [CertifiedParentAccounting, LateFinalizeCredits, CycleTick, RewardsGemDelivery, BoundaryOutcome, OracleSlashWindow, HookEvents], expected end [], actual begin [CertifiedParentAccounting, LateFinalizeCredits, CycleTick, RewardsGemDelivery, OracleSlashWindow, HookEvents], actual end []")),
    ];
    let cases = cases();
    assert_eq!(cases.len(), expected.len());
    for (case, (name, node_error, leader_error)) in cases.iter().zip(expected) {
        let layout = case.layout();
        let result = match case.check {
            Check::Layout(activation) => validate_layout(&case.body, &case.header, activation),
            Check::Parent => validate_parent(&layout, &case.header),
            Check::Boundary => validate_boundary(&layout, &case.artifacts),
        };
        assert_eq!(case.name, name);
        let expected_error = if node { node_error } else { leader_error };
        assert_eq!(
            result.as_ref().err().map(String::as_str),
            expected_error,
            "{}",
            name
        );
    }
}
