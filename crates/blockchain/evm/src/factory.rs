//! Custom EVM factory and `evm2` type family for Outbe.
//!
//! `OutbeEvmFactory` constructs concrete [`OutbeEvm`] (`evm2::Evm<'a, OutbeEvmTypes>`)
//! instances wired with [`OutbePrecompileProvider`], safe opcode guards
//! (`create_guard`, `selfdestruct_guard`, `native_delegation`), and Outbe's
//! transaction registry (`outbe_tx_registry`).

use alloy_consensus::Transaction as _;
use alloy_primitives::{Address, Bytes, B256, U256};
use evm2::{
    env::TxEnvExt,
    ethereum::{
        default_settle_gas, effective_gas_price, eip1559, eip2930, eip4844, eip7702,
        execute_initial_frame, halted_initial_frame, initial_gas_and_reservoir, legacy,
        prepare_initial_frame, LazyTxEip7702, PreparedTx, TxEnvelope,
    },
    evm::{
        handler::{DefaultTxHandlerHooks, GasSettlement, TxHandlerHooks},
        DynDatabase, SystemTx as EvmSystemTx,
    },
    interpreter::{GasTracker, MessageResult},
    registry::{HandlerResult, TxHandler, TxRegistry, TxRequest},
    utils::b256_to_word,
    Evm, EvmConfig, EvmConfigSelector, EvmFeatures, EvmTypesHost, ExecutionConfig, OpcodeConfig,
    SpecId, TxResult, Version,
};
#[cfg(any(test, feature = "test-hooks"))]
use evm2::{interpreter::InstrStop, registry::HandlerError, TxResultExt};
use reth_ethereum::evm::EthEvmEnv;
use std::sync::{Arc, Mutex};

use crate::{
    create_guard::{GuardedCreate, TopLevelCreateGuard},
    native_delegation::{normalize_delegated_native_call, GuardedNativeMarkerOpcode},
    precompiles::{
        ExecutionAbortBridge, OutbePrecompileExecutionContext, OutbePrecompileProvider,
        OutbePrecompileRuntime,
    },
    selfdestruct_guard::GuardedSelfdestruct,
    tee_attestation_activation::TeeAttestationChainSpecStateV1,
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

/// Intrinsic/floor gas values and pre-bump sender nonce captured around `prepare_with_hooks`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CapturedPreparedGas {
    pub(crate) intrinsic: u64,
    pub(crate) initial_state_gas: u64,
    pub(crate) floor_gas: u64,
    pub(crate) sender_nonce: u64,
}

/// Outbe instance-specific extension state stored inside `evm2::Evm<'a, OutbeEvmTypes>`.
#[derive(Default)]
pub struct OutbeEvmExt {
    pub(crate) abort_bridge: ExecutionAbortBridge,
    pub(crate) runtime_host: Option<Arc<dyn crate::EvmRuntimeHost>>,
    pub(crate) owned_by_executor: bool,
    pub(crate) precompiles_mut_called: bool,
    pub(crate) witness_state: Option<Arc<Mutex<outbe_tee::witness::WitnessState>>>,
    detached_trace_guard: DetachedTraceCleanupGuard,
    pub(crate) native_frame_depth: usize,
    #[cfg(test)]
    pub(crate) max_native_frame_depth: usize,
    pub(crate) current_call_depth: u16,
    pub(crate) captured_prepared_gas: Option<CapturedPreparedGas>,
    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) last_system_call: Option<EvmSystemTx>,
}

impl std::fmt::Debug for OutbeEvmExt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutbeEvmExt")
            .field("owned_by_executor", &self.owned_by_executor)
            .field("precompiles_mut_called", &self.precompiles_mut_called)
            .field("runtime_host", &self.runtime_host.is_some())
            .field("witness_state", &self.witness_state.is_some())
            .field("native_frame_depth", &self.native_frame_depth)
            .field("current_call_depth", &self.current_call_depth)
            .finish()
    }
}

impl OutbeEvmExt {
    pub(crate) fn set_owned_by_executor(&mut self, owned: bool) {
        self.owned_by_executor = owned;
    }

    pub(crate) fn set_attached_witness_state(
        &mut self,
        state: Option<Arc<Mutex<outbe_tee::witness::WitnessState>>>,
    ) {
        self.witness_state = state;
    }
}

