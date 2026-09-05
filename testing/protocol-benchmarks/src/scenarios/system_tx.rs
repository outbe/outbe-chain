use std::time::Instant;

use alloy_primitives::{address, Bytes, B256};
use outbe_primitives::{
    consensus::{DkgBoundaryArtifact, ReshareResult},
    consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
    reshare_artifact::LateFinalizeCreditsArtifact,
    system_tx::{
        protocol_block_gas_limit, SystemTxInputV2, SystemTxKind, SystemTxVisibleGasPlan,
        OUTBE_SYSTEM_TX_ADDRESS, SYSTEM_TX_VISIBLE_GAS_FLOOR,
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
    kind: SystemTxKind,
}

#[derive(Clone)]
pub struct PreparedSystemTx {
    input: SystemTxInputV2,
    block_number: u64,
}

impl SystemTxScenario {
    #[must_use]
    pub const fn all() -> [Self; 10] {
        [
            Self::new(SystemTxKind::CertifiedParentAccounting),
            Self::new(SystemTxKind::LateFinalizeCredits),
            Self::new(SystemTxKind::OcompLifecycleBegin),
            Self::new(SystemTxKind::CycleTick),
            Self::new(SystemTxKind::RewardsGemDelivery),
            Self::new(SystemTxKind::BoundaryOutcome),
            Self::new(SystemTxKind::TeeBootstrap),
            Self::new(SystemTxKind::OracleSlashWindow),
            Self::new(SystemTxKind::HookEvents),
            Self::new(SystemTxKind::OcompTerminalRequest),
        ]
    }

    #[must_use]
    pub const fn new(kind: SystemTxKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> SystemTxKind {
        self.kind
    }
}

impl BenchmarkScenario for SystemTxScenario {
    type Prepared = PreparedSystemTx;

    fn metadata(&self) -> ScenarioMetadata {
        let name = kind_name(self.kind);
        ScenarioMetadata::new(
            format!("system-tx/{name}/rust-wire/single"),
            format!("System tx {name} (Rust wire/accounting only)"),
            ExecutionClass::SystemTransaction,
            Profile::Single,
        )
        .with_fidelity(ScenarioFidelity::PartialStubbed)
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        Ok(PreparedSystemTx {
            input: input_for(self.kind)?,
            block_number: if self.kind == SystemTxKind::TeeBootstrap {
                BOOTSTRAP_BLOCK_NUMBER
            } else {
                STEADY_BLOCK_NUMBER
            },
        })
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        let total_started = Instant::now();

        let encode_started = Instant::now();
        let calldata = prepared.input.encode().map_err(|error| error.to_string())?;
        let encode_ns = elapsed_ns(encode_started);

        let decode_started = Instant::now();
        let decoded =
            SystemTxInputV2::decode(calldata.as_ref()).map_err(|error| error.to_string())?;
        let decode_ns = elapsed_ns(decode_started);

        let gas_plan_started = Instant::now();
        let plan = SystemTxVisibleGasPlan::new(
            protocol_block_gas_limit(prepared.block_number),
            &[(self.kind, calldata.clone())],
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
        let ce_gas_limit = plan
            .ce_gas_limit(0)
            .ok_or("system tx visible gas plan omitted entry 0 CE budget")?;

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
            (decoded.kind() == self.kind).to_string(),
        )
        .with_postcondition(
            "system_tx.body_zone",
            format!("{:?}", self.kind.body_zone()),
        )
        .with_postcondition(
            "system_tx.visible_charge_conserved",
            (visible_total == intrinsic.saturating_add(protocol_precharge)).to_string(),
        )
        .with_postcondition(
            "system_tx.full_execution_gas",
            "unavailable in Rust-only v1; TODO outbe-chain-6le.7",
        )
        .with_postcondition("system_tx.internal_result_is_production_total", "false")
        .with_artifact("visible.gas_limit", gas_limit.to_string())
        .with_artifact("visible.ce_gas_limit", ce_gas_limit.to_string())
        .with_artifact(
            "visible.block_gas_limit",
            protocol_block_gas_limit(prepared.block_number).to_string(),
        );
        observation.child_frames.push(ChildFrameTrace {
            label: "production system-tx execution".to_owned(),
            target: format!("{OUTBE_SYSTEM_TX_ADDRESS:?}"),
            selector: format!("0x{}", hex::encode(self.kind.selector())),
            status: "excluded; TODO outbe-chain-6le.7".to_owned(),
            gas_used: 0,
            fidelity: ChildFrameFidelity::BenchmarkStub,
        });
        Ok(observation)
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

const fn kind_name(kind: SystemTxKind) -> &'static str {
    match kind {
        SystemTxKind::CertifiedParentAccounting => "certified-parent-accounting",
        SystemTxKind::LateFinalizeCredits => "late-finalize-credits",
        SystemTxKind::OcompLifecycleBegin => "ocomp-lifecycle-begin",
        SystemTxKind::CycleTick => "cycle-tick",
        SystemTxKind::RewardsGemDelivery => "rewards-gem-delivery",
        SystemTxKind::BoundaryOutcome => "boundary-outcome",
        SystemTxKind::TeeBootstrap => "tee-bootstrap",
        SystemTxKind::OracleSlashWindow => "oracle-slash-window",
        SystemTxKind::HookEvents => "hook-events",
        SystemTxKind::OcompTerminalRequest => "ocomp-terminal-request",
    }
}

fn input_for(kind: SystemTxKind) -> Result<SystemTxInputV2, String> {
    Ok(match kind {
        SystemTxKind::CertifiedParentAccounting => SystemTxInputV2::CertifiedParentAccounting {
            metadata: sample_metadata(),
        },
        SystemTxKind::LateFinalizeCredits => SystemTxInputV2::LateFinalizeCredits {
            artifact: LateFinalizeCreditsArtifact::default(),
        },
        SystemTxKind::OcompLifecycleBegin => SystemTxInputV2::OcompLifecycleBegin,
        SystemTxKind::CycleTick => SystemTxInputV2::CycleTick,
        SystemTxKind::RewardsGemDelivery => SystemTxInputV2::RewardsGemDelivery,
        SystemTxKind::BoundaryOutcome => SystemTxInputV2::BoundaryOutcome {
            artifact: sample_boundary(),
        },
        SystemTxKind::TeeBootstrap => SystemTxInputV2::TeeBootstrap {
            payload: sample_tee_bootstrap()?,
        },
        SystemTxKind::OracleSlashWindow => SystemTxInputV2::OracleSlashWindow,
        SystemTxKind::HookEvents => SystemTxInputV2::HookEvents,
        SystemTxKind::OcompTerminalRequest => SystemTxInputV2::OcompTerminalRequest,
    })
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
    DkgBoundaryArtifact {
        epoch: 8,
        dkg_cycle: 2,
        freeze_height: STEADY_BLOCK_NUMBER - 2,
        planned_activation_height: STEADY_BLOCK_NUMBER,
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
            new_active_set: vec![address!("0x3333333333333333333333333333333333333333")],
            active_set_hash: B256::repeat_byte(0x55),
        },
    }
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
