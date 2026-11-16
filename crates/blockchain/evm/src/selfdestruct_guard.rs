//! SELFDESTRUCT that would strand COEN on a system address, or burn it in a contract that the same
//! transaction made, sends the balance to address 0 instead. The Promis Limit burns it from there.

use alloy_primitives::{keccak256, Address, U256};
use outbe_primitives::addresses::is_reserved_dynamic_precompile_address;
use revm::{
    context::{ContextTr, Database, JournalTr},
    context_interface::context::ContextError,
    handler::instructions::EthInstructions,
    interpreter::{
        instructions::host::selfdestruct,
        interpreter_types::{InputsTr, StackTr},
        Host, Instruction, InstructionContext, InstructionResult, InterpreterTypes,
    },
    state::EvmState,
};

const SELFDESTRUCT_OPCODE: u8 = 0xff;
/// Every system address carries this one-byte code, from the executor's marker list or from
/// genesis. EIP-3541 keeps any deployed code from starting with it.
const MARKER_CODE: [u8; 1] = [0xef];

pub(crate) fn install<WIRE, HOST>(instructions: &mut EthInstructions<WIRE, HOST>)
where
    WIRE: InterpreterTypes,
    HOST: Host + ContextTr<Journal: JournalTr<State = EvmState>>,
{
    let gas = instructions.gas_table()[SELFDESTRUCT_OPCODE as usize];
    instructions.insert_instruction(
        SELFDESTRUCT_OPCODE,
        Instruction::new(guarded_selfdestruct::<WIRE, HOST>),
        gas,
    );
}

fn guarded_selfdestruct<WIRE, HOST>(
    context: InstructionContext<'_, HOST, WIRE>,
) -> revm::interpreter::InstructionExecResult
where
    WIRE: InterpreterTypes,
    HOST: Host + ContextTr<Journal: JournalTr<State = EvmState>> + ?Sized,
{
    let address = context.interpreter.input.target_address();
    if let Some(top) = context.interpreter.stack.top() {
        let target = Address::from_word((*top).into());
        if goes_to_address_zero(&mut *context.host, address, target)? {
            *top = U256::ZERO;
        }
    }
    selfdestruct::<WIRE, HOST>(context)
}

fn goes_to_address_zero<HOST>(
    host: &mut HOST,
    address: Address,
    target: Address,
) -> Result<bool, InstructionResult>
where
    HOST: ContextTr<Journal: JournalTr<State = EvmState>> + ?Sized,
{
    if target == address {
        return Ok(host
            .journal()
            .evm_state()
            .get(&address)
            .is_some_and(|account| account.is_created_locally()));
    }
    if target == Address::ZERO || is_reserved_dynamic_precompile_address(target) {
        return Ok(false);
    }
    // Read the code hash without loading the account, so the gas of the normal path does not
    // change.
    let code_hash = match host.journal().evm_state().get(&target) {
        Some(account) => account.info.code_hash,
        None => match host.journal_mut().db_mut().basic(target) {
            Ok(info) => info.map(|info| info.code_hash).unwrap_or_default(),
            Err(err) => {
                *host.error() = Err(ContextError::Db(err));
                return Err(InstructionResult::FatalExternalError);
            }
        },
    };
    Ok(code_hash == keccak256(MARKER_CODE))
}