/// Runtime `evm2` type family for Outbe.
#[derive(Clone, Copy)]
#[allow(missing_debug_implementations)]
pub struct OutbeEvmTypes(());

impl EvmTypesHost for OutbeEvmTypes {
    type ConfigSelector = OutbeEvmConfigSelector;
    type SpecId = SpecId;
    type Tx = TxEnvelope;
    type EvmExt = OutbeEvmExt;
    type MessageExt = ();
    type MessageResultExt = ();
    type TxEnvExt = ();
    type TxResultExt = ();
    type BlockEnvExt = ();
    type Host<'a> = Evm<'a, Self>;
}

/// Concrete Outbe EVM instance backed directly by `evm2::Evm`.
pub type OutbeEvm<'a> = Evm<'a, OutbeEvmTypes>;

/// Compile-time opcode configuration for Outbe on top of `BASE_SPEC_ID`.
#[allow(missing_copy_implementations, missing_debug_implementations)]
pub struct OutbeOpcodeConfig<const BASE_SPEC_ID: u32>(());

const fn build_outbe_opcode_config<const BASE_SPEC_ID: u32>() -> OpcodeConfig<OutbeEvmTypes> {
    use evm2::interpreter::op;

    let mut cfg = OpcodeConfig::<OutbeEvmTypes>::base::<evm2::BaseEvmConfig<BASE_SPEC_ID>>();
    if cfg.is_defined(op::CREATE) {
        let gas = cfg.static_gas(op::CREATE);
        cfg.set_instruction::<GuardedCreate<false>>(op::CREATE, gas);
    }
    if cfg.is_defined(op::CREATE2) {
        let gas = cfg.static_gas(op::CREATE2);
        cfg.set_instruction::<GuardedCreate<true>>(op::CREATE2, gas);
    }
    if cfg.is_defined(op::SELFDESTRUCT) {
        let gas = cfg.static_gas(op::SELFDESTRUCT);
        cfg.set_instruction::<GuardedSelfdestruct>(op::SELFDESTRUCT, gas);
    }
    cfg.set_instruction::<GuardedNativeMarkerOpcode>(0xef, 0);
    cfg
}

impl<const BASE_SPEC_ID: u32> EvmConfig<OutbeEvmTypes> for OutbeOpcodeConfig<BASE_SPEC_ID> {
    const BASE_SPEC_ID: SpecId = SpecId::try_from_u32(BASE_SPEC_ID).expect("invalid spec id");
    const OPCODE_CONFIG: &'static OpcodeConfig<OutbeEvmTypes> =
        &build_outbe_opcode_config::<BASE_SPEC_ID>();
}

/// Runtime config selector mapping `SpecId` to Outbe's guarded dispatch tables.
#[allow(missing_copy_implementations, missing_debug_implementations)]
pub struct OutbeEvmConfigSelector(());

impl EvmConfigSelector<OutbeEvmTypes> for OutbeEvmConfigSelector {
    type Config<const BASE_SPEC_ID: u32, const CUSTOM_SPEC_ID: u32> =
        OutbeOpcodeConfig<BASE_SPEC_ID>;

    fn execution_config(spec_id: SpecId) -> ExecutionConfig<OutbeEvmTypes> {
        evm2::spec_to_generic!(spec_id, |BASE_SPEC_ID| {
            ExecutionConfig::for_config::<OutbeOpcodeConfig<BASE_SPEC_ID>>()
        })
    }
}

/// Resolves the execution gas limit for an Outbe system call targeting `OUTBE_SYSTEM_TX_ADDRESS`.
pub(crate) fn resolve_outbe_system_call_gas_limit(evm: &OutbeEvm<'_>, data: &[u8]) -> u64 {
    crate::system_tx::SystemTx::decode(data)
        .ok()
        .and_then(|sys_tx| {
            evm.ext()
                .runtime_host
                .as_deref()
                .and_then(|host| host.block_order().step_for_tx(&sys_tx))
        })
        .map_or(
            outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT,
            |step| step.gas_limit,
        )
}

/// Builds the `evm2::evm::SystemTx` for an Outbe or upstream system call.
pub(crate) fn build_outbe_system_tx(
    evm: &OutbeEvm<'_>,
    caller: Address,
    contract: Address,
    data: Bytes,
) -> EvmSystemTx {
    let mut sys_tx = EvmSystemTx::new(contract, data).with_caller(caller);
    if contract == outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS {
        let gas_limit = resolve_outbe_system_call_gas_limit(evm, &sys_tx.data);
        sys_tx = sys_tx.with_gas_limit(gas_limit);
    }
    sys_tx
}

