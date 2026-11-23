//! SELFDESTRUCT that would strand COEN on a system address, or burn it in a contract that the same
//! transaction made, sends the balance to address 0 instead. The Promis Limit burns it from there.

use crate::factory::OutbeEvmTypes;
use alloy_primitives::{keccak256, Address};
use evm2::{
    interpreter::{
        instructions as instr,
        private::{Instruction, InstructionFlags},
        InstrStop, InterpreterState, Pc, Result as InstrResult, StackMut, Word,
    },
    utils::WordExt as _,
    ExecutionError,
};
use outbe_primitives::addresses::is_reserved_dynamic_precompile_address;

/// Every system address carries this one-byte code, from the executor's marker list or from
/// genesis. EIP-3541 keeps any deployed code from starting with it.
const MARKER_CODE: [u8; 1] = [0xef];

/// Safe `SELFDESTRUCT` instruction wrapper that redirects protected beneficiaries to `Address::ZERO`.
pub(crate) struct GuardedSelfdestruct;

impl Instruction<OutbeEvmTypes> for GuardedSelfdestruct {
    const FLAGS: InstructionFlags =
        <instr::selfdestruct<OutbeEvmTypes> as Instruction<OutbeEvmTypes>>::FLAGS;

    fn execute(
        pc: &mut Pc,
        mut stack: StackMut<'_>,
        state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
    ) -> InstrResult {
        if !state.is_static() {
            if let Some(top) = stack.as_slice_mut().last_mut() {
                let address = state.message().destination;
                let target = top.to_address();
                if goes_to_address_zero(state, address, target)? {
                    *top = Word::ZERO;
                }
            }
        }
        instr::selfdestruct::<OutbeEvmTypes>::execute(pc, stack, state)
    }
}

fn goes_to_address_zero(
    state: &mut InterpreterState<'_, '_, OutbeEvmTypes>,
    address: Address,
    target: Address,
) -> std::result::Result<bool, InstrStop> {
    if target == address {
        let is_created = state
            .host()
            .state_mut()
            .account(&address)
            .map(|account| account.is_created())
            .map_err(|err| state.fail(ExecutionError::Database(err)))?;
        return Ok(is_created);
    }
    if target == Address::ZERO || is_reserved_dynamic_precompile_address(target) {
        return Ok(false);
    }
    // Read the code hash without loading or warming the account so the gas of the normal path does
    // not change.
    let code_hash = state
        .host()
        .state_mut()
        .account_info_untracked(&target)
        .map_err(|err| state.fail(ExecutionError::Database(err)))?
        .map(|info| info.code_hash)
        .unwrap_or_default();
    Ok(code_hash == keccak256(MARKER_CODE))
}
