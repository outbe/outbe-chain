use super::SystemTx;
use alloy_primitives::Bytes;
use outbe_primitives::{
    block::{
        BlockHeightRegime, BlockRuntimeContext, BlockSystemFlags, MutationEntitlementContext,
        ProtocolModule,
    },
    error::Result,
    storage::MutationEntitlements,
    system_tx_context::{
        SystemTxBuildContext, SystemTxPayloadBuildError, SystemTxPayloadValidationError,
        SystemTxValidationContext,
    },
};

#[derive(Clone, Copy, Debug)]
pub struct ProtocolStep {
    pub name: &'static str,
    pub revert_fails_block: bool,
    pub include_validator_snapshot: bool,
    pub needs_cycle_active_utc_day: bool,
    pub is_active: fn(BlockHeightRegime, BlockSystemFlags) -> bool,
    pub build_payload:
        fn(&SystemTxBuildContext<'_>) -> std::result::Result<Bytes, SystemTxPayloadBuildError>,
    pub validate_payload: fn(
        &[u8],
        &SystemTxValidationContext<'_>,
    ) -> std::result::Result<(), SystemTxPayloadValidationError>,
    pub mutation_entitlements: fn(MutationEntitlementContext<'_>) -> MutationEntitlements,
    pub run: fn(&BlockRuntimeContext<'_>) -> Result<()>,
}

fn always_active(_: BlockHeightRegime, _: BlockSystemFlags) -> bool {
    true
}

pub const fn consensus_step<M: ProtocolModule>() -> ProtocolStep {
    begin_step::<M>()
}

pub const fn begin_step<M: ProtocolModule>() -> ProtocolStep {
    ProtocolStep {
        name: M::NAME,
        revert_fails_block: true,
        include_validator_snapshot: false,
        needs_cycle_active_utc_day: M::NEEDS_CYCLE_ACTIVE_UTC_DAY,
        is_active: always_active,
        build_payload: M::build_payload,
        validate_payload: M::validate_payload,
        mutation_entitlements: M::mutation_entitlements,
        run: M::begin_block,
    }
}

pub const fn end_step<M: ProtocolModule>() -> ProtocolStep {
    ProtocolStep {
        name: M::NAME,
        revert_fails_block: false,
        include_validator_snapshot: false,
        needs_cycle_active_utc_day: M::NEEDS_CYCLE_ACTIVE_UTC_DAY,
        is_active: always_active,
        build_payload: M::build_payload,
        validate_payload: M::validate_payload,
        mutation_entitlements: M::mutation_entitlements,
        run: M::end_block,
    }
}

impl ProtocolStep {
    #[must_use]
    pub const fn with_mutation_entitlements(
        mut self,
        f: fn(MutationEntitlementContext<'_>) -> MutationEntitlements,
    ) -> Self {
        self.mutation_entitlements = f;
        self
    }

    #[must_use]
    pub const fn with_validators(mut self) -> Self {
        self.include_validator_snapshot = true;
        self
    }

    #[must_use]
    pub const fn soft_fail(mut self) -> Self {
        self.revert_fails_block = false;
        self
    }

    #[must_use]
    pub const fn active_when(
        mut self,
        pred: fn(BlockHeightRegime, BlockSystemFlags) -> bool,
    ) -> Self {
        self.is_active = pred;
        self
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BlockOrder {
    pub consensus: &'static [ProtocolStep],
    pub begin_block: &'static [ProtocolStep],
    pub end_block: &'static [ProtocolStep],
}

impl BlockOrder {
    pub fn step_for_tx(&self, tx: &SystemTx) -> Option<&'static ProtocolStep> {
        let steps = match tx {
            SystemTx::Consensus { .. } => self.consensus,
            SystemTx::BeginBlock { .. } => self.begin_block,
            SystemTx::EndBlock { .. } => self.end_block,
        };
        steps.get(usize::from(tx.step_index()))
    }

    pub fn step_name_or_stage(&self, tx: &SystemTx) -> &'static str {
        self.step_for_tx(tx)
            .map_or_else(|| tx.stage_name(), |step| step.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use outbe_primitives::{
        reshare_artifact::OutbeBlockArtifacts,
        storage::{scoped_block_binding, MutationPurposeTag},
    };

    struct NopModule;
    impl ProtocolModule for NopModule {
        const NAME: &'static str = "nop";
    }

    fn test_step_entitlements(ctx: MutationEntitlementContext<'_>) -> MutationEntitlements {
        if !ctx.payload.is_empty() {
            return MutationEntitlements::NONE;
        }
        MutationEntitlements::exact(
            MutationPurposeTag::CycleLifecycle,
            scoped_block_binding(
                b"TEST_END_STEP_V1",
                ctx.chain_id,
                ctx.block_number,
                ctx.timestamp,
            ),
        )
    }

    #[test]
    fn protocol_steps_copy_metadata_and_default_to_no_mutation_authority() {
        let artifacts = OutbeBlockArtifacts::default();
        let build = SystemTxBuildContext {
            regime: BlockHeightRegime::SteadyState,
            block_number: 2,
            parent_hash: B256::ZERO,
            artifacts: &artifacts,
            parent_consensus_metadata: None,
            pending_tee_bootstrap: None,
            expected_tee_policy: None,
        };
        let validation = SystemTxValidationContext {
            regime: build.regime,
            block_number: 2,
            parent_hash: B256::ZERO,
            artifacts: &artifacts,
        };
        let authority = MutationEntitlementContext {
            payload: &[],
            chain_id: 1,
            block_number: 2,
            timestamp: 11,
            cycle_active_utc_day: None,
        };
        for step in [
            consensus_step::<NopModule>(),
            begin_step::<NopModule>(),
            end_step::<NopModule>(),
        ] {
            assert_eq!((step.build_payload)(&build).unwrap(), Bytes::new());
            assert!((step.validate_payload)(&[], &validation).is_ok());
            assert!(matches!(
                (step.validate_payload)(b"unexpected", &validation),
                Err(SystemTxPayloadValidationError::UnexpectedNonEmptyPayload {
                    module: "nop",
                    len: 10
                })
            ));
            assert_eq!(
                (step.mutation_entitlements)(authority),
                MutationEntitlements::NONE
            );
        }
        let process = end_step::<NopModule>().with_mutation_entitlements(test_step_entitlements);
        assert_eq!(
            (process.mutation_entitlements)(authority),
            MutationEntitlements::exact(
                MutationPurposeTag::CycleLifecycle,
                scoped_block_binding(b"TEST_END_STEP_V1", 1, 2, 11),
            )
        );
        assert_eq!(
            (process.mutation_entitlements)(MutationEntitlementContext {
                payload: b"bad",
                ..authority
            }),
            MutationEntitlements::NONE
        );
        let cycle = crate::system_tx::TEST_BLOCK_ORDER.begin_block[8];
        assert!(cycle.needs_cycle_active_utc_day);
        assert_eq!(
            BlockOrder::default().step_name_or_stage(&SystemTx::BeginBlock { step_index: 99 }),
            "BeginBlock"
        );
        assert!(BlockOrder::default()
            .step_for_tx(&SystemTx::EndBlock { step_index: 255 })
            .is_none());
    }
}