#[cfg(any(test, feature = "test-hooks"))]
fn maybe_forced_outbe_system_call_result(
    contract: Address,
) -> Option<HandlerResult<TxResult<OutbeEvmTypes>>> {
    if contract != outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS {
        return None;
    }
    if FORCE_OUTBE_SYSTEM_CALL_ERROR.with(|cell| cell.get()) {
        return Some(Err(HandlerError::Fatal(evm2::AnyError::new(
            std::io::Error::other("forced Outbe system-call error for regression test"),
        ))));
    }
    if FORCE_OUTBE_SYSTEM_CALL_OOG_HALT.with(|cell| cell.get()) {
        return Some(Ok(TxResultExt {
            status: false,
            total_gas_spent: outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT,
            stop: InstrStop::OutOfGas,
            ..TxResultExt::default()
        }));
    }
    if FORCE_OUTBE_SYSTEM_CALL_REVERT.with(|cell| cell.get()) {
        return Some(Ok(TxResultExt {
            status: false,
            total_gas_spent: 42_000,
            stop: InstrStop::Revert,
            output: Bytes::from_static(b"forced Outbe system-call revert"),
            ..TxResultExt::default()
        }));
    }
    None
}

/// Executes an Outbe or standard Ethereum system call without finalizing transaction state
/// (used by detached trace replay inside a transaction handler).
pub(crate) fn execute_outbe_system_call(
    evm: &mut OutbeEvm<'_>,
    caller: Address,
    contract: Address,
    data: Bytes,
) -> HandlerResult<TxResult<OutbeEvmTypes>> {
    #[cfg(any(test, feature = "test-hooks"))]
    if let Some(forced) = maybe_forced_outbe_system_call_result(contract) {
        return forced;
    }
    let sys_tx = build_outbe_system_tx(evm, caller, contract, data);
    #[cfg(any(test, feature = "test-utils"))]
    {
        evm.ext_mut().last_system_call = Some(sys_tx.clone());
    }
    let call = evm.ext().abort_bridge.begin_call();
    let result = evm.execute_system_call(sys_tx);
    call.finish(result)
}

/// Executes an Outbe or standard Ethereum system call and returns its detached result and state.
///
/// Calls into `OUTBE_SYSTEM_TX_ADDRESS` use their protocol step's gas budget (defaulting to
/// `SYSTEM_TX_ARTIFACT_GAS_LIMIT`); all other system contracts keep upstream `SYSTEM_CALL_GAS_LIMIT`.
pub fn transact_outbe_system_call(
    evm: &mut OutbeEvm<'_>,
    caller: Address,
    contract: Address,
    data: Bytes,
) -> HandlerResult<evm2::TxResultWithState<OutbeEvmTypes>> {
    #[cfg(any(test, feature = "test-hooks"))]
    if let Some(forced) = maybe_forced_outbe_system_call_result(contract) {
        return forced.map(|result| evm2::TxResultWithState {
            result,
            pending_state: Default::default(),
            _non_exhaustive: (),
        });
    }
    let sys_tx = build_outbe_system_tx(evm, caller, contract, data);
    #[cfg(any(test, feature = "test-utils"))]
    {
        evm.ext_mut().last_system_call = Some(sys_tx.clone());
    }
    let call = evm.ext().abort_bridge.begin_call();
    let outcome = evm.system_call(sys_tx).map(|executed| executed.detach());
    call.finish(outcome)
}

/// Captures the validated intrinsic/floor gas values computed by `evm2::ethereum::*::prepare_with_hooks`
/// while preserving all default upstream pre-execution and settlement behavior.
struct CapturePreparedHooks;

impl TxHandlerHooks<OutbeEvmTypes> for CapturePreparedHooks {
    fn adjust_intrinsic_gas(
        host: &mut OutbeEvm<'_>,
        _envelope: &TxEnvelope,
        intrinsic: &mut u64,
        initial_state_gas: &mut u64,
        floor_gas: &mut u64,
    ) -> HandlerResult<()> {
        host.ext_mut().captured_prepared_gas = Some(CapturedPreparedGas {
            intrinsic: *intrinsic,
            initial_state_gas: *initial_state_gas,
            floor_gas: *floor_gas,
            sender_nonce: 0,
        });
        Ok(())
    }
}

