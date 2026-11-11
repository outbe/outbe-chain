use alloy_primitives::{Address, Bytes, B256};

use crate::{
    reshare_artifact::{ConsensusHeaderArtifact, OutbeBlockArtifacts},
    storage::{
        metadosis_process_ready_binding, MetadosisMutationEntitlements, MetadosisMutationPurposeTag,
    },
    system_tx::SystemTx,
    system_tx_context::{
        SystemTxBuildContext, SystemTxContext, SystemTxPayloadBuildError,
        SystemTxPayloadValidationError, SystemTxValidationContext,
    },
};

use crate::{
    error::{PrecompileError, Result},
    storage::{StorageBacked, StorageHandle},
};

/// Error returned when a production block context is constructed with `genesis_timestamp == 0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("genesis_timestamp must be non-zero")]
pub struct ZeroGenesisTimestamp;

impl From<ZeroGenesisTimestamp> for PrecompileError {
    fn from(_: ZeroGenesisTimestamp) -> Self {
        PrecompileError::fatal("genesis_timestamp must be non-zero")
    }
}

/// Canonical protocol height regime for a block.
///
/// Ontologically partitions the chain's block numbers into three distinct
/// lifecycle regimes so callers do not scatter raw `0`, `1`, and `>= 2`
/// comparisons across consensus, payload building, and EVM execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockHeightRegime {
    /// Block 0: Canonical genesis block.
    /// Carries no transactions, no begin-zone system txs, and no execution witness.
    Genesis,
    /// Block 1: One-time network bootstrap block.
    /// Carries begin-zone system txs (mandatory epoch-0 `BoundaryOutcome`, mandatory
    /// `TeeBootstrap`, `ValidatorLiveness`, optional `RadiclePublication`, and the
    /// 9 `BEGIN_BLOCK_ORDER` modules), no parent finalization accounting
    /// (`CertifiedParentAccounting` / `LateFinalizeCredits`), and no user transactions.
    GenesisBootstrap,
    /// Block `>= 2`: Steady-state execution regime.
    /// Begins with mandatory `CertifiedParentAccounting` (ordinal 0) and
    /// `LateFinalizeCredits` (ordinal 1), followed by the remaining begin-zone
    /// system txs and user transactions.
    SteadyState,
}

impl BlockHeightRegime {
    #[must_use]
    pub const fn for_block(block_number: u64) -> Self {
        match block_number {
            0 => Self::Genesis,
            crate::system_tx::GENESIS_BOOTSTRAP_BLOCK_NUMBER => Self::GenesisBootstrap,
            _ => Self::SteadyState,
        }
    }

    #[must_use]
    pub const fn is_genesis(self) -> bool {
        matches!(self, Self::Genesis)
    }

    #[must_use]
    pub const fn is_genesis_bootstrap(self) -> bool {
        matches!(self, Self::GenesisBootstrap)
    }

    #[must_use]
    pub const fn is_steady_state(self) -> bool {
        matches!(self, Self::SteadyState)
    }

    #[must_use]
    pub const fn has_system_tx_prefix(self) -> bool {
        !matches!(self, Self::Genesis)
    }

    #[must_use]
    pub const fn has_parent_finalization_accounting(self) -> bool {
        matches!(self, Self::SteadyState)
    }

    #[must_use]
    pub const fn requires_genesis_bootstrap_txs(self) -> bool {
        matches!(self, Self::GenesisBootstrap)
    }

    #[must_use]
    pub const fn allows_user_transactions(self) -> bool {
        matches!(self, Self::SteadyState)
    }

    #[must_use]
    pub const fn has_end_block_system_txs(self) -> bool {
        matches!(self, Self::SteadyState)
    }

    #[must_use]
    pub const fn requires_rewards_beneficiary(self) -> bool {
        !matches!(self, Self::Genesis)
    }
}

/// The lifecycle executor builds this context from canonical block/header,
/// chain, and validator-set state before calling runtime modules.
#[derive(Clone, Debug)]
pub struct BlockContext {
    pub block_number: u64,
    pub timestamp: u64,
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub genesis_timestamp: u64,
    pub proposer: Address,
    pub validators: Vec<Address>,
    pub block_gas_limit: u64,
    pub user_gas_used: u64,
}

impl BlockContext {
    #[must_use]
    pub const fn regime(&self) -> BlockHeightRegime {
        BlockHeightRegime::for_block(self.block_number)
    }

    /// Gas capacity remaining in the block after user transactions.
    #[must_use]
    pub const fn remaining_block_gas(&self) -> u64 {
        self.block_gas_limit.saturating_sub(self.user_gas_used)
    }
}

#[derive(Clone)]
pub struct BlockRuntimeContext<'storage> {
    pub block: BlockContext,
    pub storage: StorageHandle<'storage>,
    pub system_tx: Option<SystemTxContext>,
}

