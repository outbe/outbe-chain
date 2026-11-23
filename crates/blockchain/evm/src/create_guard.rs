//! Genesis-active reservation of the dynamic precompile address class.
//!
//! The guard deliberately reuses evm2's normal `CreateCollision` path:
//! - For top-level `CREATE` transactions targeting an uninitialized reserved
//!   dynamic precompile address, [`TopLevelCreateGuard`] temporarily presents
//!   a non-zero nonce on the destination during `execute_prepared` and restores
//!   it immediately afterward.
//! - For nested `CREATE` / `CREATE2` opcodes targeting an uninitialized reserved
//!   dynamic precompile address, [`GuardedCreate`] does the same around
//!   `instr::create::execute` so evm2 runs its exact canonical `CreateCollision`
//!   handling (warming destination, bumping caller nonce, burning child gas,
//!   pushing `0`).

use crate::factory::{OutbeEvm, OutbeEvmTypes};
use alloy_primitives::{Address, Keccak256, TxKind, B256};
use evm2::{
    evm::{DbResult, StateCheckpoint},
    interpreter::{
        instructions as instr,
        private::{Instruction, InstructionFlags},
        InstrStop, InterpreterState, Pc, Result as InstrResult, StackMut,
    },
    utils::WordExt as _,
    EvmFeatures, ExecutionError,
};
use outbe_primitives::addresses::is_reserved_dynamic_precompile_address;

/// Upper bound on initcode size we hash when peeking nested `CREATE2` destinations.
const MAX_PEEK_CREATE2_INITCODE: usize = 1024 * 1024;
/// Upper bound on memory offset we peek for nested `CREATE2` before delegating to `instr::create`.
const MAX_PEEK_CREATE2_OFFSET: usize = 16 * 1024 * 1024;

/// Active temporary nonce guard for a top-level `CREATE` transaction.
pub(crate) struct TopLevelCreateGuard {
    active: Option<(Address, StateCheckpoint)>,
}

impl TopLevelCreateGuard {
    /// Identifies an uninitialized reserved destination BEFORE `prepare` bumps sender nonce.
    pub(crate) fn detect(
        evm: &mut OutbeEvm<'_>,
        caller: Address,
        kind: TxKind,
    ) -> DbResult<Option<Address>> {
        if !kind.is_create() {
            return Ok(None);
        }
        let sender_nonce = evm
            .state_mut()
            .account_info_untracked(&caller)?
            .map_or(0, |info| info.nonce);
        let destination = caller.create(sender_nonce);
        if !is_reserved_dynamic_precompile_address(destination) {
            return Ok(None);
        }
        let existing = evm.state_mut().account_info_untracked(&destination)?;
        let needs_guard = existing.is_none_or(|info| info.nonce == 0 && info.is_empty_code_hash());
        Ok(needs_guard.then_some(destination))
    }

    /// Arms the temporary collision nonce after `prepare` succeeds.
    pub(crate) fn arm(evm: &mut OutbeEvm<'_>, dest: Option<Address>) -> DbResult<Self> {
        let active = match dest {
            Some(dest) => {
                let cp = evm.state_mut().checkpoint();
                evm.state_mut().account(&dest)?.set_nonce(1);
                Some((dest, cp))
            }
            None => None,
        };
        Ok(Self { active })
    }

    /// Restores the destination after `execute_prepared` completes.
    pub(crate) fn finish(self, evm: &mut OutbeEvm<'_>, failed: bool) -> DbResult<()> {
        let Some((dest, cp)) = self.active else {
            return Ok(());
        };
        if failed {
            let features = evm.version().features;
            evm.state_mut().rollback(cp, features);
        } else {
            evm.state_mut().account(&dest)?.set_nonce(0);
        }
        Ok(())
    }
}

/// Safe `CREATE` / `CREATE2` instruction wrapper enforcing the reserved dynamic precompile guard.
pub(crate) struct GuardedCreate<const IS_CREATE2: bool>;

impl<const IS_CREATE2: bool> Instruction<OutbeEvmTypes> for GuardedCreate<IS_CREATE2> {
    const FLAGS: InstructionFlags =
        <instr::create<OutbeEvmTypes, IS_CREATE2> as Instruction<OutbeEvmTypes>>::FLAGS;

