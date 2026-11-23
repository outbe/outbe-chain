//! EIP-7702 delegations to native routes execute empty code, not the persisted
//! account marker. All frame accounting and completion remain owned by evm2.

use crate::factory::{OutbeEvm, OutbeEvmTypes};
use alloy_primitives::{Address, TxKind, B256, KECCAK256_EMPTY};
use evm2::{
    bytecode::Bytecode,
    ethereum::TxEnvelope,
    evm::{DbResult, StateCheckpoint},
    interpreter::{
        instructions as instr,
        private::{Instruction, InstructionFlags},
        InstrStop, InterpreterState, Message, MessageKind, Pc, Result as InstrResult, StackMut,
    },
    utils::WordExt as _,
    EvmFeatures, ExecutionError,
};

/// Normalizes a prepared sub-call message whose `call_target` is not a registered
/// precompile, but whose resolved EIP-7702 delegation target (`code_address`) IS
/// a registered precompile, so that it executes empty bytecode instead of the `0xef` marker.
pub(crate) fn normalize_delegated_native_call(
    eip7702: bool,
    host: Option<&dyn crate::EvmRuntimeHost>,
    message: &mut Message<OutbeEvmTypes>,
) {
    if !eip7702 {
        return;
    }
    if !matches!(
        message.kind,
        MessageKind::Call
            | MessageKind::CallCode
            | MessageKind::DelegateCall
            | MessageKind::StaticCall
    ) {
        return;
    }
    let Some(host) = host else {
        return;
    };
    if !host.is_registered_precompile(&message.call_target)
        && message.code_address != message.call_target
        && host.is_registered_precompile(&message.code_address)
    {
        message.code = Bytecode::default();
    }
}

/// Active temporary marker-code mask for top-level EIP-7702 calls targeting a native precompile.
pub(crate) struct TopLevelNativeDelegateGuard {
    masked: Vec<MaskedDelegateCode>,
    checkpoint: Option<StateCheckpoint>,
}

impl TopLevelNativeDelegateGuard {
    /// Arms temporary empty-code masks for any native precompile delegate reachable from `to`.
    pub(crate) fn arm(
        evm: &mut OutbeEvm<'_>,
        envelope: &TxEnvelope,
        kind: TxKind,
    ) -> DbResult<Self> {
        let masked = collect_top_level_delegated_native_markers(evm, envelope, kind)?;
        let checkpoint = if masked.is_empty() {
            None
        } else {
            let cp = evm.state_mut().checkpoint();
            for item in &masked {
                evm.state_mut()
                    .account(&item.delegate)?
                    .set_code(KECCAK256_EMPTY, Bytecode::default());
            }
            Some(cp)
        };
        Ok(Self { masked, checkpoint })
    }

    /// Restores masked delegate marker bytecode after `execute_prepared` completes.
    pub(crate) fn finish(self, evm: &mut OutbeEvm<'_>, failed: bool) -> DbResult<()> {
        let Some(cp) = self.checkpoint else {
            return Ok(());
        };
        if failed {
            let features = evm.version().features;
            evm.state_mut().rollback(cp, features);
        } else {
            for item in self.masked {
                evm.state_mut()
                    .account(&item.delegate)?
                    .set_code(item.code_hash, item.code);
            }
        }
        Ok(())
    }
}

fn collect_top_level_delegated_native_markers(
    evm: &mut OutbeEvm<'_>,
    envelope: &TxEnvelope,
    kind: TxKind,
) -> DbResult<Vec<MaskedDelegateCode>> {
    let TxKind::Call(to) = kind else {
        return Ok(Vec::new());
    };
    if !evm.feature(EvmFeatures::EIP7702) {
        return Ok(Vec::new());
    }
    let Some(host) = evm.ext().runtime_host.clone() else {
        return Ok(Vec::new());
    };
    if host.is_registered_precompile(&to) {
        return Ok(Vec::new());
    }
    let candidates = collect_candidate_delegates(evm, envelope, to)?;
    let mut masked = Vec::new();
    for delegate in candidates {
        if host.is_registered_precompile(&delegate)
            && !masked
                .iter()
                .any(|item: &MaskedDelegateCode| item.delegate == delegate)
        {
            if let Some(entry) = load_marker_code(evm, delegate)? {
                masked.push(entry);
            }
        }
    }
    Ok(masked)
}

fn collect_candidate_delegates(
    evm: &mut OutbeEvm<'_>,
    envelope: &TxEnvelope,
    to: Address,
) -> DbResult<Vec<Address>> {
    let mut candidates = Vec::new();
    if let Some(delegate) = evm.state_mut().code_untracked(&to)?.eip7702_address() {
        candidates.push(delegate);
    }
    if let Some(eip7702_tx) = envelope.as_eip7702() {
        candidates.extend(
            eip7702_tx
                .authorizations()
                .filter(|auth| auth.authority() == Some(to))
                .map(|auth| *auth.address()),
        );
    }
    Ok(candidates)
}

fn load_marker_code(
    evm: &mut OutbeEvm<'_>,
    delegate: Address,
) -> DbResult<Option<MaskedDelegateCode>> {
    let Some(info) = evm.state_mut().account_info_untracked(&delegate)? else {
        return Ok(None);
    };
    if info.is_empty_code_hash() {
        return Ok(None);
    }
    let code = evm.state_mut().code_untracked(&delegate)?;
    Ok(Some(MaskedDelegateCode {
        delegate,
        code_hash: info.code_hash,
        code,
    }))
}