/// Builds Outbe's transaction registry for `spec_id`.
///
/// Wraps all active Ethereum transaction types with:
/// - detached trace witness matching/replay,
/// - `with_evm_transact_witness_scope`,
/// - `ExecutionAbortBridge` fatal-abort preservation,
/// - top-level reserved-dynamic-precompile CREATE collision guarding, and
/// - post-authorization top-level EIP-7702 native delegation frame normalization (zero state mutation).
pub fn outbe_tx_registry(spec_id: SpecId) -> TxRegistry<OutbeEvmTypes, TxResult<OutbeEvmTypes>> {
    let mut registry = TxRegistry::new().with_handler(
        0,
        TxEnvelope::as_legacy,
        OutbeTxHandler {
            validate_prepare: legacy::prepare_with_hooks::<OutbeEvmTypes, DefaultTxHandlerHooks>,
            prepare: legacy::prepare_with_hooks::<OutbeEvmTypes, CapturePreparedHooks>,
            execute: execute_standard_prepared,
        },
    );

    if spec_id.enables(SpecId::BERLIN) {
        registry.register(
            1,
            TxEnvelope::as_eip2930,
            OutbeTxHandler {
                validate_prepare: eip2930::prepare_with_hooks::<
                    OutbeEvmTypes,
                    DefaultTxHandlerHooks,
                >,
                prepare: eip2930::prepare_with_hooks::<OutbeEvmTypes, CapturePreparedHooks>,
                execute: execute_standard_prepared,
            },
        );
    }
    if spec_id.enables(SpecId::LONDON) {
        registry.register(
            2,
            TxEnvelope::as_eip1559,
            OutbeTxHandler {
                validate_prepare: eip1559::prepare_with_hooks::<
                    OutbeEvmTypes,
                    DefaultTxHandlerHooks,
                >,
                prepare: eip1559::prepare_with_hooks::<OutbeEvmTypes, CapturePreparedHooks>,
                execute: execute_standard_prepared,
            },
        );
    }
    if spec_id.enables(SpecId::CANCUN) {
        registry.register(
            3,
            TxEnvelope::as_eip4844,
            OutbeTxHandler {
                validate_prepare: eip4844::prepare_with_hooks::<
                    OutbeEvmTypes,
                    DefaultTxHandlerHooks,
                >,
                prepare: eip4844::prepare_with_hooks::<OutbeEvmTypes, CapturePreparedHooks>,
                execute: execute_standard_prepared,
            },
        );
    }
    if spec_id.enables(SpecId::PRAGUE) {
        registry.register(
            4,
            TxEnvelope::as_eip7702,
            OutbeTxHandler {
                validate_prepare: eip7702::prepare_with_hooks::<
                    OutbeEvmTypes,
                    DefaultTxHandlerHooks,
                >,
                prepare: eip7702::prepare_with_hooks::<OutbeEvmTypes, CapturePreparedHooks>,
                execute: execute_eip7702_prepared,
            },
        );
    }

    registry
}

