//! Custom EVM factory for Outbe.
//!
//! `OutbeEvmFactory` creates EVM instances with Outbe precompiles registered
//! via `set_ctx_dispatch_hook` in `extend_outbe_precompiles`. The factory is
//! wired into reth's node via
//! `OutbeExecutorBuilder` (defined in `crate::config`).

use alloy_evm::{
    eth::EthEvmContext,
    precompiles::PrecompilesMap,
    revm::handler::{instructions::EthInstructions, EthFrame, EthPrecompiles, PrecompileProvider},
    Evm, EvmFactory,
};
use alloy_primitives::{Address, Bytes, TxKind, B256};
use core::ops::{Deref, DerefMut};
use reth_ethereum::evm::{
    primitives::{Database, EvmEnv},
    revm::{
        context::{BlockEnv, CfgEnv, Context, Evm as RevmEvm, TxEnv},
        context_interface::{
            result::{EVMError, HaltReason, ResultAndState},
            ContextSetters,
        },
        inspector::{Inspector, NoOpInspector},
        interpreter::{interpreter::EthInterpreter, InterpreterResult},
        primitives::hardfork::SpecId,
        ExecuteEvm, MainBuilder, MainContext,
    },
};
use revm::handler::{system_call::SystemCallTx, Handler, MainnetHandler};
use revm::inspector::InspectorHandler;

use crate::{
    create_guard::{self, ReservedNamespaceHandler},
    native_delegation::NativeDelegationEvm,
    precompiles::{extend_outbe_precompiles, OutbePrecompileExecutionContext},
    tee_attestation_activation::TeeAttestationChainSpecStateV1,
};

#[cfg(any(test, feature = "test-hooks"))]
use reth_ethereum::evm::revm::context_interface::result::{
    ExecutionResult, OutOfGasError, ResultGas,
};

