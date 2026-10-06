//! Sub-call driver.
//!
//! Drives a child CALL/STATICCALL frame from inside an outbe Rust precompile.
//! The driver constructs a fresh borrow-mode
//! `Evm<&mut EthEvmContext<DB>, (), EthInstructions<...>, EthPrecompiles,
//! EthFrame<...>>`. Then it mirrors revm's canonical
//! [`Handler::run_exec_loop`](https://docs.rs/revm-handler/18.1.0/src/revm_handler/handler.rs.html)
//! pattern until the child terminates.
//!
//! The child frame uses [`crate::precompiles::OutbeSubCallPrecompiles`]. Thus both
//! the Ethereum precompiles `0x01..0x0a` AND the outbe stateful precompiles are
//! reachable from the child frame.
//!
//! The OUTER caller provides atomicity: it wraps
//! `storage.call(...)` / `storage.staticcall(...)` in `StorageHandle::with_checkpoint`.
//! The driver itself does NOT take an extra checkpoint. `make_call_frame`
//! handles per-frame journal checkpoints internally.

use crate::precompiles::{
    OcompActivationBlockMeter, OutbePrecompileExecutionContext, OutbePrecompileRuntime,
};
use alloy_evm::eth::EthEvmContext;
use alloy_primitives::{Address, B256, U256};
use core::fmt::Debug;
use outbe_compressed_entities::ExecutionScope;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_primitives::storage::{SubCallError, SubCallInput, SubCallOutput, SubCallStatus};
use revm::{
    context::{Evm, LocalContextTr},
    context_interface::{journaled_state::account::JournaledAccountTr, ContextTr, JournalTr},
    handler::{instructions::EthInstructions, EthFrame, EvmTr, FrameResult, ItemOrResult},
    interpreter::{
        interpreter::EthInterpreter,
        interpreter_action::{CallInputs, FrameInit, FrameInput},
        CallInput, CallOutcome, CallScheme, CallValue, InstructionResult, InterpreterResult,
        SharedMemory,
    },
    primitives::hardfork::SpecId,
    state::Bytecode,
    Database,
};
use std::sync::Arc;

/// Execution environment shared by a sub-call's child frame and precompiles.
pub struct SubCallEnvironment {
    pub self_address: Address,
    pub outer_is_static: bool,
    pub spec: SpecId,
    pub runtime_body_readers: Option<RuntimeBodyReaders>,
    pub execution_scope: Arc<ExecutionScope>,
}

/// Runs a sub-call with the executor-owned compressed-entity lifecycle scope.
///
/// `environment.outer_is_static = true` forces the child to STATICCALL regardless of the
/// caller's `input.is_static` field (outer STATIC propagates inward).
pub fn run<DB>(
    ctx: &mut EthEvmContext<DB>,
    environment: SubCallEnvironment,
    input: SubCallInput,
) -> std::result::Result<SubCallOutput, SubCallError>
where
    DB: Database + Debug,
    DB::Error: Debug,
{
    let SubCallEnvironment {
        self_address,
        outer_is_static,
        spec,
        runtime_body_readers,
        execution_scope,
    } = environment;
    run_with_ocomp_context(
        ctx,
        SubCallContext {
            self_address,
            outer_is_static,
            execution: OutbePrecompileExecutionContext::new(spec, B256::ZERO),
            runtime: OutbePrecompileRuntime::new(
                runtime_body_readers,
                execution_scope,
                None,
                false,
            ),
            activation_meter: Arc::new(OcompActivationBlockMeter),
        },
        input,
    )
}

pub(crate) struct SubCallContext {
    pub(crate) self_address: Address,
    pub(crate) outer_is_static: bool,
    pub(crate) execution: OutbePrecompileExecutionContext,
    pub(crate) runtime: OutbePrecompileRuntime,
    pub(crate) activation_meter: Arc<OcompActivationBlockMeter>,
}