/// Pinned to `evm2` rev `66687c18b34d3bdbb899f5eb7605070615c03475` (`66687c1`):
/// relies on `prepare_with_hooks::<OutbeEvmTypes, CapturePreparedHooks>` capturing
/// intrinsic, floor, and initial state gas via `adjust_intrinsic_gas` before executing
/// the initial frame with Outbe's EIP-7702 native-delegation normalization.
fn execute_standard_prepared<Tx: alloy_consensus::Transaction>(
    req: TxRequest<'_, '_, OutbeEvmTypes, Tx>,
    gas: CapturedPreparedGas,
) -> HandlerResult<TxResult<OutbeEvmTypes>> {
    let caller = req.tx.signer();
    let tx: &Tx = req.tx.inner();
    let gas_price = tx.gas_price().map(U256::from).unwrap_or_else(|| {
        effective_gas_price(
            U256::from(tx.max_fee_per_gas()),
            U256::from(tx.max_priority_fee_per_gas().unwrap_or(0)),
            req.host.block().basefee,
        )
    });
    let blob_hashes = tx
        .blob_versioned_hashes()
        .unwrap_or_default()
        .iter()
        .copied()
        .map(b256_to_word)
        .collect();
    let (execution_gas_limit, reservoir) = initial_gas_and_reservoir(
        req.host.version(),
        tx.gas_limit(),
        gas.intrinsic,
        gas.initial_state_gas,
    );
    *req.host.tx_env_mut() = TxEnvExt {
        origin: caller,
        gas_price,
        chain_id: U256::from(req.host.version().chain_id),
        blob_hashes,
        ext: (),
        _non_exhaustive: (),
    };
    let mut tx_gas =
        GasTracker::new_with_execution_gas_and_reservoir(execution_gas_limit, reservoir);
    let mut frame = prepare_initial_frame(
        req.host,
        caller,
        gas.sender_nonce,
        tx.kind(),
        tx.input(),
        tx.value(),
        &mut tx_gas,
    )?;
    if !frame.runtime_oog {
        normalize_delegated_native_call(
            req.host.feature(EvmFeatures::EIP7702),
            req.host.ext().runtime_host.as_deref(),
            &mut frame.message,
        );
    }
    let result =
        execute_initial_frame(req.host, frame, &mut tx_gas, execution_gas_limit, reservoir)?;
    default_settle_gas(
        req.host,
        GasSettlement {
            caller,
            gas_price,
            gas_limit: tx.gas_limit(),
            floor_gas: gas.floor_gas,
            initial_state_gas: gas.initial_state_gas,
            state_refund: 0,
            result,
        },
    )
}

fn apply_eip7702_auth_phase(
    host: &mut OutbeEvm<'_>,
    caller: Address,
    tx: &LazyTxEip7702,
    tx_gas: &mut GasTracker,
) -> HandlerResult<(bool, u64, u64)> {
    let chain_id = host.version().chain_id;
    if host.feature(EvmFeatures::EIP2780) {
        let mut auth_charges =
            eip7702::RuntimeAuthCharges::new(host.version(), tx_gas, caller, tx.to, tx.value);
        let oog = eip7702::apply_auth_list(host, chain_id, tx.authorizations(), &mut auth_charges)?;
        Ok((oog, 0, 0))
    } else {
        let mut auth_refunds = eip7702::AuthRefunds::new(host.version());
        eip7702::apply_auth_list(host, chain_id, tx.authorizations(), &mut auth_refunds)?;
        tx_gas.set_reservoir(tx_gas.reservoir() + auth_refunds.state_refund);
        Ok((
            false,
            auth_refunds.state_refund,
            auth_refunds.execution_refund,
        ))
    }
}

/// Pinned to `evm2` rev `66687c18b34d3bdbb899f5eb7605070615c03475` (`66687c1`):
/// mirrors `evm2::ethereum::eip7702::execute_prepared` while applying Outbe's
/// `normalize_delegated_native_call` between `prepare_initial_frame` and `execute_initial_frame`.
fn execute_eip7702_prepared(
    req: TxRequest<'_, '_, OutbeEvmTypes, LazyTxEip7702>,
    gas: CapturedPreparedGas,
) -> HandlerResult<TxResult<OutbeEvmTypes>> {
    let caller = req.tx.signer();
    let tx = req.tx.inner();
    let gas_price = effective_gas_price(
        U256::from(tx.max_fee_per_gas),
        U256::from(tx.max_priority_fee_per_gas),
        req.host.block().basefee,
    );
    *req.host.tx_env_mut() = TxEnvExt {
        origin: caller,
        gas_price,
        chain_id: U256::from(req.host.version().chain_id),
        ..TxEnvExt::default()
    };
    let (execution_gas_limit, reservoir) = initial_gas_and_reservoir(
        req.host.version(),
        tx.gas_limit,
        gas.intrinsic,
        gas.initial_state_gas,
    );
    let mut tx_gas =
        GasTracker::new_with_execution_gas_and_reservoir(execution_gas_limit, reservoir);
    let runtime_checkpoint = req.host.state_mut().checkpoint();
    let (auth_oog, state_refund, execution_refund) =
        apply_eip7702_auth_phase(req.host, caller, tx, &mut tx_gas)?;

    let mut frame = if auth_oog {
        halted_initial_frame(
            req.host,
            caller,
            tx.nonce,
            tx.to.into(),
            &tx.input,
            tx.value,
            &GasTracker::new_with_execution_gas_and_reservoir(execution_gas_limit, reservoir),
        )
    } else {
        prepare_initial_frame(
            req.host,
            caller,
            gas.sender_nonce,
            tx.to.into(),
            &tx.input,
            tx.value,
            &mut tx_gas,
        )?
    };
    if frame.runtime_oog {
        let features = req.host.version().features;
        req.host.state_mut().rollback(runtime_checkpoint, features);
    } else {
        normalize_delegated_native_call(
            req.host.feature(EvmFeatures::EIP7702),
            req.host.ext().runtime_host.as_deref(),
            &mut frame.message,
        );
    }
    let mut result: MessageResult<OutbeEvmTypes> =
        execute_initial_frame(req.host, frame, &mut tx_gas, execution_gas_limit, reservoir)?;
    result.gas.set_refunded(
        result
            .gas
            .refunded()
            .saturating_add(i64::try_from(execution_refund).unwrap_or(i64::MAX)),
    );
    default_settle_gas(
        req.host,
        GasSettlement {
            caller,
            gas_price,
            gas_limit: tx.gas_limit,
            floor_gas: gas.floor_gas,
            initial_state_gas: gas.initial_state_gas,
            state_refund,
            result,
        },
    )
}