#[cfg(any(test, feature = "test-hooks"))]
thread_local! {
    static FORCE_OUTBE_SYSTEM_CALL_ERROR: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    static FORCE_OUTBE_SYSTEM_CALL_OOG_HALT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    static FORCE_OUTBE_SYSTEM_CALL_REVERT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-hooks"))]
pub fn with_forced_outbe_system_call_error<R>(f: impl FnOnce() -> R) -> R {
    struct ResetForcedOutbeSystemCallError;

    impl Drop for ResetForcedOutbeSystemCallError {
        fn drop(&mut self) {
            FORCE_OUTBE_SYSTEM_CALL_ERROR.with(|cell| cell.set(false));
        }
    }

    FORCE_OUTBE_SYSTEM_CALL_ERROR.with(|cell| cell.set(true));
    let _guard = ResetForcedOutbeSystemCallError;
    f()
}

#[cfg(any(test, feature = "test-hooks"))]
pub fn with_forced_outbe_system_call_oog_halt<R>(f: impl FnOnce() -> R) -> R {
    struct ResetForcedOutbeSystemCallOogHalt;

    impl Drop for ResetForcedOutbeSystemCallOogHalt {
        fn drop(&mut self) {
            FORCE_OUTBE_SYSTEM_CALL_OOG_HALT.with(|cell| cell.set(false));
        }
    }

    FORCE_OUTBE_SYSTEM_CALL_OOG_HALT.with(|cell| cell.set(true));
    let _guard = ResetForcedOutbeSystemCallOogHalt;
    f()
}

#[cfg(any(test, feature = "test-hooks"))]
pub fn with_forced_outbe_system_call_revert<R>(f: impl FnOnce() -> R) -> R {
    struct ResetForcedOutbeSystemCallRevert;

    impl Drop for ResetForcedOutbeSystemCallRevert {
        fn drop(&mut self) {
            FORCE_OUTBE_SYSTEM_CALL_REVERT.with(|cell| cell.set(false));
        }
    }

    FORCE_OUTBE_SYSTEM_CALL_REVERT.with(|cell| cell.set(true));
    let _guard = ResetForcedOutbeSystemCallRevert;
    f()
}

#[derive(Default)]
struct DetachedTraceCleanupGuard {
    active: bool,
}

impl Drop for DetachedTraceCleanupGuard {
    fn drop(&mut self) {
        if self.active {
            crate::executor::witness::clear_detached_trace_witness();
        }
    }
}

/// Outbe EVM wrapper.
///
/// Upstream `EthEvm::transact_system_call` delegates to revm's EIP system-call
/// helper, which uses the upstream system-call gas budget. Outbe begin-zone
/// system transactions use their runtime protocol step's gas budget, defaulting
/// to `SYSTEM_TX_ARTIFACT_GAS_LIMIT` when no step is registered. Calls into
/// `OUTBE_SYSTEM_TX_ADDRESS` build the system-call `TxEnv` locally.
/// Other system calls keep upstream semantics.
#[expect(missing_debug_implementations)]
pub struct OutbeEvm<DB: Database, I, PRECOMPILE = EthPrecompiles> {
    inner: RevmEvm<
        EthEvmContext<DB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
        EthFrame,
    >,
    inspect: bool,
    abort_bridge: crate::precompiles::ExecutionAbortBridge,
    runtime_host: Option<std::sync::Arc<dyn crate::EvmRuntimeHost>>,
    owned_by_executor: bool,
    precompiles_mut_called: bool,
    witness_state: Option<std::sync::Arc<std::sync::Mutex<outbe_tee::witness::WitnessState>>>,
    detached_trace_guard: DetachedTraceCleanupGuard,
}

/// Creates a new Outbe EVM instance.
pub fn outbe_evm<DB: Database, I, PRECOMPILE>(
    evm: RevmEvm<
        EthEvmContext<DB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
        EthFrame,
    >,
    inspect: bool,
) -> OutbeEvm<DB, I, PRECOMPILE> {
    OutbeEvm {
        inner: evm,
        inspect,
        abort_bridge: crate::precompiles::ExecutionAbortBridge::default(),
        runtime_host: None,
        owned_by_executor: false,
        precompiles_mut_called: false,
        witness_state: None,
        detached_trace_guard: DetachedTraceCleanupGuard::default(),
    }
}

impl<DB: Database, I, PRECOMPILE> OutbeEvm<DB, I, PRECOMPILE> {
    fn resolve_outbe_system_call_gas_limit(&self, data: &[u8]) -> u64 {
        crate::system_tx::SystemTx::decode(data)
            .ok()
            .and_then(|sys_tx| {
                self.runtime_host
                    .as_deref()
                    .and_then(|host| host.block_order().step_for_tx(&sys_tx))
            })
            .map_or(
                outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT,
                |step| step.gas_limit,
            )
    }

    pub(crate) fn set_owned_by_executor(&mut self, owned: bool) {
        self.owned_by_executor = owned;
    }

    pub(crate) fn set_attached_witness_state(
        &mut self,
        state: Option<std::sync::Arc<std::sync::Mutex<outbe_tee::witness::WitnessState>>>,
    ) {
        self.witness_state = state;
    }
    /// Consumes self and returns the inner revm instance.
    pub fn into_inner(
        self,
    ) -> RevmEvm<
        EthEvmContext<DB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
        EthFrame,
    > {
        self.inner
    }

    /// Provides a reference to the EVM context.
    pub const fn ctx(&self) -> &EthEvmContext<DB> {
        &self.inner.ctx
    }

    /// Provides a mutable reference to the EVM context.
    pub const fn ctx_mut(&mut self) -> &mut EthEvmContext<DB> {
        &mut self.inner.ctx
    }

    fn run_inner_transact_raw(
        &mut self,
        tx: TxEnv,
    ) -> Result<ResultAndState<HaltReason>, EVMError<DB::Error>>
    where
        I: Inspector<EthEvmContext<DB>>,
        PRECOMPILE: PrecompileProvider<EthEvmContext<DB>, Output = InterpreterResult>,
    {
        let call = self.abort_bridge.begin_call();
        self.inner.ctx.set_tx(tx);
        let mut handler: ReservedNamespaceHandler<_, EVMError<DB::Error>> = Default::default();
        let host = self.runtime_host.as_deref();
        let output = if self.inspect {
            handler.inspect_run(&mut NativeDelegationEvm(&mut self.inner, host))
        } else {
            handler.run(&mut NativeDelegationEvm(&mut self.inner, host))
        };
        let state = self.inner.finalize();
        Ok(ResultAndState::new(call.finish(output)?, state))
    }
}

impl<DB: Database, I, PRECOMPILE> Deref for OutbeEvm<DB, I, PRECOMPILE> {
    type Target = EthEvmContext<DB>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.ctx()
    }
}

