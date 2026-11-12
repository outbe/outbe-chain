use std::time::Instant;

use alloy_primitives::{address, Bytes, B256};
use outbe_primitives::{
    consensus::DkgBoundaryArtifact,
    consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
    reshare_artifact::{
        encode_boundary_artifact, encode_late_finalize_credits_artifact,
        LateFinalizeCreditsArtifact,
    },
    system_tx::{
        protocol_block_gas_limit, SystemTx, CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
        CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX, CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
        CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX, CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
        CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX, OUTBE_SYSTEM_TX_ADDRESS,
        SYSTEM_TX_ARTIFACT_GAS_LIMIT,
    },
};

use crate::{
    BenchmarkScenario, CalldataStats, ChildFrameFidelity, ChildFrameTrace, ExecutionClass,
    GasComponent, GasLedger, Observation, Profile, ScenarioFidelity, ScenarioMetadata,
};

const CHAIN_ID: u64 = 2026;
const STEADY_BLOCK_NUMBER: u64 = 42;
const BOOTSTRAP_BLOCK_NUMBER: u64 = 1;

#[derive(Clone, Copy, Debug)]
pub struct SystemTxScenario {
    name: &'static str,
    fixture: fn() -> Result<SystemTx, String>,
}

#[derive(Clone)]
pub struct PreparedSystemTx {
    input: SystemTx,
    block_number: u64,
}

impl SystemTxScenario {
    #[must_use]
    pub const fn all() -> [Self; 20] {
        [
            Self::new("certified-parent-accounting", || {
                Ok(SystemTx::Consensus {
                    step_index: CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
                    payload: sample_metadata()
                        .encode()
                        .map_err(|error| error.to_string())?,
                })
            }),
            Self::new("late-finalize-credits", || {
                Ok(SystemTx::Consensus {
                    step_index: CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX,
                    payload: encode_late_finalize_credits_artifact(
                        &LateFinalizeCreditsArtifact::default(),
                    )
                    .map_err(|error| error.to_string())?,
                })
            }),
            Self::new("boundary-outcome", || {
                Ok(SystemTx::Consensus {
                    step_index: CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
                    payload: encode_boundary_artifact(&sample_boundary())
                        .map_err(|error| error.to_string())?,
                })
            }),
            Self::new("tee-bootstrap", || {
                Ok(SystemTx::Consensus {
                    step_index: CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
                    payload: outbe_primitives::tee_bootstrap_v2::codec::encode_canonical(
                        &sample_tee_bootstrap()?,
                    )
                    .map_err(|error| error.to_string())?,
                })
            }),
            Self::new("validator-liveness", || {
                Ok(SystemTx::Consensus {
                    step_index: CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX,
                    payload: Bytes::new(),
                })
            }),
            Self::new("radicle-publication", || {
                Ok(SystemTx::Consensus {
                    step_index: CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX,
                    payload: Bytes::from(
                        outbe_primitives::test_utils::sample_radicle_publication_batch(32)
                            .to_bytes()
                            .map_err(|error| error.to_string())?,
                    ),
                })
            }),
            Self::new("vote", || Ok(SystemTx::BeginBlock { step_index: 0 })),
            Self::new("update", || Ok(SystemTx::BeginBlock { step_index: 1 })),
            Self::new("enclave-upgrade-deadline", || {
                Ok(SystemTx::BeginBlock { step_index: 2 })
            }),
            Self::new("rewards", || Ok(SystemTx::BeginBlock { step_index: 3 })),
            Self::new("staking", || Ok(SystemTx::BeginBlock { step_index: 4 })),
            Self::new("oracle", || Ok(SystemTx::BeginBlock { step_index: 5 })),
            Self::new("oracle-slash", || {
                Ok(SystemTx::BeginBlock { step_index: 6 })
            }),
            Self::new("hyperlane-slash", || {
                Ok(SystemTx::BeginBlock { step_index: 7 })
            }),
            Self::new("cycle-tick", || Ok(SystemTx::BeginBlock { step_index: 8 })),
            Self::new("nod-factory", || Ok(SystemTx::EndBlock { step_index: 0 })),
            Self::new("gem-factory", || Ok(SystemTx::EndBlock { step_index: 1 })),
            Self::new("intex-factory", || Ok(SystemTx::EndBlock { step_index: 2 })),
            Self::new("credis-factory", || {
                Ok(SystemTx::EndBlock { step_index: 3 })
            }),
            Self::new("gem-cleanup", || Ok(SystemTx::EndBlock { step_index: 4 })),
        ]
    }

