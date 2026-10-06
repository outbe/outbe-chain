use super::*;
use outbe_primitives::reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit};
use outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2;
use reth_primitives_traits::Recovered;

type BeginInputs = Vec<(
    SystemTxKind,
    SystemTxInputV2,
    Option<AccountedParentArtifact>,
)>;

struct Scenario {
    height: u64,
    active: bool,
    artifacts: OutbeBlockArtifacts,
    body: Vec<Recovered<TransactionSigned>>,
    pending: Option<TeeBootstrapV2>,
    metadata: Option<CertifiedParentAccountingMetadata>,
    parent: B256,
    hint: Option<AccountedParentArtifact>,
}

impl Scenario {
    fn new(height: u64) -> Self {
        let parent = B256::with_last_byte(0xA5);
        let metadata = (height >= 2).then(|| CertifiedParentAccountingMetadata {
            finalized_block_number: height - 1,
            finalized_block_hash: parent,
            ..Default::default()
        });
        Self {
            height,
            active: false,
            artifacts: OutbeBlockArtifacts::default(),
            body: Vec::new(),
            pending: (height == 1).then(|| sample_tee_bootstrap_payload(1)),
            metadata,
            parent,
            hint: Some(AccountedParentArtifact {
                summary: ExecutionSummaryArtifact {
                    validator_fee_sum: U256::from(42),
                },
                timestamp: TEST_BLOCK_TIMESTAMP_BASE + height.saturating_sub(1),
                state_root: Some(B256::with_last_byte(0xB5)),
            }),
        }
    }

    fn resolve(&self) -> Result<BeginInputs, alloy_evm::block::BlockExecutionError> {
        let config = OutbeEvmConfig::new(test_chain_spec());
        let mut state = State::builder()
            .with_database(CacheDB::<EmptyDBTyped<ProviderError>>::default())
            .with_bundle_update()
            .build();
        let evm = config.evm_with_env(&mut state, test_evm_env(self.height, REWARDS_ADDRESS));
        let mut ctx = execution_ctx(None, encode_outbe_block_artifacts(&self.artifacts).unwrap());
        ctx.inner.parent_hash = self.parent;
        ctx.parent_consensus_metadata = self.metadata.clone();
        ctx.parent_artifact_hint = self.hint;
        ctx.expected_begin_system_txs = self.body.clone();
        ctx.pending_tee_bootstrap = self.pending.clone();
        let mut executor = config.create_executor(evm, ctx);
        executor.ocomp_lifecycle_active = self.active;
        executor.begin_block_system_tx_inputs(self.height, &self.artifacts)
    }

    fn signed_body(&self) -> Vec<Recovered<TransactionSigned>> {
        let signer = test_evm_signer();
        let activation = if self.active {
            OcompLifecycleActivation::at_block(0)
        } else {
            OcompLifecycleActivation::Disabled
        };
        let config = OutbeEvmConfig::new(test_chain_spec())
            .with_evm_signer(signer.clone())
            .with_ocomp_lifecycle_activation(activation);
        config
            .build_begin_system_txs(
                self.height,
                CHAIN_ID,
                outbe_primitives::system_tx::protocol_block_gas_limit(self.height),
                self.parent,
                &encode_outbe_block_artifacts(&self.artifacts).unwrap(),
                self.metadata.clone(),
                Some(signer.address()),
                None,
                self.pending.clone(),
            )
            .unwrap()
    }

    fn with_boundary(mut self) -> Self {
        self.artifacts.consensus_header_artifact =
            Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary_with(
                true,
                vec![(test_evm_signer().address(), dummy_pubkey(0xA2))],
            )));
        self
    }
}

