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

#[cfg(test)]
mod tests {
    use super::MARKER_CODE;
    use crate::OutbeEvmFactory;
    use alloy_consensus::TxLegacy;
    use alloy_primitives::{Address, Bytes, TxKind, U256};
    use evm2::{
        bytecode::Bytecode,
        env::BlockEnvExt,
        ethereum::TxEnvelope,
        evm::{AccountInfo, CacheDB, EmptyDB},
        SpecId,
    };
    use reth_ethereum::evm::EthEvmEnv;
    use reth_primitives_traits::Recovered;

    const CALLER: Address = Address::repeat_byte(0xc1);
    const SYSTEM_ADDR: Address = Address::repeat_byte(0x95);
    const NORMAL_ADDR: Address = Address::repeat_byte(0x42);
    const CONTRACT: Address = Address::repeat_byte(0xd1);
    const AMOUNT: u64 = 5_000_000;
    const GAS_LIMIT: u64 = 1_000_000;

    fn test_env() -> EthEvmEnv<crate::factory::OutbeEvmTypes> {
        EthEvmEnv::new(
            SpecId::PRAGUE,
            BlockEnvExt {
                beneficiary: Address::repeat_byte(0xfe),
                gas_limit: U256::from(30_000_000_u64),
                ..Default::default()
            },
            1,
        )
    }

    fn put_code(db: &mut CacheDB<EmptyDB>, address: Address, balance: U256, code: &[u8]) {
        let bytecode = Bytecode::new_legacy(Bytes::copy_from_slice(code));
        db.insert_account_info(
            &address,
            AccountInfo {
                balance,
                code_hash: bytecode.hash_slow(),
                code: Some(bytecode),
                ..Default::default()
            },
        );
    }

    fn selfdestruct_to(target: Address) -> Vec<u8> {
        let mut code = vec![0x73];
        code.extend_from_slice(target.as_slice());
        code.push(0xff);
        code
    }

    #[test]
    fn selfdestruct_to_marker_address_redirects_balance_to_address_zero() {
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            &CALLER,
            AccountInfo {
                balance: U256::from(AMOUNT),
                ..Default::default()
            },
        );
        put_code(&mut db, SYSTEM_ADDR, U256::ZERO, &MARKER_CODE);
        put_code(
            &mut db,
            CONTRACT,
            U256::from(AMOUNT),
            &selfdestruct_to(SYSTEM_ADDR),
        );

        let mut evm = OutbeEvmFactory::new(crate::host::stub_host()).create_evm(db, test_env());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(1),
                nonce: 0,
                gas_price: 0,
                gas_limit: GAS_LIMIT,
                to: TxKind::Call(CONTRACT),
                value: U256::ZERO,
                input: Bytes::new(),
            }),
            CALLER,
        );
        let detached = evm.transact(&tx).expect("executes").detach();
        assert!(detached.result.is_success(), "{:?}", detached.result);

        let zero_balance = detached
            .pending_state
            .account_changes_for(&Address::ZERO)
            .and_then(|acc| acc.current.as_ref().map(|info| info.balance))
            .unwrap_or(U256::ZERO);
        assert_eq!(zero_balance, U256::from(AMOUNT));
        let system_balance = detached
            .pending_state
            .account_changes_for(&SYSTEM_ADDR)
            .and_then(|acc| acc.current.as_ref().map(|info| info.balance))
            .unwrap_or(U256::ZERO);
        assert_eq!(system_balance, U256::ZERO);
    }

    #[test]
    fn selfdestruct_of_same_tx_created_contract_to_self_redirects_to_address_zero() {
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            &CALLER,
            AccountInfo {
                balance: U256::from(AMOUNT),
                ..Default::default()
            },
        );
        let mut evm = OutbeEvmFactory::new(crate::host::stub_host()).create_evm(db, test_env());
        // Initcode: ADDRESS (0x30) SELFDESTRUCT (0xff)
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(1),
                nonce: 0,
                gas_price: 0,
                gas_limit: GAS_LIMIT,
                to: TxKind::Create,
                value: U256::from(AMOUNT),
                input: Bytes::from_static(&[0x30, 0xff]),
            }),
            CALLER,
        );
        let detached = evm.transact(&tx).expect("executes").detach();
        assert!(detached.result.is_success(), "{:?}", detached.result);

        let zero_balance = detached
            .pending_state
            .account_changes_for(&Address::ZERO)
            .and_then(|acc| acc.current.as_ref().map(|info| info.balance))
            .unwrap_or(U256::ZERO);
        assert_eq!(zero_balance, U256::from(AMOUNT));
    }

    #[test]
    fn selfdestruct_to_normal_address_keeps_standard_ethereum_transfer() {
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            &CALLER,
            AccountInfo {
                balance: U256::from(AMOUNT),
                ..Default::default()
            },
        );
        put_code(
            &mut db,
            CONTRACT,
            U256::from(AMOUNT),
            &selfdestruct_to(NORMAL_ADDR),
        );

        let mut evm = OutbeEvmFactory::new(crate::host::stub_host()).create_evm(db, test_env());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(1),
                nonce: 0,
                gas_price: 0,
                gas_limit: GAS_LIMIT,
                to: TxKind::Call(CONTRACT),
                value: U256::ZERO,
                input: Bytes::new(),
            }),
            CALLER,
        );
        let detached = evm.transact(&tx).expect("executes").detach();
        assert!(detached.result.is_success(), "{:?}", detached.result);

        let normal_balance = detached
            .pending_state
            .account_changes_for(&NORMAL_ADDR)
            .and_then(|acc| acc.current.as_ref().map(|info| info.balance))
            .unwrap_or(U256::ZERO);
        assert_eq!(normal_balance, U256::from(AMOUNT));
        assert!(detached
            .pending_state
            .account_changes_for(&Address::ZERO)
            .is_none());
    }
}