#[derive(Debug, thiserror::Error)]
#[error(
    "evm2 prepare_with_hooks completed without capturing prepared gas via adjust_intrinsic_gas"
)]
struct MissingCapturedPreparedGasError;

struct OutbeTxHandler<V, P, E> {
    validate_prepare: V,
    prepare: P,
    execute: E,
}

impl<V, P, E> OutbeTxHandler<V, P, E> {
    fn run_prepared_with_guards<Tx>(
        &self,
        mut req: TxRequest<'_, '_, OutbeEvmTypes, Tx>,
    ) -> HandlerResult<TxResult<OutbeEvmTypes>>
    where
        P: Fn(&mut TxRequest<'_, '_, OutbeEvmTypes, Tx>) -> HandlerResult<PreparedTx> + Send + Sync,
        E: Fn(
                TxRequest<'_, '_, OutbeEvmTypes, Tx>,
                CapturedPreparedGas,
            ) -> HandlerResult<TxResult<OutbeEvmTypes>>
            + Send
            + Sync,
    {
        let caller = req.tx.signer();
        let kind = req.envelope.kind();
        let sender_nonce = req
            .host
            .state_mut()
            .account_info_untracked(&caller)?
            .map_or(0, |info| info.nonce);
        let create_dest = TopLevelCreateGuard::detect(req.host, caller, kind)?;
        req.host.ext_mut().captured_prepared_gas = None;
        let _prepared = (self.prepare)(&mut req)?;
        let mut gas = req
            .host
            .ext_mut()
            .captured_prepared_gas
            .take()
            .ok_or_else(|| {
                evm2::registry::HandlerError::Fatal(evm2::AnyError::new(
                    MissingCapturedPreparedGasError,
                ))
            })?;
        gas.sender_nonce = sender_nonce;

        let create_guard = TopLevelCreateGuard::arm(req.host, create_dest)?;

        let result = (self.execute)(
            TxRequest {
                envelope: req.envelope,
                tx: req.tx,
                host: &mut *req.host,
                _non_exhaustive: (),
            },
            gas,
        );

        create_guard.finish(req.host, result.is_err())?;
        result
    }
}

