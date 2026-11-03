use std::time::Instant;

use alloy_primitives::{address, Bytes, B256};
use outbe_primitives::{
    consensus::DkgBoundaryArtifact,
    consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
    reshare_artifact::LateFinalizeCreditsArtifact,
    system_tx::{
        protocol_block_gas_limit, SystemTx, SystemTxVisibleGasPlan, OUTBE_SYSTEM_TX_ADDRESS,
        SYSTEM_TX_VISIBLE_GAS_FLOOR,
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
    pub const fn all() -> [Self; 8] {
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
            Self::new("cycle-tick", || Ok(SystemTx::CycleTick)),
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
            Self::new("oracle-slash-window", || Ok(SystemTx::OracleSlashWindow)),
            Self::new("hook-events", || Ok(SystemTx::HookEvents)),
            Self::new("radicle-publication", || {
                Ok(SystemTx::RadiclePublication {
                    batch: outbe_primitives::test_utils::sample_radicle_publication_batch(32),
                })
            }),
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

        let gas_plan_started = Instant::now();
        let plan = SystemTxVisibleGasPlan::new(
            protocol_block_gas_limit(prepared.block_number),
            std::slice::from_ref(&calldata),
        )
        .map_err(|error| error.to_string())?;
        let gas_plan_ns = elapsed_ns(gas_plan_started);

        let calldata_stats = CalldataStats::ethereum(calldata.as_ref());
        let intrinsic = plan
            .intrinsic_gas(0)
            .ok_or("system tx visible gas plan omitted entry 0")?;
        let protocol_precharge = plan
            .protocol_precharge(0)
            .ok_or("system tx visible gas plan omitted entry 0 precharge")?;
        let visible_total = intrinsic
            .checked_add(protocol_precharge)
            .ok_or("system tx visible gas total overflow")?;
        let gas_limit = plan
            .gas_limit(0)
            .ok_or("system tx visible gas plan omitted entry 0 gas limit")?;
        let execution_gas_limit = plan
            .execution_gas_limit(0)
            .map_err(|err| err.to_string())?
            .ok_or("system tx visible gas plan omitted entry 0 execution gas limit")?;

        let mut observation = Observation::new(
            [
                (GasLedger::SystemVisible, visible_total),
                (GasLedger::SystemInternal, 0),
            ],
            [
                GasComponent::new(
                    GasLedger::SystemVisible,
                    "visible.transaction_base",
                    SYSTEM_TX_VISIBLE_GAS_FLOOR,
                    1,
                ),
                GasComponent::new(
                    GasLedger::SystemVisible,
                    "visible.zero_calldata_bytes",
                    calldata_stats.zero_byte_gas,
                    calldata_stats.zero_bytes,
                ),
                GasComponent::new(
                    GasLedger::SystemVisible,
                    "visible.nonzero_calldata_bytes",
                    calldata_stats.nonzero_byte_gas,
                    calldata_stats.nonzero_bytes,
                ),
                GasComponent::new(
                    GasLedger::SystemVisible,
                    "visible.protocol_precharge",
                    protocol_precharge,
                    u64::from(protocol_precharge != 0),
                ),
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
        .with_latency("rust/visible_gas_plan", gas_plan_ns)
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
        .with_postcondition(
            "system_tx.visible_charge_conserved",
            (visible_total == intrinsic.saturating_add(protocol_precharge)).to_string(),
        )
        .with_postcondition(
            "system_tx.full_execution_gas",
            concat!("unavailable in Rust-only v1; TO", "DO outbe-chain-6le.7"),
        )
        .with_postcondition("system_tx.internal_result_is_production_total", "false")
        .with_artifact("visible.gas_limit", gas_limit.to_string())
        .with_artifact(
            "visible.execution_gas_limit",
            execution_gas_limit.to_string(),
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