impl<DB: Database, I, PRECOMPILE> DerefMut for OutbeEvm<DB, I, PRECOMPILE> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx_mut()
    }
}

impl<DB, I, PRECOMPILE> Evm for OutbeEvm<DB, I, PRECOMPILE>
where
    DB: Database,
    I: Inspector<EthEvmContext<DB>>,
    PRECOMPILE: PrecompileProvider<EthEvmContext<DB>, Output = InterpreterResult>,
{
    type DB = DB;
    type Tx = TxEnv;
    type Error = EVMError<DB::Error>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PRECOMPILE;
    type Inspector = I;

    fn block(&self) -> &BlockEnv {
        &self.inner.ctx.block
    }

    fn cfg_env(&self) -> &CfgEnv<Self::Spec> {
        &self.inner.ctx.cfg
    }

    fn chain_id(&self) -> u64 {
        self.inner.ctx.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        let attached_state = self.witness_state.clone();
        let input = crate::executor::witness::EvmTransactWitnessInput {
            attached_state: attached_state.as_ref(),
            owned_by_executor: self.owned_by_executor,
            precompiles_mut_called: self.precompiles_mut_called,
            chain_id: self.chain_id(),
            block_number: self.block().number.saturating_to::<u64>(),
            block_timestamp: self.block().timestamp.saturating_to::<u64>(),
        };
        if let Some(matched) = crate::executor::witness::match_detached_trace_tx(&input, &tx) {
            self.detached_trace_guard.active = true;
            return crate::executor::witness::run_matched_detached_trace_tx(matched, |action| {
                match action {
                    crate::executor::witness::DetachedTraceTxAction::SkipPreexecutedPhase1 {
                        gas_limit,
                    } => Ok(crate::executor::witness::synthetic_trace_success(
                        gas_limit,
                        Vec::new(),
                    )),
                    crate::executor::witness::DetachedTraceTxAction::ReservedSystemCall {
                        data,
                        context,
                    } => crate::executor::with_preloaded_system_tx_context(*context, || {
                        self.transact_system_call(
                            outbe_primitives::addresses::SYSTEM_ADDRESS,
                            outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                            data,
                        )
                    }),
                    crate::executor::witness::DetachedTraceTxAction::NormalTransact => {
                        let result = self.run_inner_transact_raw(tx)?;
                        crate::executor::witness::record_detached_trace_user_gas(
                            result.result.gas().tx_gas_used(),
                        );
                        Ok(result)
                    }
                }
            });
        }
        crate::executor::witness::with_evm_transact_witness_scope(input, || {
            self.run_inner_transact_raw(tx)
        })
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        let call = self.abort_bridge.begin_call();
        if contract != outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS {
            self.inner
                .ctx
                .set_tx(TxEnv::new_system_tx_with_caller(caller, contract, data));
            let mut handler: MainnetHandler<_, EVMError<DB::Error>, EthFrame> = Default::default();
            let result = call.finish(handler.run_system_call(&mut NativeDelegationEvm(
                &mut self.inner,
                self.runtime_host.as_deref(),
            )))?;
            return Ok(ResultAndState::new(result, self.inner.finalize()));
        }

        #[cfg(any(test, feature = "test-hooks"))]
        if FORCE_OUTBE_SYSTEM_CALL_ERROR.with(|cell| cell.get()) {
            return Err(EVMError::Custom(
                "forced Outbe system-call error for regression test".into(),
            ));
        }
        #[cfg(any(test, feature = "test-hooks"))]
        if FORCE_OUTBE_SYSTEM_CALL_OOG_HALT.with(|cell| cell.get()) {
            return Ok(ResultAndState::new(
                ExecutionResult::Halt {
                    reason: HaltReason::OutOfGas(OutOfGasError::Precompile),
                    gas: ResultGas::new_with_state_gas(
                        outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT,
                        0,
                        0,
                        0,
                    ),
                    logs: Vec::new(),
                },
                Default::default(),
            ));
        }
        #[cfg(any(test, feature = "test-hooks"))]
        if FORCE_OUTBE_SYSTEM_CALL_REVERT.with(|cell| cell.get()) {
            return Ok(ResultAndState::new(
                ExecutionResult::Revert {
                    gas: ResultGas::new_with_state_gas(42_000, 0, 0, 0),
                    logs: Vec::new(),
                    output: Bytes::from_static(b"forced Outbe system-call revert"),
                },
                Default::default(),
            ));
        }

        let gas_limit = self.resolve_outbe_system_call_gas_limit(&data);
        let tx = TxEnv::builder()
            .caller(caller)
            .kind(TxKind::Call(contract))
            .data(data)
            .gas_limit(gas_limit)
            .build_fill();

        self.inner.ctx.set_tx(tx);
        let mut handler: MainnetHandler<_, EVMError<DB::Error>, EthFrame> =
            MainnetHandler::default();
        let result = call.finish(handler.run_system_call(&mut NativeDelegationEvm(
            &mut self.inner,
            self.runtime_host.as_deref(),
        )))?;
        let state = self.inner.finalize();
        Ok(ResultAndState::new(result, state))
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let Context {
            block: block_env,
            cfg: cfg_env,
            journaled_state,
            ..
        } = self.inner.ctx;

        (journaled_state.database, EvmEnv { block_env, cfg_env })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    fn precompiles_mut(&mut self) -> &mut Self::Precompiles {
        if !self.owned_by_executor {
            self.precompiles_mut_called = true;
        }
        &mut self.inner.precompiles
    }

    fn components(&self) -> (&Self::DB, &Self::Inspector, &Self::Precompiles) {
        (
            &self.inner.ctx.journaled_state.database,
            &self.inner.inspector,
            &self.inner.precompiles,
        )
    }

    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles) {
        (
            &mut self.inner.ctx.journaled_state.database,
            &mut self.inner.inspector,
            &mut self.inner.precompiles,
        )
    }
}