impl<Tx, V, P, E> TxHandler<OutbeEvmTypes, Tx, TxResult<OutbeEvmTypes>> for OutbeTxHandler<V, P, E>
where
    V: Fn(&mut TxRequest<'_, '_, OutbeEvmTypes, Tx>) -> HandlerResult<PreparedTx> + Send + Sync,
    P: Fn(&mut TxRequest<'_, '_, OutbeEvmTypes, Tx>) -> HandlerResult<PreparedTx> + Send + Sync,
    E: Fn(
            TxRequest<'_, '_, OutbeEvmTypes, Tx>,
            CapturedPreparedGas,
        ) -> HandlerResult<TxResult<OutbeEvmTypes>>
        + Send
        + Sync,
{
    fn validate(&self, mut req: TxRequest<'_, '_, OutbeEvmTypes, Tx>) -> HandlerResult<()> {
        (self.validate_prepare)(&mut req).map(|_| ())
    }

    fn execute(
        &self,
        req: TxRequest<'_, '_, OutbeEvmTypes, Tx>,
    ) -> HandlerResult<TxResult<OutbeEvmTypes>> {
        let attached_state = req.host.ext().witness_state.clone();
        let input = crate::executor::witness::EvmTransactWitnessInput {
            attached_state: attached_state.as_ref(),
            owned_by_executor: req.host.ext().owned_by_executor,
            precompiles_mut_called: req.host.ext().precompiles_mut_called,
            chain_id: req.host.version().chain_id,
            block_number: req.host.block().number.saturating_to::<u64>(),
            block_timestamp: req.host.block().timestamp.saturating_to::<u64>(),
        };
        if let Some(matched) =
            crate::executor::witness::match_detached_trace_tx(&input, req.envelope)
        {
            req.host.ext_mut().detached_trace_guard.active = true;
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
                        execute_outbe_system_call(
                            req.host,
                            outbe_primitives::addresses::SYSTEM_ADDRESS,
                            outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                            data,
                        )
                    }),
                    crate::executor::witness::DetachedTraceTxAction::NormalTransact => {
                        let call = req.host.ext().abort_bridge.begin_call();
                        let result = call.finish(self.run_prepared_with_guards(req))?;
                        crate::executor::witness::record_detached_trace_user_gas(
                            result.tx_gas_used(),
                        );
                        Ok(result)
                    }
                }
            });
        }
        crate::executor::witness::with_evm_transact_witness_scope(input, || {
            let call = req.host.ext().abort_bridge.begin_call();
            call.finish(self.run_prepared_with_guards(req))
        })
    }
}

/// Custom EVM factory that registers Outbe stateful precompiles and safe opcode guards.
#[derive(Clone)]
pub struct OutbeEvmFactory {
    /// Immutable chain identity installed by `OutbeEvmConfig`.
    genesis_hash: B256,
    genesis_timestamp: u64,
    tee_attestation_v1: TeeAttestationChainSpecStateV1,
    /// Host installed for every EVM this factory creates.
    runtime_host: Arc<dyn crate::EvmRuntimeHost>,
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
    pub fn new(runtime_host: Arc<dyn crate::EvmRuntimeHost>) -> Self {
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
    pub fn with_runtime_host(mut self, host: Arc<dyn crate::EvmRuntimeHost>) -> Self {
        self.runtime_host = host;
        self
    }

    #[must_use]
    pub fn with_tee_attestation_v1(mut self, state: TeeAttestationChainSpecStateV1) -> Self {
        self.tee_attestation_v1 = state;
        self
    }

    /// Returns the runtime version for `spec` and `chain_id`.
    pub fn version(&self, spec: SpecId, chain_id: u64) -> Version {
        let mut version = Version::new(spec);
        version.chain_id = chain_id;
        version
    }

    /// Creates a concrete `OutbeEvm` with the given database and `EthEvmEnv<OutbeEvmTypes>`.
    pub fn create_evm<'a, DB>(&self, db: DB, env: EthEvmEnv<OutbeEvmTypes>) -> OutbeEvm<'a>
    where
        DB: DynDatabase + 'a,
    {
        let spec = env.spec;
        let abort_bridge = ExecutionAbortBridge::default();
        let precompiles = OutbePrecompileProvider::new(
            OutbePrecompileExecutionContext::new(spec, self.genesis_hash)
                .with_genesis_timestamp(self.genesis_timestamp)
                .with_tee_attestation_v1(self.tee_attestation_v1.clone()),
            OutbePrecompileRuntime::new().with_abort_bridge(abort_bridge.clone()),
            Arc::clone(&self.runtime_host),
        );
        let config = ExecutionConfig::for_spec_and_version(spec, env.version);
        let registry = outbe_tx_registry(spec);
        let ext = OutbeEvmExt {
            abort_bridge,
            runtime_host: Some(Arc::clone(&self.runtime_host)),
            ..OutbeEvmExt::default()
        };
        Evm::new_with_execution_config_and_ext(
            config,
            spec,
            env.block,
            registry,
            db,
            precompiles,
            ext,
        )
    }

    /// Creates a concrete `OutbeEvm` with an installed inspector.
    pub fn create_evm_with_inspector<'a, DB, I>(
        &self,
        db: DB,
        env: EthEvmEnv<OutbeEvmTypes>,
        inspector: I,
    ) -> OutbeEvm<'a>
    where
        DB: DynDatabase + 'a,
        I: evm2::Inspector<OutbeEvmTypes> + 'a,
    {
        let mut evm = self.create_evm(db, env);
        evm.set_inspector(inspector);
        evm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, U256};
    use evm2::{
        env::{BlockEnvExt, TxEnvExt},
        evm::{EmptyDB, SYSTEM_CALL_GAS_LIMIT},
        EvmFeatures,
    };
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

