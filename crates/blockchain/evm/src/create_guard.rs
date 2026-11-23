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

#[cfg(test)]
mod tests {
    use crate::OutbeEvmFactory;
    use alloy_consensus::TxLegacy;
    use alloy_primitives::{keccak256, Address, Bytes, TxKind, B256, U256};
    use evm2::{
        bytecode::Bytecode,
        env::BlockEnvExt,
        ethereum::TxEnvelope,
        evm::{AccountInfo, CacheDB, EmptyDB},
        interpreter::InstrStop,
        SpecId,
    };
    use outbe_primitives::addresses::is_reserved_dynamic_precompile_address;
    use reth_ethereum::evm::EthEvmEnv;
    use reth_primitives_traits::Recovered;

    const GAS_LIMIT: u64 = 1_000_000;
    const CALLER: Address = Address::repeat_byte(0xb1);

    fn test_env() -> EthEvmEnv<crate::factory::OutbeEvmTypes> {
        EthEvmEnv::new(
            SpecId::PRAGUE,
            BlockEnvExt {
                gas_limit: U256::from(30_000_000_u64),
                ..Default::default()
            },
            1,
        )
    }

    fn address_from_counter(counter: u64) -> Address {
        let mut bytes = [0u8; 20];
        bytes[12..].copy_from_slice(&counter.to_be_bytes());
        Address::from(bytes)
    }

    fn find_create_caller(nonce: u64, reserved: bool) -> Address {
        (1..2_000_000)
            .map(address_from_counter)
            .find(|addr| is_reserved_dynamic_precompile_address(addr.create(nonce)) == reserved)
            .expect("search range contains both address classes")
    }

    fn find_create2_salt(factory: Address, initcode: &[u8]) -> B256 {
        let hash = keccak256(initcode);
        (0u64..2_000_000)
            .map(|n| B256::from(U256::from(n)))
            .find(|salt| is_reserved_dynamic_precompile_address(factory.create2(*salt, hash)))
            .expect("search range contains reserved CREATE2 destination")
    }

    fn funded(nonce: u64) -> AccountInfo {
        AccountInfo {
            balance: U256::from(10_u64).pow(U256::from(18_u64)),
            nonce,
            ..Default::default()
        }
    }

    /// Factory bytecode running `CREATE(value=0, offset=0, size=1)` with initcode `[0x00]`
    /// and storing the returned address (or `0` on collision) at slot 0.
    fn nested_create_factory_code() -> Vec<u8> {
        // MSTORE8(0, 0x00) is unnecessary (zero memory); CREATE(0, 0, 1) -> SSTORE(0, addr)
        vec![0x60, 0x01, 0x60, 0x00, 0x60, 0x00, 0xf0, 0x60, 0x00, 0x55]
    }

    /// Factory bytecode running `CREATE2(value=0, offset=0, size=1, salt)` with initcode `[0x00]`
    /// and storing the returned address (or `0` on collision) at slot 0.
    fn nested_create2_factory_code(salt: B256) -> Vec<u8> {
        let mut code = vec![0x7f];
        code.extend_from_slice(salt.as_slice());
        code.extend_from_slice(&[0x60, 0x01, 0x60, 0x00, 0x60, 0x00, 0xf5, 0x60, 0x00, 0x55]);
        code
    }