/// Custom EVM factory that registers Outbe stateful precompiles.
#[derive(Clone)]
pub struct OutbeEvmFactory {
    /// Immutable chain identity installed by `OutbeEvmConfig`.
    genesis_hash: B256,
    genesis_timestamp: u64,
    tee_attestation_v1: TeeAttestationChainSpecStateV1,
    /// Host installed for every EVM this factory creates.
    runtime_host: std::sync::Arc<dyn crate::EvmRuntimeHost>,
}

impl core::fmt::Debug for OutbeEvmFactory {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("OutbeEvmFactory")
            .field("genesis_hash", &self.genesis_hash)
            .field("genesis_timestamp", &self.genesis_timestamp)
            .field("tee_attestation_v1", &self.tee_attestation_v1)
            .field("runtime_host", &"installed")
            .finish()
    }
}

impl OutbeEvmFactory {
    /// Creates a factory that installs `host` on every EVM it builds.
    #[must_use]
    pub fn new(runtime_host: std::sync::Arc<dyn crate::EvmRuntimeHost>) -> Self {
        Self {
            genesis_hash: B256::ZERO,
            genesis_timestamp: 0,
            tee_attestation_v1: TeeAttestationChainSpecStateV1::default(),
            runtime_host,
        }
    }

    /// Binds every EVM/precompile context created by this factory to the
    /// canonical genesis hash from ChainSpec.
    #[must_use]
    pub fn with_genesis_hash(mut self, genesis_hash: B256) -> Self {
        self.genesis_hash = genesis_hash;
        self
    }

    /// Returns the immutable genesis hash installed for every EVM context.
    #[must_use]
    pub const fn genesis_hash(&self) -> B256 {
        self.genesis_hash
    }

    pub fn with_genesis_timestamp(mut self, genesis_timestamp: u64) -> Self {
        self.genesis_timestamp = genesis_timestamp;
        self
    }

    pub fn genesis_timestamp(&self) -> u64 {
        self.genesis_timestamp
    }

    /// Stores `host` for every EVM this factory creates.
    #[must_use]
    pub fn with_runtime_host(mut self, host: std::sync::Arc<dyn crate::EvmRuntimeHost>) -> Self {
        self.runtime_host = host;
        self
    }

    #[must_use]
    pub fn with_tee_attestation_v1(mut self, state: TeeAttestationChainSpecStateV1) -> Self {
        self.tee_attestation_v1 = state;
        self
    }
}