fn signed_input(
    input: SystemTxInputV2,
    height: u64,
    ordinal: usize,
) -> Recovered<TransactionSigned> {
    let signer = test_evm_signer();
    let unsigned = build_unsigned_system_tx(
        input.kind(),
        ordinal.try_into().unwrap(),
        height,
        CHAIN_ID,
        input.encode().unwrap(),
    )
    .unwrap();
    Recovered::new_unchecked(signer.sign_unsigned(unsigned).unwrap(), signer.address())
}

#[derive(Clone, Copy)]
struct PhaseMatrixCase {
    name: &'static str,
    height: u64,
    active: bool,
    boundary: bool,
}

const PHASE_MATRIX_CASES: [PhaseMatrixCase; 16] = [
    PhaseMatrixCase {
        name: "height_0_inactive_ordinary",
        height: 0,
        active: false,
        boundary: false,
    },
    PhaseMatrixCase {
        name: "height_0_inactive_boundary",
        height: 0,
        active: false,
        boundary: true,
    },
    PhaseMatrixCase {
        name: "height_0_active_ordinary",
        height: 0,
        active: true,
        boundary: false,
    },
    PhaseMatrixCase {
        name: "height_0_active_boundary",
        height: 0,
        active: true,
        boundary: true,
    },
    PhaseMatrixCase {
        name: "height_1_inactive_ordinary",
        height: 1,
        active: false,
        boundary: false,
    },
    PhaseMatrixCase {
        name: "height_1_inactive_boundary",
        height: 1,
        active: false,
        boundary: true,
    },
    PhaseMatrixCase {
        name: "height_1_active_ordinary",
        height: 1,
        active: true,
        boundary: false,
    },
    PhaseMatrixCase {
        name: "height_1_active_boundary",
        height: 1,
        active: true,
        boundary: true,
    },
    PhaseMatrixCase {
        name: "height_2_inactive_ordinary",
        height: 2,
        active: false,
        boundary: false,
    },
    PhaseMatrixCase {
        name: "height_2_inactive_boundary",
        height: 2,
        active: false,
        boundary: true,
    },
    PhaseMatrixCase {
        name: "height_2_active_ordinary",
        height: 2,
        active: true,
        boundary: false,
    },
    PhaseMatrixCase {
        name: "height_2_active_boundary",
        height: 2,
        active: true,
        boundary: true,
    },
    PhaseMatrixCase {
        name: "height_9_inactive_ordinary",
        height: 9,
        active: false,
        boundary: false,
    },
    PhaseMatrixCase {
        name: "height_9_inactive_boundary",
        height: 9,
        active: false,
        boundary: true,
    },
    PhaseMatrixCase {
        name: "height_9_active_ordinary",
        height: 9,
        active: true,
        boundary: false,
    },
    PhaseMatrixCase {
        name: "height_9_active_boundary",
        height: 9,
        active: true,
        boundary: true,
    },
];

fn phase_matrix_scenario(case: PhaseMatrixCase) -> Scenario {
    let PhaseMatrixCase {
        height,
        active,
        boundary,
        ..
    } = case;
    let mut scenario = Scenario::new(height);
    scenario.active = active;
    if boundary {
        scenario = scenario.with_boundary();
    }
    // Nonempty credit payload checks preservation as well as phase order.
    if height >= 2 {
        scenario.artifacts.late_finalize_credits = Some(LateFinalizeCreditsArtifact {
            batches: vec![PerBlockCredit {
                fb_number: height - 1,
                fb_hash: scenario.parent,
                epoch: 0,
                view: 3,
                parent_view: 2,
                committee_set_hash: B256::with_last_byte(0xC5),
                signer_bitmap: vec![1],
                aggregate_signature: [0; 96],
            }],
        });
    }
    scenario
}

fn assert_signed_begin_body(
    scenario: &Scenario,
    proposer: &BeginInputs,
    body: &[Recovered<TransactionSigned>],
) {
    assert_eq!(proposer.len(), body.len());
    for ((kind, input, summary), tx) in proposer.iter().zip(body) {
        assert_eq!(input.kind(), *kind);
        assert_eq!(&input.encode().unwrap(), tx.tx().input());
        assert_eq!(
            *summary,
            if *kind == SystemTxKind::CertifiedParentAccounting {
                scenario.hint
            } else {
                None
            }
        );
    }
}