pub(crate) fn run_with_ocomp_context<DB>(
    ctx: &mut EthEvmContext<DB>,
    context: SubCallContext,
    input: SubCallInput,
) -> std::result::Result<SubCallOutput, SubCallError>
where
    DB: Database + Debug,
    DB::Error: Debug,
{
    let effective_is_static = context.outer_is_static || input.is_static;

    // Static context + non-zero value -> reject early.
    if effective_is_static && !input.value.is_zero() {
        return Err(SubCallError::StateChangeDuringStaticCall);
    }

    // Depth check via journal.
    if ctx.journal().depth() >= 1024 {
        return Err(SubCallError::DepthLimitExceeded);
    }

    // Pre-load bytecode (mirror revm-handler-18.1.0/src/execution.rs:22-37).
    // Handles EIP-7702 delegation by re-loading from the delegate's address.
    let (bytecode_hash, bytecode) = load_target_bytecode(ctx, input.target)?;

    let call_inputs = build_call_inputs(
        &input,
        context.self_address,
        effective_is_static,
        (bytecode_hash, bytecode),
    );

    // Construct fresh borrow-mode Evm wrapping &mut ctx.
    // CTX = &mut EthEvmContext<DB> impls ContextTr via #[auto_impl(&mut, Box)]
    // on the trait.
    let mut instructions =
        EthInstructions::<EthInterpreter, &mut EthEvmContext<DB>>::new_mainnet_with_spec(
            context.execution.spec_id(),
        );
    crate::create_guard::install(&mut instructions);
    let precompiles = crate::precompiles::OutbeSubCallPrecompiles::<DB>::new(
        context.execution,
        context.runtime,
        context.activation_meter,
    );
    // A precompile resolves contract-originated calldata through `ctx.local()`, so the child
    // frame has to carve its memory out of that buffer.
    let mut caller_memory = CallerMemory(SharedMemory::new_with_buffer(
        ctx.local().shared_memory_buffer().clone(),
    ));
    #[allow(clippy::type_complexity)]
    let mut evm: Evm<
        &mut EthEvmContext<DB>,
        (),
        EthInstructions<EthInterpreter, &mut EthEvmContext<DB>>,
        crate::precompiles::OutbeSubCallPrecompiles<DB>,
        EthFrame<EthInterpreter>,
    > = Evm::new(ctx, instructions, precompiles);

    let frame_input = FrameInit {
        depth: 0,
        memory: caller_memory.0.new_child_context(),
        frame_input: FrameInput::Call(Box::new(call_inputs)),
    };

    // Canonical handler frame loop
    // (revm-handler-18.1.0/src/handler.rs:416-446).
    let frame_result = run_exec_loop(
        &mut crate::native_delegation::NativeDelegationEvm(&mut evm),
        frame_input,
    )?;

    // Translate FrameResult -> SubCallOutput.
    let call_outcome = match frame_result {
        FrameResult::Call(outcome) => outcome,
        FrameResult::Create(_) => {
            return Err(SubCallError::Fatal(
                "sub-call returned CREATE outcome (impossible for Call frame_input)".to_string(),
            ));
        }
    };
    Ok(call_outcome_to_subcall_output(
        call_outcome,
        input.gas_limit,
    ))
}

fn build_call_inputs(
    input: &SubCallInput,
    caller: Address,
    effective_is_static: bool,
    target_code: (B256, Bytecode),
) -> CallInputs {
    CallInputs {
        input: CallInput::Bytes(input.calldata.clone()),
        return_memory_offset: 0..0,
        gas_limit: input.gas_limit,
        reservoir: 0,
        bytecode_address: input.target,
        known_bytecode: target_code,
        target_address: input.target,
        caller,
        value: if effective_is_static {
            CallValue::Transfer(U256::ZERO)
        } else {
            CallValue::Transfer(input.value)
        },
        scheme: if effective_is_static {
            CallScheme::StaticCall
        } else {
            CallScheme::Call
        },
        is_static: effective_is_static,
        charged_new_account_state_gas: false,
    }
}

/// Gives the child context back even when the frame loop unwinds: the buffer outlives the sub-call.
struct CallerMemory(SharedMemory);

impl Drop for CallerMemory {
    fn drop(&mut self) {
        self.0.free_child_context();
    }
}