impl EvmFactory for OutbeEvmFactory {
    type Evm<DB: Database, I: Inspector<EthEvmContext<DB>, EthInterpreter>> =
        OutbeEvm<DB, I, Self::Precompiles>;
    type Tx = TxEnv;
    type Error<DBError: revm::context::DBErrorMarker> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Context<DB: Database> = EthEvmContext<DB>;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        let spec = input.cfg_env.spec;
        let mut precompiles = PrecompilesMap::from_static(EthPrecompiles::new(spec).precompiles);
        let abort_bridge = crate::precompiles::ExecutionAbortBridge::default();
        // Register Outbe stateful precompiles via dynamic lookup.
        extend_outbe_precompiles::<DB>(
            &mut precompiles,
            OutbePrecompileExecutionContext::new(spec, self.genesis_hash)
                .with_genesis_timestamp(self.genesis_timestamp)
                .with_tee_attestation_v1(self.tee_attestation_v1.clone()),
            crate::precompiles::OutbePrecompileRuntime::new()
                .with_abort_bridge(abort_bridge.clone()),
            std::sync::Arc::clone(&self.runtime_host),
        );

        let mut evm = Context::mainnet()
            .with_db(db)
            .with_cfg(input.cfg_env)
            .with_block(input.block_env)
            .build_mainnet_with_inspector(NoOpInspector {})
            .with_precompiles(precompiles);
        create_guard::install(&mut evm.instruction);
        crate::selfdestruct_guard::install(&mut evm.instruction);

        let mut evm = outbe_evm(evm, false);
        evm.abort_bridge = abort_bridge;
        evm.runtime_host = Some(std::sync::Arc::clone(&self.runtime_host));
        evm
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>, EthInterpreter>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let evm = self.create_evm(db, input);

        let abort_bridge = evm.abort_bridge.clone();
        let runtime_host = evm.runtime_host.clone();
        let owned_by_executor = evm.owned_by_executor;
        let precompiles_mut_called = evm.precompiles_mut_called;
        let witness_state = evm.witness_state.clone();
        let mut evm = outbe_evm(evm.into_inner().with_inspector(inspector), true);
        evm.abort_bridge = abort_bridge;
        evm.runtime_host = runtime_host;
        evm.owned_by_executor = owned_by_executor;
        evm.precompiles_mut_called = precompiles_mut_called;
        evm.witness_state = witness_state;
        evm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Address;
    use revm::database_interface::EmptyDB;