fn assert_begin_phase_order(proposer: &BeginInputs, height: u64, active: bool) {
    if height == 0 {
        assert!(proposer.is_empty());
        return;
    }
    let kinds: Vec<_> = proposer.iter().map(|(kind, _, _)| *kind).collect();
    assert_eq!(kinds.last(), Some(&SystemTxKind::HookEvents));
    assert!(!kinds.contains(&SystemTxKind::OcompTerminalRequest));
    assert_eq!(kinds.contains(&SystemTxKind::TeeBootstrap), height == 1);
    assert_eq!(kinds.contains(&SystemTxKind::OcompLifecycleBegin), active);
    let cycle = kinds
        .iter()
        .position(|kind| *kind == SystemTxKind::CycleTick)
        .unwrap();
    assert_eq!(kinds[cycle + 1], SystemTxKind::RewardsGemDelivery);
    if active {
        assert_eq!(kinds[cycle - 1], SystemTxKind::OcompLifecycleBegin);
    }
    if height >= 2 {
        assert_eq!(
            &kinds[..2],
            &[
                SystemTxKind::CertifiedParentAccounting,
                SystemTxKind::LateFinalizeCredits
            ]
        );
    }
}

#[test]
fn begin_inputs_match_signed_proposer_body_and_verifier_across_phase_matrix() {
    for case in PHASE_MATRIX_CASES {
        let mut scenario = phase_matrix_scenario(case);
        let proposer = scenario.resolve().expect(case.name);
        let body = scenario.signed_body();
        assert_signed_begin_body(&scenario, &proposer, &body);
        scenario.body = body;
        assert_eq!(scenario.resolve().unwrap(), proposer);
        assert_begin_phase_order(&proposer, case.height, case.active);
    }
}

#[test]
fn verifier_reports_missing_malformed_and_wrong_kind_at_the_first_bad_ordinal() {
    for height in [1, 2] {
        let mut scenario = Scenario::new(height).with_boundary();
        scenario.active = true;
        let canonical = scenario.signed_body();
        // An empty vector intentionally denotes proposer mode; nonempty prefixes
        // exercise missing verifier inputs without changing that contract.
        for length in 1..canonical.len() {
            scenario.body = canonical[..length].to_vec();
            assert!(scenario
                .resolve()
                .unwrap_err()
                .to_string()
                .contains(&format!(
                    "missing expected begin system tx at ordinal {length}"
                )));
        }
        for ordinal in 0..canonical.len() {
            scenario.body = canonical.clone();
            scenario.body[ordinal] =
                Recovered::new_unchecked(test_regular_tx(), test_evm_signer().address());
            assert!(scenario
                .resolve()
                .unwrap_err()
                .to_string()
                .contains("decode expected begin system tx input:"));
            scenario.body = canonical.clone();
            scenario.body[ordinal] = canonical[(ordinal + 1) % canonical.len()].clone();
            let kind = SystemTxInputV2::decode(canonical[ordinal].tx().input())
                .unwrap()
                .kind();
            let expected = if kind == SystemTxKind::TeeBootstrap {
                format!("expected mandatory OST3 system tx at ordinal {ordinal}")
            } else {
                format!("expected {kind:?} system tx at ordinal {ordinal}")
            };
            let error = scenario.resolve().unwrap_err().to_string();
            assert!(error.contains(&expected), "{error}");
        }
    }
}