    #[test]
    fn top_level_create_into_reserved_dynamic_precompile_halts_with_collision_and_bumps_nonce() {
        let reserved_caller = find_create_caller(0, true);
        let reserved_dest = reserved_caller.create(0);
        let adjacent_caller = find_create_caller(0, false);

        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(&reserved_caller, funded(0));
        let mut evm = OutbeEvmFactory::new(crate::host::stub_host()).create_evm(db, test_env());

        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(1),
                nonce: 0,
                gas_price: 0,
                gas_limit: GAS_LIMIT,
                to: TxKind::Create,
                value: U256::ZERO,
                input: Bytes::from_static(&[0x00]),
            }),
            reserved_caller,
        );
        let detached = evm.transact(&tx).expect("executes as halt").detach();
        assert_eq!(detached.result.stop, InstrStop::CreateCollision);
        assert_eq!(
            detached
                .pending_state
                .account_changes_for(&reserved_caller)
                .and_then(|acc| acc.current.as_ref().map(|info| info.nonce)),
            Some(1)
        );
        if let Some(dest) = detached.pending_state.account_changes_for(&reserved_dest) {
            if let Some(info) = &dest.current {
                assert_eq!(info.nonce, 0);
                assert!(info.is_empty_code_hash());
            }
        }

        // Adjacent non-reserved destination deploys normally.
        let mut adj_db = CacheDB::new(EmptyDB::default());
        adj_db.insert_account_info(&adjacent_caller, funded(0));
        let mut adj_evm =
            OutbeEvmFactory::new(crate::host::stub_host()).create_evm(adj_db, test_env());
        let adj_tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(1),
                nonce: 0,
                gas_price: 0,
                gas_limit: GAS_LIMIT,
                to: TxKind::Create,
                value: U256::ZERO,
                input: Bytes::from_static(&[0x00]),
            }),
            adjacent_caller,
        );
        let adj_res = adj_evm
            .transact(&adj_tx)
            .expect("adjacent executes")
            .commit();
        assert!(adj_res.is_success(), "{adj_res:?}");
    }

    #[test]
    fn nested_create_and_create2_into_reserved_dynamic_precompile_push_zero_and_bump_nonce() {
        // 1. Nested CREATE targeting reserved address (factory nonce starts at 1).
        let create_factory = find_create_caller(1, true);
        let create_dest = create_factory.create(1);
        let factory_code = Bytecode::new_legacy(nested_create_factory_code().into());

        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(&CALLER, funded(0));
        db.insert_account_info(
            &create_factory,
            AccountInfo {
                nonce: 1,
                code_hash: factory_code.hash_slow(),
                code: Some(factory_code),
                ..Default::default()
            },
        );
        let mut evm = OutbeEvmFactory::new(crate::host::stub_host()).create_evm(db, test_env());
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(1),
                nonce: 0,
                gas_price: 0,
                gas_limit: GAS_LIMIT,
                to: TxKind::Call(create_factory),
                value: U256::ZERO,
                input: Bytes::new(),
            }),
            CALLER,
        );
        let detached = evm.transact(&tx).expect("nested CREATE executes").detach();
        assert!(detached.result.is_success(), "{:?}", detached.result);
        let factory_changes = detached
            .pending_state
            .account_changes_for(&create_factory)
            .expect("factory changes recorded");
        assert_eq!(
            factory_changes.storage.slots[&U256::ZERO].value.current,
            U256::ZERO,
            "nested CREATE into reserved address must push 0"
        );
        assert_eq!(
            factory_changes.current.as_ref().map(|info| info.nonce),
            Some(2),
            "nested CREATE collision must still increment factory nonce"
        );
        if let Some(dest) = detached.pending_state.account_changes_for(&create_dest) {
            if let Some(info) = &dest.current {
                assert_eq!(info.nonce, 0);
                assert!(info.is_empty_code_hash());
            }
        }

        // 2. Nested CREATE2 targeting reserved address.
        let create2_factory = Address::repeat_byte(0xc4);
        let salt = find_create2_salt(create2_factory, &[0x00]);
        let create2_dest = create2_factory.create2(salt, keccak256([0x00]));
        let c2_code = Bytecode::new_legacy(nested_create2_factory_code(salt).into());

        let mut db2 = CacheDB::new(EmptyDB::default());
        db2.insert_account_info(&CALLER, funded(0));
        db2.insert_account_info(
            &create2_factory,
            AccountInfo {
                nonce: 1,
                code_hash: c2_code.hash_slow(),
                code: Some(c2_code),
                ..Default::default()
            },
        );
        let mut evm2 = OutbeEvmFactory::new(crate::host::stub_host()).create_evm(db2, test_env());
        let tx2 = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(1),
                nonce: 0,
                gas_price: 0,
                gas_limit: GAS_LIMIT,
                to: TxKind::Call(create2_factory),
                value: U256::ZERO,
                input: Bytes::new(),
            }),
            CALLER,
        );
        let detached2 = evm2
            .transact(&tx2)
            .expect("nested CREATE2 executes")
            .detach();
        assert!(detached2.result.is_success(), "{:?}", detached2.result);
        let c2_changes = detached2
            .pending_state
            .account_changes_for(&create2_factory)
            .expect("CREATE2 factory changes recorded");
        assert_eq!(
            c2_changes.storage.slots[&U256::ZERO].value.current,
            U256::ZERO,
            "nested CREATE2 into reserved address must push 0"
        );
        assert_eq!(
            c2_changes.current.as_ref().map(|info| info.nonce),
            Some(2),
            "nested CREATE2 collision must still increment factory nonce"
        );
        if let Some(dest) = detached2.pending_state.account_changes_for(&create2_dest) {
            if let Some(info) = &dest.current {
                assert_eq!(info.nonce, 0);
                assert!(info.is_empty_code_hash());
            }
        }
    }
}
