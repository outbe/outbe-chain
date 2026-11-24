//! Sub-call driver.
//!
//! Drives a child CALL/STATICCALL frame from inside an outbe Rust precompile
//! directly through the live [`evm2::Evm`]'s [`Host::execute_message`] entrypoint.
//!
//! The live EVM already carries [`crate::precompiles::OutbePrecompileProvider`]
//! and Outbe's opcode guards, so both Ethereum precompiles `0x01..0x0a` and
//! Outbe stateful precompiles are reachable from child frames with zero `unsafe`.
//!
//! The OUTER caller provides atomicity: it wraps `storage.call(...)` /
//! `storage.staticcall(...)` in `StorageHandle::with_checkpoint`. The driver
//! itself does NOT take an extra checkpoint: `execute_message` handles
//! per-frame journal checkpoints internally.

use crate::{factory::OutbeEvmTypes, precompiles::OutbePrecompileRuntime};
use alloy_primitives::{Address, B256, U256};
use evm2::{
    bytecode::Bytecode,
    constants::CALL_DEPTH_LIMIT,
    interpreter::{Gas, Host as _, InstrStop, Message, MessageExt, MessageKind, MessageResult},
    Evm, EvmFeatures, ExecutionError, SpecId,
};
use outbe_primitives::storage::{SubCallError, SubCallInput, SubCallOutput, SubCallStatus};

/// Execution environment shared by a sub-call's child frame and precompiles.
pub struct SubCallEnvironment {
    pub self_address: Address,
    pub outer_is_static: bool,
    pub spec: SpecId,
}

/// Runs a sub-call through the executor-owned EVM instance.
///
/// `environment.outer_is_static = true` forces the child to STATICCALL regardless of the
/// caller's `input.is_static` field (outer STATIC propagates inward).
/// `host` authorizes every precompile the child frame reaches.
pub fn run_with_host(
    evm: &mut Evm<'_, OutbeEvmTypes>,
    environment: SubCallEnvironment,
    input: SubCallInput,
    host: std::sync::Arc<dyn crate::EvmRuntimeHost>,
) -> std::result::Result<SubCallOutput, SubCallError> {
    let SubCallEnvironment {
        self_address,
        outer_is_static,
        spec: _,
    } = environment;
    run_with_context(
        evm,
        SubCallContext {
            self_address,
            outer_is_static,
            runtime: OutbePrecompileRuntime::new(),
            host,
        },
        input,
    )
}

pub(crate) struct SubCallContext {
    pub(crate) self_address: Address,
    pub(crate) outer_is_static: bool,
    pub(crate) runtime: OutbePrecompileRuntime,
    pub(crate) host: std::sync::Arc<dyn crate::EvmRuntimeHost>,
}

pub(crate) fn run_with_context(
    evm: &mut Evm<'_, OutbeEvmTypes>,
    context: SubCallContext,
    input: SubCallInput,
) -> std::result::Result<SubCallOutput, SubCallError> {
    let limit = input.gas_limit;
    let outcome = match run_frame(evm, context, input, None) {
        Ok(outcome) => outcome,
        Err(
            error @ (SubCallError::DepthLimitExceeded | SubCallError::StateChangeDuringStaticCall),
        ) => {
            return Ok(SubCallOutput {
                status: SubCallStatus::Halt(error),
                returndata: Default::default(),
                gas_used: 0,
                gas_refunded: 0,
            });
        }
        Err(error) => return Err(error),
    };
    // Direct-driver users supply their own bounded allowance. Production uses
    // the provider's real parent meter instead of this standalone tracker.
    let mut parent = Gas::new(limit);
    if parent.tracker_mut().try_spend(limit).is_err() {
        return Err(SubCallError::Fatal(
            "cannot reserve standalone child gas".into(),
        ));
    }
    settle_outcome(outcome, &mut parent)
}

