use super::*;
use outbe_primitives::reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit};
use outbe_primitives::system_tx::{
    SystemTxStepId, CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX,
    CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX, CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
};
use outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2;
use reth_primitives_traits::Recovered;

type BeginInputs = Vec<(SystemTx, Option<AccountedParentArtifact>)>;

struct Scenario {
    height: u64,
    artifacts: OutbeBlockArtifacts,
    body: Vec<Recovered<TransactionSigned>>,
    pending: Option<TeeBootstrapV2>,
    metadata: Option<CertifiedParentAccountingMetadata>,
    parent: B256,
    hint: Option<AccountedParentArtifact>,
}

#[test]
fn publication_begin_inputs_match_signed_proposer_and_verifier_after_hooks() {
    for height in [1, 2] {
        let mut scenario = Scenario::new(height).with_boundary();
        scenario.artifacts.radicle_publication_enabled = true;
        scenario.artifacts.radicle_publication =
            Some(outbe_primitives::test_utils::sample_radicle_publication_batch(32));
        let proposer = scenario.resolve().unwrap();
        let body = scenario.signed_body();
        assert_signed_begin_body(&scenario, &proposer, &body);
        let pub_idx = proposer
            .iter()
            .position(|(tx, _)| {
                matches!(
                    tx,
                    SystemTx::Consensus {
                        step_index:
                            outbe_primitives::system_tx::CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX,
                        ..
                    }
                )
            })
            .expect("RadiclePublication must be present in CONSENSUS_ORDER");
        assert!(matches!(
            proposer[pub_idx - 1].0,
            SystemTx::Consensus {
                step_index: outbe_primitives::system_tx::CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX,
                ..
            }
        ));
        assert!(matches!(
            proposer[pub_idx + 1].0,
            SystemTx::BeginBlock { step_index: 0 }
        ));
        assert!(matches!(
            proposer.last().unwrap().0,
            SystemTx::BeginBlock { step_index: 8 }
        ));
        scenario.body = body;
        let verifier = scenario.resolve().unwrap();
        assert_eq!(proposer, verifier);
    }
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
            artifacts: if height == 1 {
                decode_outbe_block_artifacts(&fixtures::block_one_artifacts(Bytes::new())).unwrap()
            } else {
                OutbeBlockArtifacts::default()
            },
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
        let config = OutbeEvmConfig::new(
            test_chain_spec(),
            std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
        );
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
        let executor = config.create_executor(evm, ctx);
        executor.begin_block_system_tx_inputs(self.height, &self.artifacts)
    }

    fn signed_body(&self) -> Vec<Recovered<TransactionSigned>> {
        let signer = test_evm_signer();
        let config = OutbeEvmConfig::new(
            test_chain_spec(),
            std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
        )
        .with_evm_signer(signer.clone());
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

fn signed_input(input: SystemTx, height: u64, ordinal: usize) -> Recovered<TransactionSigned> {
    let signer = test_evm_signer();
    let unsigned = build_unsigned_system_tx(
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
    boundary: bool,
}

const PHASE_MATRIX_CASES: [PhaseMatrixCase; 8] = [
    PhaseMatrixCase {
        name: "height_0_ordinary",
        height: 0,
        boundary: false,
    },
    PhaseMatrixCase {
        name: "height_0_boundary",
        height: 0,
        boundary: true,
    },
    PhaseMatrixCase {
        name: "height_1_ordinary",
        height: 1,
        boundary: false,
    },
    PhaseMatrixCase {
        name: "height_1_boundary",
        height: 1,
        boundary: true,
    },
    PhaseMatrixCase {
        name: "height_2_ordinary",
        height: 2,
        boundary: false,
    },
    PhaseMatrixCase {
        name: "height_2_boundary",
        height: 2,
        boundary: true,
    },
    PhaseMatrixCase {
        name: "height_9_ordinary",
        height: 9,
        boundary: false,
    },
    PhaseMatrixCase {
        name: "height_9_boundary",
        height: 9,
        boundary: true,
    },
];

fn phase_matrix_scenario(case: PhaseMatrixCase) -> Scenario {
    let PhaseMatrixCase {
        height, boundary, ..
    } = case;
    let mut scenario = Scenario::new(height);
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
    for ((input, summary), tx) in proposer.iter().zip(body) {
        assert_eq!(&input.encode().unwrap(), tx.tx().input());
        assert_eq!(
            *summary,
            if matches!(
                input,
                SystemTx::Consensus {
                    step_index: outbe_primitives::system_tx::CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
                    ..
                }
            ) {
                scenario.hint
            } else {
                None
            }
        );
    }
}

fn assert_begin_phase_order(proposer: &BeginInputs, height: u64) {
    if height == 0 {
        assert!(proposer.is_empty());
        return;
    }
    let steps: Vec<_> = proposer.iter().map(|(input, _)| input.step_id()).collect();
    assert_eq!(steps.last(), Some(&SystemTxStepId::begin_block(8)));
    assert_eq!(
        steps.contains(&SystemTxStepId::consensus(
            CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX
        )),
        height == 1
    );
    assert!(steps.contains(&SystemTxStepId::begin_block(8)));
    if height >= 2 {
        assert_eq!(
            &steps[..2],
            &[
                SystemTxStepId::consensus(CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX),
                SystemTxStepId::consensus(CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX)
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
        assert_begin_phase_order(&proposer, case.height);
    }
}

#[test]
fn verifier_rejects_bad_inputs_and_out_of_order_steps() {
    for height in [1, 2] {
        let mut scenario = Scenario::new(height).with_boundary();
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
            let body: Vec<_> = scenario.body.iter().map(|tx| tx.tx().clone()).collect();
            assert!(outbe_evm::system_tx::split_system_layout(&body).is_err());
            let input = SystemTx::decode(canonical[ordinal].tx().input()).unwrap();
            let expected = format!(
                "expected {}",
                outbe_runtime::BLOCK_ORDER.step_name_or_stage(&input)
            );
            let error = scenario.resolve().unwrap_err().to_string();
            assert!(error.contains(&expected), "{error}");
        }
    }
}

#[test]
fn genesis_ignores_stale_inputs_and_block_one_requires_bootstrap_from_its_active_source() {
    let mut genesis = Scenario::new(0).with_boundary();
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
    assert!(first.resolve().unwrap().iter().any(|(input, _)| matches!(
        input,
        SystemTx::Consensus {
            step_index: outbe_primitives::system_tx::CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
            ..
        }
    )));
}

#[test]
fn forbidden_late_bootstrap_precedes_cpa_and_body_validation() {
    let mut scenario = Scenario::new(2).with_boundary();
    let canonical = scenario.signed_body();
    scenario.pending = Some(sample_tee_bootstrap_payload(1));
    assert!(scenario
        .resolve()
        .unwrap_err()
        .to_string()
        .contains("OST3 bootstrap payload is forbidden at block 2"));
    scenario.pending = None;
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
            matches!(
                SystemTx::decode(tx.tx().input()).unwrap(),
                SystemTx::Consensus {
                    step_index: outbe_primitives::system_tx::CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
                    ..
                }
            )
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
    scenario.body[boundary] = signed_input(
        SystemTx::Consensus {
            step_index: outbe_primitives::system_tx::CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
            payload: outbe_primitives::reshare_artifact::encode_boundary_artifact(&artifact)
                .unwrap(),
        },
        2,
        boundary,
    );
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
            matches!(
                SystemTx::decode(tx.tx().input()).unwrap(),
                SystemTx::BeginBlock { step_index: 6 }
            )
        })
        .unwrap();
    scenario.pending = Some(sample_tee_bootstrap_payload(1));
    scenario.body[oracle] =
        Recovered::new_unchecked(test_regular_tx(), test_evm_signer().address());
    assert!(scenario
        .resolve()
        .unwrap_err()
        .to_string()
        .contains("OST3 bootstrap payload is forbidden at block 2"));
}

#[test]
fn all_prefix_inputs_are_validated_before_cpa_artifact_resolution() {
    let mut scenario = Scenario::new(2);
    scenario.body = scenario.signed_body();
    scenario.body[1] = Recovered::new_unchecked(test_regular_tx(), test_evm_signer().address());
    scenario.hint = None;
    assert!(scenario
        .resolve()
        .unwrap_err()
        .to_string()
        .contains("decode expected begin system tx input:"));
    scenario.body = scenario.signed_body();
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
    assert!(outbe_evm::system_tx::split_system_layout(&signed).is_err());
}