impl<'storage> BlockRuntimeContext<'storage> {
    pub fn new(block: BlockContext, storage: StorageHandle<'storage>) -> Self {
        Self {
            block,
            storage,
            system_tx: None,
        }
    }

    #[must_use]
    pub fn with_system_tx(mut self, ctx: SystemTxContext) -> Self {
        self.system_tx = Some(ctx);
        self
    }

    pub fn system_tx(&self) -> Result<&SystemTxContext> {
        self.system_tx
            .as_ref()
            .ok_or_else(|| MissingSystemTxContext.into())
    }

    pub fn contract<C: StorageBacked<'storage>>(&self) -> C {
        self.storage.contract::<C>()
    }

    pub fn contract_at<C: StorageBacked<'storage>>(&self, address: Address) -> C {
        self.storage.contract_at::<C>(address)
    }

    pub fn with_checkpoint<R>(&self, f: impl FnOnce() -> Result<R>) -> Result<R> {
        self.storage.with_checkpoint(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("missing system_tx context")]
pub struct MissingSystemTxContext;

impl From<MissingSystemTxContext> for PrecompileError {
    fn from(error: MissingSystemTxContext) -> Self {
        Self::Revert(error.to_string())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MetadosisEntitlementContext<'a> {
    pub payload: &'a [u8],
    pub chain_id: u64,
    pub block_number: u64,
    pub timestamp: u64,
    pub cycle_active_utc_day: Option<u32>,
}

/// Unified payload and execution contract for a protocol step.
pub trait ProtocolModule {
    const NAME: &'static str;
    const NEEDS_CYCLE_ACTIVE_UTC_DAY: bool = false;

    fn build_payload(
        _ctx: &SystemTxBuildContext<'_>,
    ) -> std::result::Result<Bytes, SystemTxPayloadBuildError> {
        Ok(Bytes::new())
    }

    fn validate_payload(
        payload: &[u8],
        _ctx: &SystemTxValidationContext<'_>,
    ) -> std::result::Result<(), SystemTxPayloadValidationError> {
        if !payload.is_empty() {
            return Err(SystemTxPayloadValidationError::UnexpectedNonEmptyPayload {
                module: Self::NAME,
                len: payload.len(),
            });
        }
        Ok(())
    }

    fn mutation_entitlements(
        _ctx: MetadosisEntitlementContext<'_>,
    ) -> MetadosisMutationEntitlements {
        MetadosisMutationEntitlements::NONE
    }

    fn begin_block(_ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockSystemFlags {
    pub has_boundary_outcome: bool,
    pub has_publication: bool,
}

impl BlockSystemFlags {
    pub fn from_artifacts(artifacts: &OutbeBlockArtifacts) -> Self {
        Self {
            has_boundary_outcome: matches!(
                &artifacts.consensus_header_artifact,
                Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
            ),
            has_publication: artifacts.radicle_publication.is_some(),
        }
    }
}

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
    pub mutation_entitlements: fn(MetadosisEntitlementContext<'_>) -> MetadosisMutationEntitlements,
    pub run: fn(&BlockRuntimeContext<'_>) -> Result<()>,
}

fn always_active(_: BlockHeightRegime, _: BlockSystemFlags) -> bool {
    true
}

fn process_ready_entitlements(
    ctx: MetadosisEntitlementContext<'_>,
) -> MetadosisMutationEntitlements {
    if !ctx.payload.is_empty() {
        return MetadosisMutationEntitlements::NONE;
    }
    MetadosisMutationEntitlements::exact(
        MetadosisMutationPurposeTag::CycleLifecycle,
        metadosis_process_ready_binding(ctx.chain_id, ctx.block_number, ctx.timestamp),
    )
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
    pub const fn with_process_ready_entitlements(mut self) -> Self {
        self.mutation_entitlements = process_ready_entitlements;
        self
    }
}

impl ProtocolStep {
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
    use crate::storage::hashmap::HashMapStorageProvider;

    #[test]
    fn block_height_regime_partitions_genesis_bootstrap_and_steady_state() {
        let g = BlockHeightRegime::for_block(0);
        assert!(g.is_genesis());
        assert!(!g.is_genesis_bootstrap());
        assert!(!g.is_steady_state());
        assert!(!g.has_system_tx_prefix());
        assert!(!g.has_parent_finalization_accounting());
        assert!(!g.requires_genesis_bootstrap_txs());
        assert!(!g.allows_user_transactions());
        assert!(!g.has_end_block_system_txs());
        assert!(!g.requires_rewards_beneficiary());

        let b = BlockHeightRegime::for_block(1);
        assert!(!b.is_genesis());
        assert!(b.is_genesis_bootstrap());
        assert!(!b.is_steady_state());
        assert!(b.has_system_tx_prefix());
        assert!(!b.has_parent_finalization_accounting());
        assert!(b.requires_genesis_bootstrap_txs());
        assert!(!b.allows_user_transactions());
        assert!(!b.has_end_block_system_txs());
        assert!(b.requires_rewards_beneficiary());

        for height in [2, 3, 100, u64::MAX] {
            let s = BlockHeightRegime::for_block(height);
            assert!(!s.is_genesis());
            assert!(!s.is_genesis_bootstrap());
            assert!(s.is_steady_state());
            assert!(s.has_system_tx_prefix());
            assert!(s.has_parent_finalization_accounting());
            assert!(!s.requires_genesis_bootstrap_txs());
            assert!(s.allows_user_transactions());
            assert!(s.has_end_block_system_txs());
            assert!(s.requires_rewards_beneficiary());
        }
    }

    struct NopModule;

    impl ProtocolModule for NopModule {
        const NAME: &'static str = "nop";
    }

    fn gas_context(block_gas_limit: u64, user_gas_used: u64) -> BlockContext {
        BlockContext {
            block_number: 2,
            timestamp: 11,
            chain_id: 1,
            genesis_hash: B256::ZERO,
            genesis_timestamp: 11,
            proposer: Address::ZERO,
            validators: Vec::new(),
            block_gas_limit,
            user_gas_used,
        }
    }

    #[test]
    fn protocol_module_defaults_succeed_for_begin_and_end_block() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let ctx = BlockRuntimeContext::new(gas_context(100, 40), storage);
            assert_eq!(NopModule::NAME, "nop");
            assert!(NopModule::begin_block(&ctx).is_ok());
            assert!(NopModule::end_block(&ctx).is_ok());
        });
    }

    #[test]
    fn remaining_block_gas_saturates_at_zero() {
        assert_eq!(gas_context(100, 40).remaining_block_gas(), 60);
        assert_eq!(gas_context(100, 100).remaining_block_gas(), 0);
        assert_eq!(gas_context(100, 101).remaining_block_gas(), 0);
        assert_eq!(gas_context(0, u64::MAX).remaining_block_gas(), 0);
    }
    #[test]
    fn system_context_is_explicit_and_execution_bridge_preserves_verified_facts() {
        use crate::system_tx_context::{AccountedParentArtifact, SystemTxExecutionContext};
        use std::sync::Arc;
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let block = gas_context(100, 40);
            let ctx = BlockRuntimeContext::new(block.clone(), storage);
            assert!(
                matches!(ctx.system_tx(), Err(PrecompileError::Revert(message)) if message == "missing system_tx context")
            );
            let summary = AccountedParentArtifact {
                summary: crate::reshare_artifact::ExecutionSummaryArtifact::default(),
                timestamp: 9,
                state_root: Some(B256::repeat_byte(7)),
            };
            let signers = Arc::new(vec![vec![1, 3]]);
            let preloaded = SystemTxExecutionContext {
                proposer: Address::repeat_byte(8),
                finalized_summary: Some(summary),
                canonical_vrf_proof_hash: B256::repeat_byte(6),
                verified_late_credit_signers: Some(signers.clone()),
                block_gas_limit: 200,
                user_gas_used: 80,
            };
            let sys = SystemTxContext::from_preloaded(
                Bytes::from_static(b"payload"),
                Some(preloaded),
                None,
            );
            let execution = sys.to_execution_context(&block);
            assert_eq!(execution.proposer, block.proposer);
            assert_eq!(execution.block_gas_limit, 100);
            assert_eq!(execution.user_gas_used, 40);
            assert_eq!(execution.finalized_summary, Some(summary));
            assert_eq!(execution.canonical_vrf_proof_hash, B256::repeat_byte(6));
            assert!(Arc::ptr_eq(
                execution.verified_late_credit_signers.as_ref().unwrap(),
                &signers
            ));
            let ctx = ctx.with_system_tx(sys.clone());
            assert_eq!(ctx.system_tx().unwrap(), &sys);
            let empty = SystemTxContext::from_preloaded(Bytes::new(), None, None);
            assert_eq!(empty, SystemTxContext::default());
        });
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
        let authority = MetadosisEntitlementContext {
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
                MetadosisMutationEntitlements::NONE
            );
        }
        let process = end_step::<NopModule>().with_process_ready_entitlements();
        assert_eq!(
            (process.mutation_entitlements)(authority),
            MetadosisMutationEntitlements::exact(
                MetadosisMutationPurposeTag::CycleLifecycle,
                metadosis_process_ready_binding(1, 2, 11)
            )
        );
        assert_eq!(
            (process.mutation_entitlements)(MetadosisEntitlementContext {
                payload: b"bad",
                ..authority
            }),
            MetadosisMutationEntitlements::NONE
        );
        let cycle = crate::test_utils::TEST_BLOCK_ORDER.begin_block[8];
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