/// Pre-load target bytecode + hash. Mirrors revm-handler's
/// `create_init_frame` logic for EIP-7702 delegation.
fn load_target_bytecode<DB>(
    ctx: &mut EthEvmContext<DB>,
    target: Address,
) -> std::result::Result<(B256, Bytecode), SubCallError>
where
    DB: Database,
    DB::Error: Debug,
{
    // First pass: read info from target (info + delegate decision).
    let (delegate, hash, code) = {
        let journal = ctx.journal_mut();
        let account = journal
            .load_account_with_code_mut(target)
            .map_err(|e| SubCallError::DatabaseError(format!("{e:?}")))?;
        let info = &account.data.account().info;
        let delegate = info.code.as_ref().and_then(Bytecode::eip7702_address);
        (
            delegate,
            info.code_hash,
            info.code.clone().unwrap_or_default(),
        )
    };

    // EIP-7702 delegate handling: re-load from the delegate address.
    if let Some(delegate_addr) = delegate {
        let journal = ctx.journal_mut();
        let account = journal
            .load_account_with_code_mut(delegate_addr)
            .map_err(|e| SubCallError::DatabaseError(format!("{e:?}")))?;
        let info = &account.data.account().info;
        return Ok((info.code_hash, info.code.clone().unwrap_or_default()));
    }

    Ok((hash, code))
}

/// Mirrors revm-handler-18.1.0's [`run_exec_loop`].
///
/// Runs the frame stack inside `evm` to completion for `first_frame_input`,
/// returning the top-level [`FrameResult`].
fn run_exec_loop<E>(
    evm: &mut E,
    first_frame_input: FrameInit,
) -> std::result::Result<FrameResult, SubCallError>
where
    E: EvmTr<Frame = EthFrame<EthInterpreter>>,
    <E as EvmTr>::Context: ContextTr,
{
    let res = evm
        .frame_init(first_frame_input)
        .map_err(|e| SubCallError::Fatal(format!("frame_init: {e:?}")))?;
    if let ItemOrResult::Result(frame_result) = res {
        return Ok(frame_result);
    }

    loop {
        let call_or_result = evm
            .frame_run()
            .map_err(|e| SubCallError::Fatal(format!("frame_run: {e:?}")))?;
        let Some(result) = finish_or_initialize_frame(evm, call_or_result)? else {
            continue;
        };
        if let Some(r) = evm
            .frame_return_result(result)
            .map_err(|e| SubCallError::Fatal(format!("frame_return_result: {e:?}")))?
        {
            return Ok(r);
        }
    }
}

fn finish_or_initialize_frame<E>(
    evm: &mut E,
    call_or_result: ItemOrResult<FrameInit, FrameResult>,
) -> std::result::Result<Option<FrameResult>, SubCallError>
where
    E: EvmTr<Frame = EthFrame<EthInterpreter>>,
    <E as EvmTr>::Context: ContextTr,
{
    let init = match call_or_result {
        ItemOrResult::Result(result) => return Ok(Some(result)),
        ItemOrResult::Item(init) => init,
    };
    match evm
        .frame_init(init)
        .map_err(|e| SubCallError::Fatal(format!("frame_init nested: {e:?}")))?
    {
        ItemOrResult::Item(_) => Ok(None),
        ItemOrResult::Result(result) => Ok(Some(result)),
    }
}

/// Convert revm's [`CallOutcome`] (terminal frame state) into outbe's
/// [`SubCallOutput`].
fn call_outcome_to_subcall_output(outcome: CallOutcome, original_gas_limit: u64) -> SubCallOutput {
    let CallOutcome { result, .. } = outcome;
    let InterpreterResult {
        result: instr,
        output,
        gas,
    } = result;

    let gas_used = original_gas_limit.saturating_sub(gas.remaining());
    let gas_refunded = gas.refunded();

    let status = match instr {
        InstructionResult::Return | InstructionResult::Stop => SubCallStatus::Success,
        InstructionResult::Revert => SubCallStatus::Revert(output.clone()),
        InstructionResult::CallTooDeep => SubCallStatus::Halt(SubCallError::DepthLimitExceeded),
        InstructionResult::OutOfGas
        | InstructionResult::MemoryOOG
        | InstructionResult::MemoryLimitOOG
        | InstructionResult::PrecompileOOG
        | InstructionResult::InvalidOperandOOG
        | InstructionResult::ReentrancySentryOOG => SubCallStatus::Halt(SubCallError::OutOfGas),
        InstructionResult::CallNotAllowedInsideStatic
        | InstructionResult::StateChangeDuringStaticCall => {
            SubCallStatus::Halt(SubCallError::StateChangeDuringStaticCall)
        }
        // Anything else: surface as a typed Fatal so the precompile can
        // decide whether to revert.
        other => SubCallStatus::Halt(SubCallError::Fatal(format!(
            "child frame halted: {other:?}"
        ))),
    };

    SubCallOutput {
        status,
        returndata: output,
        gas_used,
        gas_refunded,
    }
}
