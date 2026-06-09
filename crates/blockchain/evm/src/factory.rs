//! Custom EVM factory for Outbe.
//!
//! `OutbeEvmFactory` creates EVM instances with Outbe precompiles registered
//! via `set_precompile_lookup`. The factory is wired into reth's node via
//! `OutbeExecutorBuilder` (defined in `crate::config`).

use alloy_evm::{
    eth::EthEvmContext,
    precompiles::PrecompilesMap,
    revm::handler::{instructions::EthInstructions, EthFrame, EthPrecompiles, PrecompileProvider},
    Evm, EvmFactory,
};
use alloy_primitives::{Address, Bytes, TxKind};
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
        ExecuteEvm, InspectEvm, MainBuilder, MainContext, SystemCallEvm,
    },
};
use revm::handler::{Handler, MainnetHandler};

use crate::precompiles::extend_outbe_precompiles;

#[cfg(test)]
use reth_ethereum::evm::revm::context_interface::result::{
    ExecutionResult, OutOfGasError, ResultGas,
};

#[cfg(test)]
thread_local! {
    static FORCE_OUTBE_SYSTEM_CALL_ERROR: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    static FORCE_OUTBE_SYSTEM_CALL_OOG_HALT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    static FORCE_OUTBE_SYSTEM_CALL_REVERT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn with_forced_outbe_system_call_error<R>(f: impl FnOnce() -> R) -> R {
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

#[cfg(test)]
pub(crate) fn with_forced_outbe_system_call_oog_halt<R>(f: impl FnOnce() -> R) -> R {
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

#[cfg(test)]
pub(crate) fn with_forced_outbe_system_call_revert<R>(f: impl FnOnce() -> R) -> R {
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

/// Outbe EVM wrapper.
///
/// Upstream `EthEvm::transact_system_call` delegates to revm's EIP system-call
/// helper, which builds a 30M-gas tx. Outbe begin-zone system transactions are
/// protocol transactions with their own 10B-gas execution lane, so calls into
/// `OUTBE_SYSTEM_TX_ADDRESS` build the system-call `TxEnv` locally with
/// `SYSTEM_TX_ARTIFACT_GAS_LIMIT`. Other system calls keep upstream semantics.
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
}

impl<DB: Database, I, PRECOMPILE> OutbeEvm<DB, I, PRECOMPILE> {
    /// Creates a new Outbe EVM instance.
    pub const fn new(
        evm: RevmEvm<
            EthEvmContext<DB>,
            I,
            EthInstructions<EthInterpreter, EthEvmContext<DB>>,
            PRECOMPILE,
            EthFrame,
        >,
        inspect: bool,
    ) -> Self {
        Self {
            inner: evm,
            inspect,
        }
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
        &self.block
    }

    fn cfg_env(&self) -> &CfgEnv<Self::Spec> {
        &self.cfg
    }

    fn chain_id(&self) -> u64 {
        self.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        if self.inspect {
            self.inner.inspect_tx(tx)
        } else {
            self.inner.transact(tx)
        }
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        if contract != outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS {
            return self.inner.system_call_with_caller(caller, contract, data);
        }

        #[cfg(test)]
        if FORCE_OUTBE_SYSTEM_CALL_ERROR.with(|cell| cell.get()) {
            return Err(EVMError::Custom(
                "forced Outbe system-call error for regression test".into(),
            ));
        }
        #[cfg(test)]
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
        #[cfg(test)]
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

        let tx = TxEnv::builder()
            .caller(caller)
            .kind(TxKind::Call(contract))
            .data(data)
            .gas_limit(outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT)
            .build_fill();

        self.inner.ctx.set_tx(tx);
        let mut handler: MainnetHandler<_, EVMError<DB::Error>, EthFrame> =
            MainnetHandler::default();
        let result = handler.run_system_call(&mut self.inner)?;
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
#[derive(Clone, Debug, Default)]
pub struct OutbeEvmFactory;

impl OutbeEvmFactory {
    /// Construct the Outbe EVM factory.
    pub fn new() -> Self {
        Self
    }
}

impl EvmFactory for OutbeEvmFactory {
    type Evm<DB: Database, I: Inspector<EthEvmContext<DB>, EthInterpreter>> =
        OutbeEvm<DB, I, Self::Precompiles>;
    type Tx = TxEnv;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Context<DB: Database> = EthEvmContext<DB>;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        let spec = input.cfg_env.spec;
        let mut precompiles = PrecompilesMap::from_static(EthPrecompiles::new(spec).precompiles);

        // Register Outbe stateful precompiles via dynamic lookup.
        extend_outbe_precompiles::<DB>(&mut precompiles, spec);

        let evm = Context::mainnet()
            .with_db(db)
            .with_cfg(input.cfg_env)
            .with_block(input.block_env)
            .build_mainnet_with_inspector(NoOpInspector {})
            .with_precompiles(precompiles);

        OutbeEvm::new(evm, false)
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>, EthInterpreter>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        OutbeEvm::new(
            self.create_evm(db, input)
                .into_inner()
                .with_inspector(inspector),
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use revm::database_interface::EmptyDB;

    const USER_BLOCK_GAS_LIMIT: u64 = 30_000_000;

    fn test_env() -> EvmEnv {
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
        let factory = OutbeEvmFactory::new();
        let mut evm = factory.create_evm(EmptyDB::default(), test_env());

        let _ = evm.transact_system_call(
            outbe_primitives::addresses::SYSTEM_ADDRESS,
            outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            Bytes::new(),
        );

        assert_eq!(
            evm.ctx().tx.gas_limit,
            outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT
        );
        assert_eq!(evm.ctx().block.gas_limit, USER_BLOCK_GAS_LIMIT);
    }

    #[test]
    fn non_outbe_system_call_keeps_upstream_gas_limit() {
        let factory = OutbeEvmFactory::new();
        let mut evm = factory.create_evm(EmptyDB::default(), test_env());

        let _ = evm.transact_system_call(
            outbe_primitives::addresses::SYSTEM_ADDRESS,
            Address::repeat_byte(0x42),
            Bytes::new(),
        );

        assert_eq!(evm.ctx().tx.gas_limit, USER_BLOCK_GAS_LIMIT);
        assert_eq!(evm.ctx().block.gas_limit, USER_BLOCK_GAS_LIMIT);
    }
}