#[test]
fn genesis_ignores_stale_inputs_and_block_one_requires_bootstrap_from_its_active_source() {
    let mut genesis = Scenario::new(0).with_boundary();
    genesis.active = true;
    genesis.pending = Some(sample_tee_bootstrap_payload(1));
    genesis.body = vec![Recovered::new_unchecked(
        test_regular_tx(),
        test_evm_signer().address(),
    )];
    assert!(genesis.resolve().unwrap().is_empty());

    let mut first = Scenario::new(1);
    let body = first.signed_body();
    first.pending = None;
    assert!(first
        .resolve()
        .unwrap_err()
        .to_string()
        .contains("missing mandatory block-1 OST3 bootstrap payload"));
    first.body = body;
    assert!(first
        .resolve()
        .unwrap()
        .iter()
        .any(|(kind, _, _)| *kind == SystemTxKind::TeeBootstrap));
}

#[test]
fn forbidden_late_bootstrap_is_checked_after_cpa_and_boundary_but_before_oracle_decode() {
    let mut scenario = Scenario::new(2).with_boundary();
    let canonical = scenario.signed_body();
    scenario.pending = Some(sample_tee_bootstrap_payload(1));
    let metadata = scenario.metadata.take().unwrap();
    assert!(scenario
        .resolve()
        .unwrap_err()
        .to_string()
        .contains("missing parent consensus metadata"));
    scenario.metadata = Some(CertifiedParentAccountingMetadata {
        finalized_block_hash: B256::with_last_byte(0xFF),
        ..metadata.clone()
    });
    assert!(scenario
        .resolve()
        .unwrap_err()
        .to_string()
        .contains("metadata hash must match block parent"));
    scenario.metadata = Some(metadata);
    scenario.body = canonical;
    let boundary = scenario
        .body
        .iter()
        .position(|tx| {
            SystemTxInputV2::decode(tx.tx().input()).unwrap().kind()
                == SystemTxKind::BoundaryOutcome
        })
        .unwrap();
    let ConsensusHeaderArtifact::BoundaryOutcome(mut artifact) = scenario
        .artifacts
        .consensus_header_artifact
        .clone()
        .unwrap()
    else {
        panic!("boundary fixture")
    };
    artifact.dkg_cycle += 1;
    let original_boundary = scenario.body[boundary].clone();
    scenario.body[boundary] =
        signed_input(SystemTxInputV2::BoundaryOutcome { artifact }, 2, boundary);
    assert!(scenario
        .resolve()
        .unwrap_err()
        .to_string()
        .contains("BoundaryOutcome system tx artifact mismatch"));
    scenario.body[boundary] = original_boundary;
    let oracle = scenario
        .body
        .iter()
        .position(|tx| {
            SystemTxInputV2::decode(tx.tx().input()).unwrap().kind()
                == SystemTxKind::OracleSlashWindow
        })
        .unwrap();
    scenario.body[oracle] =
        Recovered::new_unchecked(test_regular_tx(), test_evm_signer().address());
    assert!(scenario
        .resolve()
        .unwrap_err()
        .to_string()
        .contains("OST3 bootstrap payload is forbidden at block 2"));
}

#[test]
fn cpa_artifact_resolution_fails_before_later_body_inputs_are_decoded() {
    let mut scenario = Scenario::new(2);
    scenario.body = scenario.signed_body();
    scenario.body[1] = Recovered::new_unchecked(test_regular_tx(), test_evm_signer().address());
    scenario.hint = None;
    assert!(scenario
        .resolve()
        .unwrap_err()
        .to_string()
        .contains("missing execution summary artifact for accounted-parent block"));
}

#[test]
fn surplus_begin_input_remains_owned_by_structural_body_validation() {
    let mut scenario = Scenario::new(1);
    let inputs = scenario.resolve().unwrap();
    scenario.body = scenario.signed_body();
    scenario.body.push(scenario.body.last().unwrap().clone());
    assert_eq!(scenario.resolve().unwrap(), inputs);
    let signed: Vec<_> = scenario.body.iter().map(|tx| tx.tx().clone()).collect();
    assert!(crate::system_tx::split_system_layout(&signed).is_err());
}