    fn execute(
        pc: &mut Pc,
        stack: StackMut<'_>,
        state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
    ) -> InstrResult {
        let temp_collision_dest = if !state.is_static() {
            peek_uninitialized_reserved_dest::<IS_CREATE2>(&stack, state)?
        } else {
            None
        };

        let Some(dest) = temp_collision_dest else {
            return instr::create::<OutbeEvmTypes, IS_CREATE2>::execute(pc, stack, state);
        };

        let pre_checkpoint = state.host().state_mut().checkpoint();
        let set_result = state
            .host()
            .state_mut()
            .account(&dest)
            .map(|mut account| account.set_nonce(1));
        if let Err(err) = set_result {
            return Err(state.fail(ExecutionError::Database(err)));
        }

        let result = instr::create::<OutbeEvmTypes, IS_CREATE2>::execute(pc, stack, state);

        if result.is_err() {
            let features = state.host().version().features;
            state.host().state_mut().rollback(pre_checkpoint, features);
        } else {
            let restore_result = state
                .host()
                .state_mut()
                .account(&dest)
                .map(|mut account| account.set_nonce(0));
            if let Err(err) = restore_result {
                return Err(state.fail(ExecutionError::Database(err)));
            }
        }
        result
    }
}

fn peek_uninitialized_reserved_dest<const IS_CREATE2: bool>(
    stack: &StackMut<'_>,
    state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
) -> std::result::Result<Option<Address>, InstrStop> {
    let dest = if IS_CREATE2 {
        derive_create2_destination(stack, state)
    } else {
        derive_create_destination(stack, state)?
    };
    let Some(dest) = dest.filter(|addr| is_reserved_dynamic_precompile_address(*addr)) else {
        return Ok(None);
    };
    let existing = state
        .host()
        .state_mut()
        .account_info_untracked(&dest)
        .map_err(|err| state.fail(ExecutionError::Database(err)))?;
    let needs_guard = existing.is_none_or(|info| info.nonce == 0 && info.is_empty_code_hash());
    Ok(needs_guard.then_some(dest))
}

fn derive_create_destination(
    stack: &StackMut<'_>,
    state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
) -> std::result::Result<Option<Address>, InstrStop> {
    if stack.len() < 3 {
        return Ok(None);
    }
    let caller = state.message().destination;
    let caller_nonce = state
        .host()
        .state_mut()
        .account_info_untracked(&caller)
        .map_err(|err| state.fail(ExecutionError::Database(err)))?
        .map_or(0, |info| info.nonce);
    Ok(Some(caller.create(caller_nonce)))
}

fn derive_create2_destination(
    stack: &StackMut<'_>,
    state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
) -> Option<Address> {
    let [_value, offset, len, salt] = stack.peekn::<4>()?;
    let len = len.to_usize().ok()?;
    let max_len = if state.feature(EvmFeatures::EIP3860) {
        state
            .version()
            .max_initcode_size
            .min(MAX_PEEK_CREATE2_INITCODE)
    } else {
        MAX_PEEK_CREATE2_INITCODE
    };
    if len > max_len {
        return None;
    }
    let init_code_hash = if len == 0 {
        alloy_primitives::KECCAK256_EMPTY
    } else {
        let offset = offset
            .to_usize()
            .ok()
            .filter(|o| *o <= MAX_PEEK_CREATE2_OFFSET)?;
        hash_memory_padded(state.memory().as_slice(), offset, len)
    };
    Some(
        state
            .message()
            .destination
            .create2(B256::from(salt), init_code_hash),
    )
}

fn hash_memory_padded(memory: &[u8], offset: usize, len: usize) -> B256 {
    let mut hasher = Keccak256::new();
    let end = offset.saturating_add(len);
    let in_bounds_end = end.min(memory.len());
    if offset < in_bounds_end {
        hasher.update(&memory[offset..in_bounds_end]);
    }
    let covered = in_bounds_end.saturating_sub(offset);
    let mut remaining = len.saturating_sub(covered);
    const ZEROS: [u8; 256] = [0u8; 256];
    while remaining > 0 {
        let chunk = remaining.min(ZEROS.len());
        hasher.update(&ZEROS[..chunk]);
        remaining -= chunk;
    }
    hasher.finalize()
}