/// Execute one child; its caller owns reservation and canonical settlement.
/// A supplied target was already loaded and priced by the provider.
pub(crate) fn run_frame(
    evm: &mut Evm<'_, OutbeEvmTypes>,
    context: SubCallContext,
    input: SubCallInput,
    target_code: Option<(Address, Bytecode)>,
) -> std::result::Result<MessageResult<OutbeEvmTypes>, SubCallError> {
    context.runtime.abort_bridge.check_subcall()?;
    let effective_is_static = context.outer_is_static || input.is_static;

    // Static context + non-zero value -> reject early.
    if effective_is_static && !input.value.is_zero() {
        return Err(SubCallError::StateChangeDuringStaticCall);
    }

    // Track call depth across native <-> Rust precompile boundaries.
    let depth = evm.ext().current_call_depth.saturating_add(1);
    if depth > CALL_DEPTH_LIMIT {
        return Err(SubCallError::DepthLimitExceeded);
    }

    let (code_address, bytecode) = match target_code {
        Some(loaded) => loaded,
        None => load_target_bytecode(evm, input.target)?,
    };

    let mut message = build_call_message(
        &input,
        &context,
        depth,
        effective_is_static,
        (code_address, bytecode),
    );
    crate::native_delegation::normalize_delegated_native_call(
        evm.feature(EvmFeatures::EIP7702),
        Some(context.host.as_ref()),
        &mut message,
    );

    let prev_depth = evm.ext().current_call_depth;
    evm.ext_mut().current_call_depth = depth;
    // Deliberately no Drop guard: if a panic unwinds through `evm.execute_message`,
    // `evm2` leaves `current_frame` pointing at the dropped child interpreter.
    // Leaving `native_frame_depth` incremented forces `outbe_ctx_dispatch` to
    // escalate the unwind to `PrecompileError::Fatal` instead of continuing the
    // parent frame on a dangling pointer.
    evm.ext_mut().native_frame_depth += 1;
    #[cfg(test)]
    {
        let current = evm.ext().native_frame_depth;
        if current > evm.ext().max_native_frame_depth {
            evm.ext_mut().max_native_frame_depth = current;
        }
    }
    let result = evm.execute_message(&mut message);
    evm.ext_mut().native_frame_depth -= 1;
    evm.ext_mut().current_call_depth = prev_depth;

    match result {
        Ok(outcome) => Ok(outcome),
        Err(ExecutionError::Database(err)) => Err(SubCallError::DatabaseError(err.to_string())),
        Err(ExecutionError::Fatal(err)) => Err(SubCallError::Fatal(err.to_string())),
    }
}

fn build_call_message(
    input: &SubCallInput,
    context: &SubCallContext,
    depth: u16,
    effective_is_static: bool,
    (code_address, code): (Address, Bytecode),
) -> Message<OutbeEvmTypes> {
    MessageExt {
        kind: if effective_is_static {
            MessageKind::StaticCall
        } else {
            MessageKind::Call
        },
        depth,
        gas_limit: input.gas_limit,
        reservoir: 0,
        destination: input.target,
        call_target: input.target,
        caller: context.self_address,
        input: input.calldata.clone().into(),
        value: if effective_is_static {
            U256::ZERO
        } else {
            input.value
        },
        code,
        code_address,
        caller_is_static: effective_is_static,
        salt: B256::ZERO,
        ext: (),
        _non_exhaustive: (),
    }
}

/// Pre-load target bytecode + resolved code address (following EIP-7702 delegation once).
fn load_target_bytecode(
    evm: &mut Evm<'_, OutbeEvmTypes>,
    target: Address,
) -> std::result::Result<(Address, Bytecode), SubCallError> {
    let eip7702 = evm.feature(EvmFeatures::EIP7702);
    let code = evm
        .state_mut()
        .account(&target)
        .and_then(|mut account| account.load_code())
        .map_err(|e| SubCallError::DatabaseError(format!("{e:?}")))?;

    if eip7702 {
        if let Some(delegate_addr) = code.eip7702_address() {
            let delegated_code = evm
                .state_mut()
                .account(&delegate_addr)
                .and_then(|mut account| account.load_code())
                .map_err(|e| SubCallError::DatabaseError(format!("{e:?}")))?;
            return Ok((delegate_addr, delegated_code));
        }
    }

    Ok((target, code))
}

/// Settle against the parent before narrowing the child's full gas state to
/// the public API. A halt spends its allowance, while a revert-like entry
/// failure returns unused gas. This is the same helper as native CALL.
pub(crate) fn settle_outcome(
    outcome: MessageResult<OutbeEvmTypes>,
    parent: &mut Gas,
) -> std::result::Result<SubCallOutput, SubCallError> {
    let stop = outcome.stop;
    let status = match stop {
        InstrStop::Stop | InstrStop::Return | InstrStop::SelfDestruct => SubCallStatus::Success,
        InstrStop::CallTooDeep => SubCallStatus::Halt(SubCallError::DepthLimitExceeded),
        InstrStop::OutOfFunds => SubCallStatus::Halt(SubCallError::EvmHalt(InstrStop::OutOfFunds)),
        stop if stop.is_revert() => SubCallStatus::Revert(outcome.output.clone()),
        stop if stop.is_out_of_gas() => SubCallStatus::Halt(SubCallError::OutOfGas),
        InstrStop::CallNotAllowedInsideStatic | InstrStop::StateChangeDuringStaticCall => {
            SubCallStatus::Halt(SubCallError::StateChangeDuringStaticCall)
        }
        stop if stop.is_fatal() => {
            return Err(crate::errors::SubCallDriverError::non_vm_child_terminal(
                stop,
            ));
        }
        other => SubCallStatus::Halt(SubCallError::EvmHalt(other)),
    };
    parent.merge_child_gas(outcome.gas, stop);
    Ok(SubCallOutput {
        status,
        returndata: outcome.output,
        gas_used: outcome.gas.spent(),
        gas_refunded: outcome.gas.refunded(),
    })
}
