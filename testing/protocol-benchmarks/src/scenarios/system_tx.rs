use std::time::Instant;

use alloy_primitives::{address, Bytes, B256};
use outbe_primitives::{
    consensus::DkgBoundaryArtifact,
    consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
    reshare_artifact::LateFinalizeCreditsArtifact,
    system_tx::{
        protocol_block_gas_limit, SystemTx, OUTBE_SYSTEM_TX_ADDRESS, SYSTEM_TX_ARTIFACT_GAS_LIMIT,
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
                Ok(SystemTx::CertifiedParentAccounting {
                    metadata: sample_metadata(),
                })
            }),
            Self::new("late-finalize-credits", || {
                Ok(SystemTx::LateFinalizeCredits {
                    artifact: LateFinalizeCreditsArtifact::default(),
                })
            }),
            Self::new("boundary-outcome", || {
                Ok(SystemTx::BoundaryOutcome {
                    artifact: sample_boundary(),
                })
            }),
            Self::new("tee-bootstrap", || {
                Ok(SystemTx::TeeBootstrap {
                    payload: sample_tee_bootstrap()?,
                })
            }),
            Self::new("validator-liveness", || Ok(SystemTx::ValidatorLiveness)),
            Self::new("radicle-publication", || {
                Ok(SystemTx::RadiclePublication {
                    batch: outbe_primitives::test_utils::sample_radicle_publication_batch(32),
                })
            }),
            Self::new("vote", || Ok(SystemTx::Vote)),
            Self::new("update", || Ok(SystemTx::Update)),
            Self::new("enclave-upgrade-deadline", || {
                Ok(SystemTx::EnclaveUpgradeDeadline)
            }),
            Self::new("rewards", || Ok(SystemTx::Rewards)),
            Self::new("staking", || Ok(SystemTx::Staking)),
            Self::new("oracle", || Ok(SystemTx::Oracle)),
            Self::new("oracle-slash", || Ok(SystemTx::OracleSlash)),
            Self::new("hyperlane-slash", || Ok(SystemTx::HyperlaneSlash)),
            Self::new("cycle-tick", || Ok(SystemTx::CycleTick)),
            Self::new("nod-factory", || Ok(SystemTx::NodFactory)),
            Self::new("gem-factory", || Ok(SystemTx::GemFactory)),
            Self::new("intex-factory", || Ok(SystemTx::IntexFactory)),
            Self::new("credis-factory", || Ok(SystemTx::CredisFactory)),
            Self::new("gem-cleanup", || Ok(SystemTx::GemCleanup)),
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
        let block_number = if matches!(input, SystemTx::TeeBootstrap { .. }) {
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
            (std::mem::discriminant(&decoded) == std::mem::discriminant(&prepared.input))
                .to_string(),
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