/// Safe `CALL` instruction wrapper enforcing native EIP-7702 delegation normalization.
pub(crate) struct GuardedCall;
/// Safe `CALLCODE` instruction wrapper enforcing native EIP-7702 delegation normalization.
pub(crate) struct GuardedCallCode;
/// Safe `DELEGATECALL` instruction wrapper enforcing native EIP-7702 delegation normalization.
pub(crate) struct GuardedDelegateCall;
/// Safe `STATICCALL` instruction wrapper enforcing native EIP-7702 delegation normalization.
pub(crate) struct GuardedStaticCall;

impl Instruction<OutbeEvmTypes> for GuardedCall {
    const FLAGS: InstructionFlags =
        <instr::call<OutbeEvmTypes> as Instruction<OutbeEvmTypes>>::FLAGS;

    fn execute(
        pc: &mut Pc,
        stack: StackMut<'_>,
        state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
    ) -> InstrResult {
        execute_guarded_call(pc, stack, state, instr::call::<OutbeEvmTypes>::execute)
    }
}

impl Instruction<OutbeEvmTypes> for GuardedCallCode {
    const FLAGS: InstructionFlags =
        <instr::callcode<OutbeEvmTypes> as Instruction<OutbeEvmTypes>>::FLAGS;

    fn execute(
        pc: &mut Pc,
        stack: StackMut<'_>,
        state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
    ) -> InstrResult {
        execute_guarded_call(pc, stack, state, instr::callcode::<OutbeEvmTypes>::execute)
    }
}

impl Instruction<OutbeEvmTypes> for GuardedDelegateCall {
    const FLAGS: InstructionFlags =
        <instr::delegatecall<OutbeEvmTypes> as Instruction<OutbeEvmTypes>>::FLAGS;

    fn execute(
        pc: &mut Pc,
        stack: StackMut<'_>,
        state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
    ) -> InstrResult {
        execute_guarded_call(
            pc,
            stack,
            state,
            instr::delegatecall::<OutbeEvmTypes>::execute,
        )
    }
}

impl Instruction<OutbeEvmTypes> for GuardedStaticCall {
    const FLAGS: InstructionFlags =
        <instr::staticcall<OutbeEvmTypes> as Instruction<OutbeEvmTypes>>::FLAGS;

    fn execute(
        pc: &mut Pc,
        stack: StackMut<'_>,
        state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
    ) -> InstrResult {
        execute_guarded_call(
            pc,
            stack,
            state,
            instr::staticcall::<OutbeEvmTypes>::execute,
        )
    }
}

struct MaskedDelegateCode {
    delegate: Address,
    code_hash: B256,
    code: Bytecode,
}

fn execute_guarded_call(
    pc: &mut Pc,
    stack: StackMut<'_>,
    state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
    inner: fn(&mut Pc, StackMut<'_>, &mut InterpreterState<'_, '_, OutbeEvmTypes>) -> InstrResult,
) -> InstrResult {
    let masked = if state.feature(EvmFeatures::EIP7702) {
        find_delegated_native_marker(&stack, state)?
    } else {
        None
    };

    let Some(masked) = masked else {
        return inner(pc, stack, state);
    };

    let pre_checkpoint = state.host().state_mut().checkpoint();
    let mask_result = state
        .host()
        .state_mut()
        .account(&masked.delegate)
        .map(|mut account| account.set_code(KECCAK256_EMPTY, Bytecode::default()));
    if let Err(err) = mask_result {
        return Err(state.fail(ExecutionError::Database(err)));
    }

    let result = inner(pc, stack, state);

    if result.is_err() {
        let features = state.host().version().features;
        state.host().state_mut().rollback(pre_checkpoint, features);
    } else {
        let restore_result = state
            .host()
            .state_mut()
            .account(&masked.delegate)
            .map(|mut account| account.set_code(masked.code_hash, masked.code));
        if let Err(err) = restore_result {
            return Err(state.fail(ExecutionError::Database(err)));
        }
    }
    result
}

fn find_delegated_native_marker(
    stack: &StackMut<'_>,
    state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
) -> std::result::Result<Option<MaskedDelegateCode>, InstrStop> {
    let Some(delegate) = resolve_delegated_precompile(stack, state)? else {
        return Ok(None);
    };
    load_marker_code(state.host(), delegate)
        .map_err(|err| state.fail(ExecutionError::Database(err)))
}

fn resolve_delegated_precompile(
    stack: &StackMut<'_>,
    state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
) -> std::result::Result<Option<Address>, InstrStop> {
    let Some(to) = stack.peek(1).map(|word| word.to_address()) else {
        return Ok(None);
    };
    let Some(host) = state.host().ext().runtime_host.clone() else {
        return Ok(None);
    };
    if host.is_registered_precompile(&to) {
        return Ok(None);
    }
    let to_code = state
        .host()
        .state_mut()
        .code_untracked(&to)
        .map_err(|err| state.fail(ExecutionError::Database(err)))?;
    Ok(to_code
        .eip7702_address()
        .filter(|delegate| host.is_registered_precompile(delegate)))
}