    struct NopModule;

    impl outbe_primitives::block::ProtocolModule for NopModule {
        const NAME: &'static str = "nop";
    }

    #[derive(Debug)]
    struct StepGasHost {
        stub: Arc<dyn crate::EvmRuntimeHost>,
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

    const USER_BLOCK_GAS_LIMIT: u64 = 30_000_000;

    pub(super) fn test_env() -> EthEvmEnv<OutbeEvmTypes> {
        let block = BlockEnvExt {
            gas_limit: U256::from(USER_BLOCK_GAS_LIMIT),
            ..Default::default()
        };
        EthEvmEnv::new(SpecId::SHANGHAI, block, 1)
    }

    #[test]
    fn outbe_system_call_resolves_step_gas_limits_and_fallbacks() {
        use crate::system_tx::SystemTx;

        let host = Arc::new(StepGasHost {
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
            let _ = transact_outbe_system_call(
                &mut evm,
                outbe_primitives::addresses::SYSTEM_ADDRESS,
                outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                data,
            );
            assert_eq!(
                evm.ext().last_system_call.as_ref().unwrap().gas_limit,
                expected
            );
            assert_eq!(evm.block().gas_limit, U256::from(USER_BLOCK_GAS_LIMIT));
        }
        evm.ext_mut().runtime_host = None;
        let _ = transact_outbe_system_call(
            &mut evm,
            outbe_primitives::addresses::SYSTEM_ADDRESS,
            outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            SystemTx::BeginBlock { step_index: 0 }.encode().unwrap(),
        );
        assert_eq!(
            evm.ext().last_system_call.as_ref().unwrap().gas_limit,
            fallback
        );
        assert_eq!(evm.block().gas_limit, U256::from(USER_BLOCK_GAS_LIMIT));
    }

    #[test]
    fn outbe_system_call_uses_artifact_gas_limit_without_changing_block_limit() {
        let factory = OutbeEvmFactory::new(crate::host::stub_host());
        let mut evm = factory.create_evm(EmptyDB::default(), test_env());

        let caller = outbe_primitives::addresses::SYSTEM_ADDRESS;
        let target = outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS;
        let data = Bytes::from_static(b"system call input");
        // A preceding user transaction must not leak fields into the system call.
        *evm.tx_env_mut() = TxEnvExt {
            origin: Address::repeat_byte(0x11),
            gas_price: U256::from(123),
            chain_id: U256::from(99),
            ..TxEnvExt::default()
        };
        let _ = transact_outbe_system_call(&mut evm, caller, target, data.clone());

        let last = evm.ext().last_system_call.as_ref().unwrap();
        assert_eq!(
            last.gas_limit,
            outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT
        );
        assert_eq!(evm.block().gas_limit, U256::from(USER_BLOCK_GAS_LIMIT));
        assert_eq!(last.caller, caller);
        assert_eq!(last.system_contract_address, target);
        assert_eq!(last.data, data);
        assert_eq!(evm.tx_env().origin, caller);
        assert_eq!(evm.tx_env().gas_price, U256::ZERO);
        assert!(evm.feature(EvmFeatures::NONCE_CHECK));
    }

    #[test]
    fn non_outbe_system_call_keeps_upstream_gas_limit() {
        let factory = OutbeEvmFactory::new(crate::host::stub_host());
        let mut evm = factory.create_evm(EmptyDB::default(), test_env());

        let _ = transact_outbe_system_call(
            &mut evm,
            outbe_primitives::addresses::SYSTEM_ADDRESS,
            Address::repeat_byte(0x42),
            Bytes::new(),
        );

        assert_eq!(
            evm.ext().last_system_call.as_ref().unwrap().gas_limit,
            SYSTEM_CALL_GAS_LIMIT
        );
        assert_eq!(evm.block().gas_limit, U256::from(USER_BLOCK_GAS_LIMIT));
    }
}