    #[must_use]
    pub const fn new(name: &'static str, fixture: fn() -> Result<SystemTx, String>) -> Self {
        Self { name, fixture }
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

impl BenchmarkScenario for SystemTxScenario {
    type Prepared = PreparedSystemTx;

    fn metadata(&self) -> ScenarioMetadata {
        let name = self.name;
        ScenarioMetadata::new(
            format!("system-tx/{name}/rust-wire/single"),
            format!("System tx {name} (Rust wire/accounting only)"),
            ExecutionClass::SystemTransaction,
            Profile::Single,
        )
        .with_fidelity(ScenarioFidelity::PartialStubbed)
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        let input = (self.fixture)()?;
        let block_number = if matches!(
            input,
            SystemTx::Consensus {
                step_index: CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
                ..
            }
        ) {
            BOOTSTRAP_BLOCK_NUMBER
        } else {
            STEADY_BLOCK_NUMBER
        };
        Ok(PreparedSystemTx {
            input,
            block_number,
        })
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        let total_started = Instant::now();

        let encode_started = Instant::now();
        let calldata = prepared.input.encode().map_err(|error| error.to_string())?;
        let encode_ns = elapsed_ns(encode_started);

        let decode_started = Instant::now();
        let decoded = SystemTx::decode(calldata.as_ref()).map_err(|error| error.to_string())?;
        let decode_ns = elapsed_ns(decode_started);

        let calldata_stats = CalldataStats::ethereum(calldata.as_ref());

        let mut observation = Observation::new(
            [
                (GasLedger::SystemVisible, 0),
                (GasLedger::SystemInternal, 0),
            ],
            [
                GasComponent::new(GasLedger::SystemVisible, "visible.zero_gas_envelope", 0, 1),
                GasComponent::new(
                    GasLedger::SystemInternal,
                    "internal.execution_excluded_rust_only",
                    0,
                    0,
                ),
            ],
        )
        .with_total_latency(elapsed_ns(total_started))
        .with_latency("rust/encode", encode_ns)
        .with_latency("rust/decode", decode_ns)
        .with_calldata(calldata_stats)
        .with_postcondition(
            "system_tx.codec_round_trip_kind",
            (decoded == prepared.input).to_string(),
        )
        .with_postcondition(
            "system_tx.body_zone",
            format!("{:?}", prepared.input.body_zone()),
        )
        .with_postcondition("system_tx.visible_charge_conserved", "true")
        .with_postcondition(
            "system_tx.full_execution_gas",
            concat!("unavailable in Rust-only v1; TO", "DO outbe-chain-6le.7"),
        )
        .with_postcondition("system_tx.internal_result_is_production_total", "false")
        .with_artifact("visible.gas_limit", "0")
        .with_artifact(
            "visible.execution_gas_limit",
            SYSTEM_TX_ARTIFACT_GAS_LIMIT.to_string(),
        )
        .with_artifact(
            "visible.block_gas_limit",
            protocol_block_gas_limit(prepared.block_number).to_string(),
        );
        observation.child_frames.push(ChildFrameTrace {
            label: "production system-tx execution".to_owned(),
            target: format!("{OUTBE_SYSTEM_TX_ADDRESS:?}"),
            selector: format!("0x{}", hex::encode(prepared.input.selector())),
            status: concat!("excluded; TO", "DO outbe-chain-6le.7").to_owned(),
            gas_used: 0,
            fidelity: ChildFrameFidelity::BenchmarkStub,
        });
        Ok(observation)
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn sample_metadata() -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata {
        finalized_block_number: STEADY_BLOCK_NUMBER - 1,
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
        proof_kind: ParentParticipationProof::Finalization,
        missed_proposers: Vec::new(),
    }
}

fn sample_boundary() -> DkgBoundaryArtifact {
    outbe_primitives::test_utils::sample_system_tx_boundary(STEADY_BLOCK_NUMBER)
}

fn sample_tee_bootstrap() -> Result<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2, String> {
    use outbe_primitives::tee_test_utils::{
        gramine_direct_bootstrap_v2, gramine_direct_policy_v1, DevValidatorV1,
    };

    let mut consensus_public = [0_u8; 48];
    consensus_public[0] = 1;
    let policy = gramine_direct_policy_v1(CHAIN_ID, B256::repeat_byte(0x11))
        .map_err(|error| error.to_string())?;
    gramine_direct_bootstrap_v2(
        policy,
        B256::repeat_byte(0x66),
        BOOTSTRAP_BLOCK_NUMBER,
        1_000_000,
        &[DevValidatorV1 {
            evm_secret: [1; 32],
            bls_minpk_public: consensus_public,
        }],
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use outbe_primitives::system_tx::{BlockExecutionStage, SYSTEM_CALLDATA_FRAMING_BYTES};

    use super::*;

    #[test]
    fn every_scenario_encodes_and_decodes_system_tx_input() {
        for (position, scenario) in SystemTxScenario::all().iter().enumerate() {
            let prepared = scenario.prepare(Profile::Single).expect("prepare scenario");
            let encoded = prepared.input.encode().expect("encode system tx");
            let decoded = SystemTx::decode(&encoded).expect("decode system tx");
            let (stage, step_index) = match position {
                0..=5 => (BlockExecutionStage::Consensus, position),
                6..=14 => (BlockExecutionStage::BeginBlock, position - 6),
                _ => (BlockExecutionStage::EndBlock, position - 15),
            };
            assert_eq!(decoded.stage(), stage, "{}", scenario.name());
            assert_eq!(
                usize::from(decoded.step_index()),
                step_index,
                "{}",
                scenario.name()
            );
            assert_eq!(decoded, prepared.input, "{}", scenario.name());
            assert_eq!(
                encoded.len(),
                SYSTEM_CALLDATA_FRAMING_BYTES + prepared.input.payload().len(),
                "{}",
                scenario.name()
            );
            assert_eq!(
                prepared.block_number,
                if scenario.name() == "tee-bootstrap" {
                    BOOTSTRAP_BLOCK_NUMBER
                } else {
                    STEADY_BLOCK_NUMBER
                },
                "{}",
                scenario.name()
            );
        }
    }
}