    use outbe_primitives::{
        block::{BlockHeightRegime, BlockRuntimeContext},
        committee_snapshot::CommitteeSnapshot,
        consensus::{DkgBoundaryArtifact, GenesisValidators},
        consensus_metadata::CertifiedParentAccountingMetadata,
        error::Result,
        reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit},
        storage::{MutationEntitlements, StorageHandle},
        system_tx_context::SystemTxExecutionContext,
    };

    use alloy_primitives::U256;

    struct NopModule;

    impl outbe_primitives::block::ProtocolModule for NopModule {
        const NAME: &'static str = "nop";
    }

    #[derive(Debug)]
    struct StepGasHost {
        stub: std::sync::Arc<dyn crate::EvmRuntimeHost>,
    }

    impl crate::EvmRuntimeHost for StepGasHost {
        fn block_order(&self) -> crate::system_tx::BlockOrder {
            use crate::system_tx::{begin_step, consensus_step, end_step, BlockOrder};
            const ORDER: BlockOrder = BlockOrder {
                consensus: &[consensus_step::<NopModule>().with_gas_limit(123_456)],
                begin_block: &[begin_step::<NopModule>().with_gas_limit(234_567)],
                end_block: &[end_step::<NopModule>().with_gas_limit(345_678)],
            };
            ORDER
        }

        fn marker_addresses(&self) -> &'static [Address] {
            self.stub.marker_addresses()
        }

        fn is_registered_precompile(&self, address: &Address) -> bool {
            self.stub.is_registered_precompile(address)
        }

        fn precompile_is_payable(&self, address: &Address) -> bool {
            self.stub.precompile_is_payable(address)
        }

        fn precompile_base_gas(&self, address: &Address, input: &[u8]) -> u64 {
            self.stub.precompile_base_gas(address, input)
        }

        fn dispatch_precompile(
            &self,
            storage: StorageHandle<'_>,
            address: &Address,
            data: &[u8],
            caller: Address,
            value: U256,
        ) -> Result<Bytes> {
            self.stub
                .dispatch_precompile(storage, address, data, caller, value)
        }

        fn dispatch_system_tx(
            &self,
            storage: StorageHandle<'_>,
            data: &[u8],
            caller: Address,
            value: U256,
            tee: &crate::tee_attestation_activation::TeeAttestationChainSpecStateV1,
        ) -> Result<Bytes> {
            self.stub
                .dispatch_system_tx(storage, data, caller, value, tee)
        }

        fn validate_finalized_metadata(
            &self,
            storage: StorageHandle<'_>,
            metadata: &CertifiedParentAccountingMetadata,
        ) -> Result<()> {
            self.stub.validate_finalized_metadata(storage, metadata)
        }

        fn run_finalization_and_slashing(
            &self,
            ctx: &BlockRuntimeContext<'_>,
            metadata: &CertifiedParentAccountingMetadata,
            execution: &SystemTxExecutionContext,
        ) -> Result<()> {
            self.stub
                .run_finalization_and_slashing(ctx, metadata, execution)
        }

        fn authenticate_late_credit(
            &self,
            storage: &StorageHandle<'_>,
            credit: &PerBlockCredit,
        ) -> Result<()> {
            self.stub.authenticate_late_credit(storage, credit)
        }

        fn run_late_finalize_credits(
            &self,
            ctx: &BlockRuntimeContext<'_>,
            artifact: &LateFinalizeCreditsArtifact,
            execution: Option<&SystemTxExecutionContext>,
        ) -> Result<()> {
            self.stub
                .run_late_finalize_credits(ctx, artifact, execution)
        }

        fn apply_boundary_outcome(
            &self,
            storage: StorageHandle<'_>,
            boundary: &DkgBoundaryArtifact,
            block_number: u64,
            timestamp: u64,
        ) -> Result<()> {
            self.stub
                .apply_boundary_outcome(storage, boundary, block_number, timestamp)
        }

        fn prepare_boundary(
            &self,
            storage: StorageHandle<'_>,
            boundary: &DkgBoundaryArtifact,
            block_number: u64,
        ) -> Result<()> {
            self.stub.prepare_boundary(storage, boundary, block_number)
        }

        fn consensus_validators(&self, storage: StorageHandle<'_>) -> Result<Vec<Address>> {
            self.stub.consensus_validators(storage)
        }

        fn is_consensus_participant(
            &self,
            storage: StorageHandle<'_>,
            proposer: Address,
        ) -> Result<bool> {
            self.stub.is_consensus_participant(storage, proposer)
        }

        fn is_validator(&self, storage: StorageHandle<'_>, proposer: Address) -> Result<bool> {
            self.stub.is_validator(storage, proposer)
        }

        fn read_committee_snapshot(
            &self,
            storage: StorageHandle<'_>,
            key: B256,
        ) -> Result<Option<CommitteeSnapshot>> {
            self.stub.read_committee_snapshot(storage, key)
        }

        fn validate_genesis_state_for_regime(
            &self,
            regime: BlockHeightRegime,
            storage: StorageHandle<'_>,
            genesis_validators: Option<&GenesisValidators>,
        ) -> Result<()> {
            self.stub
                .validate_genesis_state_for_regime(regime, storage, genesis_validators)
        }

        fn validate_genesis_state(
            &self,
            storage: StorageHandle<'_>,
            genesis: &GenesisValidators,
        ) -> Result<()> {
            self.stub.validate_genesis_state(storage, genesis)
        }

        fn proposer_attestation(
            &self,
            storage: StorageHandle<'_>,
            proposer: Address,
        ) -> Result<Option<[u8; 32]>> {
            self.stub.proposer_attestation(storage, proposer)
        }

        fn mutation_entitlements(
            &self,
            _call: crate::precompiles::PrecompileMutationCall<'_>,
        ) -> MutationEntitlements {
            MutationEntitlements::NONE
        }

        fn is_protocol_cycle_call(&self, address: Address, caller: Address, data: &[u8]) -> bool {
            self.stub.is_protocol_cycle_call(address, caller, data)
        }

        fn derive_active_utc_day(
            &self,
            storage: StorageHandle<'_>,
            genesis_timestamp: u64,
            block_timestamp: u64,
        ) -> Result<u32> {
            self.stub
                .derive_active_utc_day(storage, genesis_timestamp, block_timestamp)
        }

        fn settle_block_burned_fees(&self, storage: StorageHandle<'_>, amount: U256) -> Result<()> {
            self.stub.settle_block_burned_fees(storage, amount)
        }

        fn user_fee_policy(&self) -> &dyn crate::UserTxFeePolicy {
            self.stub.user_fee_policy()
        }
    }

    #[test]
    fn outbe_system_call_resolves_step_gas_limits_and_fallbacks() {
        use crate::system_tx::SystemTx;

        let host = std::sync::Arc::new(StepGasHost {
            stub: crate::host::stub_host(),
        });
        let factory = OutbeEvmFactory::new(host);
        let mut evm = factory.create_evm(EmptyDB::default(), test_env());
        let fallback = outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT;
        for (data, expected) in [
            (
                SystemTx::Consensus {
                    step_index: 0,
                    payload: Bytes::new(),
                }
                .encode()
                .unwrap(),
                123_456,
            ),
            (
                SystemTx::BeginBlock { step_index: 0 }.encode().unwrap(),
                234_567,
            ),
            (
                SystemTx::EndBlock { step_index: 0 }.encode().unwrap(),
                345_678,
            ),
            (
                SystemTx::BeginBlock { step_index: 99 }.encode().unwrap(),
                fallback,
            ),
            (Bytes::from_static(b"unknown"), fallback),
        ] {
            let _ = evm.transact_system_call(
                outbe_primitives::addresses::SYSTEM_ADDRESS,
                outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                data,
            );
            assert_eq!(evm.ctx().tx.gas_limit, expected);
            assert_eq!(evm.ctx().block.gas_limit, USER_BLOCK_GAS_LIMIT);
        }
        evm.runtime_host = None;
        let _ = evm.transact_system_call(
            outbe_primitives::addresses::SYSTEM_ADDRESS,
            outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            SystemTx::BeginBlock { step_index: 0 }.encode().unwrap(),
        );
        assert_eq!(evm.ctx().tx.gas_limit, fallback);
        assert_eq!(evm.ctx().block.gas_limit, USER_BLOCK_GAS_LIMIT);
    }

    const USER_BLOCK_GAS_LIMIT: u64 = 30_000_000;

    pub(super) fn test_env() -> EvmEnv {
        EvmEnv {
            cfg_env: CfgEnv::new()
                .with_chain_id(1)
                .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
            block_env: BlockEnv {
                gas_limit: USER_BLOCK_GAS_LIMIT,
                ..Default::default()
            },
        }
    }

    #[test]
    fn outbe_system_call_uses_artifact_gas_limit_without_changing_block_limit() {
        let factory = OutbeEvmFactory::new(crate::host::stub_host());
        let mut evm = factory.create_evm(EmptyDB::default(), test_env());

        let caller = outbe_primitives::addresses::SYSTEM_ADDRESS;
        let target = outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS;
        let data = Bytes::from_static(b"system call input");
        // A preceding user transaction must not leak fields into the system call.
        evm.ctx_mut().tx = TxEnv {
            nonce: 42,
            value: alloy_primitives::U256::from(99),
            gas_price: 123,
            ..Default::default()
        };
        let _ = evm.transact_system_call(caller, target, data.clone());

        assert_eq!(
            evm.ctx().tx.gas_limit,
            outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT
        );
        assert_eq!(evm.ctx().block.gas_limit, USER_BLOCK_GAS_LIMIT);
        assert_eq!(evm.ctx().tx.caller, caller);
        assert_eq!(evm.ctx().tx.kind, TxKind::Call(target));
        assert_eq!(evm.ctx().tx.data, data);
        assert_eq!(evm.ctx().tx.value, alloy_primitives::U256::ZERO);
        assert_eq!(evm.ctx().tx.gas_price, 0);
        assert_eq!(evm.ctx().tx.nonce, 0);
        assert!(!evm.ctx().cfg.disable_nonce_check);
    }

    #[test]
    fn non_outbe_system_call_keeps_upstream_gas_limit() {
        let factory = OutbeEvmFactory::new(crate::host::stub_host());
        let mut evm = factory.create_evm(EmptyDB::default(), test_env());

        let _ = evm.transact_system_call(
            outbe_primitives::addresses::SYSTEM_ADDRESS,
            Address::repeat_byte(0x42),
            Bytes::new(),
        );

        assert_eq!(
            evm.ctx().tx.gas_limit,
            revm::handler::system_call::SYSTEM_CALL_GAS_LIMIT
        );
        assert_eq!(evm.ctx().block.gas_limit, USER_BLOCK_GAS_LIMIT);
    }
}
