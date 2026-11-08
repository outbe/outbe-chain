use alloy_primitives::{Address, B256};

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
    pub const fn has_begin_zone_system_txs(self) -> bool {
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
    pub const fn has_end_zone_system_txs(self) -> bool {
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
}

impl<'storage> BlockRuntimeContext<'storage> {
    pub fn new(block: BlockContext, storage: StorageHandle<'storage>) -> Self {
        Self { block, storage }
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

/// Unified block lifecycle interface for protocol modules.
pub trait ProtocolModule {
    /// Unique module identifier for diagnostics and tracing.
    const NAME: &'static str;

    /// Executed during the `BeginBlock` stage.
    fn begin_block(_ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        Ok(())
    }

    /// Executed during the `EndBlock` stage.
    fn end_block(_ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        Ok(())
    }
}

/// Static lifecycle contract for deterministic block-boundary runtime modules.
///
/// Implementations should be zero-sized marker types. The executor keeps the
/// ordering explicit and calls implementations through this trait instead of
/// passing ad hoc `(timestamp, block_number, ...)` argument lists.
pub trait BlockLifecycle {
    /// Typed runtime authority required by this lifecycle. Ordinary modules
    /// use [`BlockRuntimeContext`]; modules that need additional explicit
    /// capabilities wrap it in a dedicated context instead of bypassing the
    /// lifecycle contract with an ad-hoc entrypoint.
    type Context<'a, 'storage>;

    /// Value produced after all lifecycle-owned end-block work succeeds.
    ///
    /// Lifecycles without an associated output use `()`.
    type EndBlockResult;

    fn begin_block(_ctx: &Self::Context<'_, '_>) -> Result<()> {
        Ok(())
    }

    fn end_block(ctx: &Self::Context<'_, '_>) -> Result<Self::EndBlockResult>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::hashmap::HashMapStorageProvider;

    struct OutputLifecycle;

    impl BlockLifecycle for OutputLifecycle {
        type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
        type EndBlockResult = u64;

        fn end_block(ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
            Ok(ctx.block.block_number)
        }
    }

    #[test]
    fn lifecycle_end_block_can_return_an_associated_output() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext {
                    block_number: 7,
                    timestamp: 11,
                    chain_id: 1,
                    genesis_hash: B256::ZERO,
                    genesis_timestamp: 11,
                    proposer: Address::ZERO,
                    validators: Vec::new(),
                    block_gas_limit: 0,
                    user_gas_used: 0,
                },
                storage,
            );

            assert_eq!(OutputLifecycle::end_block(&ctx).unwrap(), 7);
        });
    }

    #[test]
    fn block_height_regime_partitions_genesis_bootstrap_and_steady_state() {
        let g = BlockHeightRegime::for_block(0);
        assert!(g.is_genesis());
        assert!(!g.is_genesis_bootstrap());
        assert!(!g.is_steady_state());
        assert!(!g.has_begin_zone_system_txs());
        assert!(!g.has_parent_finalization_accounting());
        assert!(!g.requires_genesis_bootstrap_txs());
        assert!(!g.allows_user_transactions());
        assert!(!g.has_end_zone_system_txs());
        assert!(!g.requires_rewards_beneficiary());

        let b = BlockHeightRegime::for_block(1);
        assert!(!b.is_genesis());
        assert!(b.is_genesis_bootstrap());
        assert!(!b.is_steady_state());
        assert!(b.has_begin_zone_system_txs());
        assert!(!b.has_parent_finalization_accounting());
        assert!(b.requires_genesis_bootstrap_txs());
        assert!(!b.allows_user_transactions());
        assert!(!b.has_end_zone_system_txs());
        assert!(b.requires_rewards_beneficiary());

        for height in [2, 3, 100, u64::MAX] {
            let s = BlockHeightRegime::for_block(height);
            assert!(!s.is_genesis());
            assert!(!s.is_genesis_bootstrap());
            assert!(s.is_steady_state());
            assert!(s.has_begin_zone_system_txs());
            assert!(s.has_parent_finalization_accounting());
            assert!(!s.requires_genesis_bootstrap_txs());
            assert!(s.allows_user_transactions());
            assert!(s.has_end_zone_system_txs());
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
}
