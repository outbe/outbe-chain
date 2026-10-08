//! Storage/subcall API tests using real revm frames.
use super::*;
use alloy_primitives::Bytes;
use revm::context_interface::journaled_state::account::JournaledAccountTr as _;
use revm::context_interface::JournalTr as _;
use revm::{
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    Context,
};

const TARGET: Address = Address::repeat_byte(0xab);
const CALLER: Address = Address::repeat_byte(0xc0);
const GAS_WORD: &[u8] = &[0x5a, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3];

fn context(code: &[u8]) -> EthEvmContext<CacheDB<EmptyDB>> {
    let mut db = CacheDB::new(EmptyDB::default());
    let code = Bytecode::new_raw(Bytes::copy_from_slice(code));
    db.insert_account_info(
        TARGET,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
    let mut ctx = Context::mainnet().with_db(db).modify_cfg_chained(|cfg| {
        cfg.set_spec_and_mainnet_gas_params(SpecId::PRAGUE);
    });
    // A real precompile frame already loaded its own account.
    ctx.journal_mut().load_account_mut(CALLER).unwrap();
    ctx
}

fn provider<DB: Database + Debug>(
    ctx: &mut EthEvmContext<DB>,
    gas: u64,
) -> CtxStorageProvider<'_, DB> {
    CtxStorageProvider::new(
        ctx,
        SubcallGasMeter::new(gas),
        CtxStorageProviderConfig {
            is_static: false,
            self_address: CALLER,
            reentrancy_stack: ReentrancyStack,
            spec: SpecId::PRAGUE,
            genesis_hash: B256::ZERO,
            runtime_body_readers: None,
            abort_bridge: Default::default(),
            execution_scope: Arc::new(ExecutionScope::default()),
            ocomp_finality_authority: None,
            ocomp_activation_block_meter: Arc::new(OcompActivationBlockMeter),
            ocomp_lifecycle_active: false,
            lysis_activation_entitled: false,
            metadosis_mutation_entitlements: MetadosisMutationEntitlements::NONE,
        },
    )
}

fn input(gas_limit: u64) -> SubCallInput {
    SubCallInput {
        target: TARGET,
        value: U256::ZERO,
        calldata: Bytes::new(),
        gas_limit,
        is_static: false,
    }
}

#[test]
fn explicit_cap_and_cold_then_warm_access_are_charged_once() {
    let mut ctx = context(GAS_WORD);
    let mut p = provider(&mut ctx, 50_000);
    for access_cost in [2600, 100] {
        let before = p.gas.remaining();
        let child = p.sub_call(input(1000)).unwrap();
        assert!(matches!(child.status, SubCallStatus::Success));
        assert_eq!(U256::from_be_slice(&child.returndata), U256::from(998));
        assert_eq!(before - p.gas.remaining(), access_cost + child.gas_used);
    }
}

#[test]
fn max_sentinel_uses_eip150_after_upfront_cost() {
    let mut ctx = context(GAS_WORD);
    let mut p = provider(&mut ctx, 50_000);
    let child = p.sub_call(input(u64::MAX)).unwrap();
    let available = 50_000 - 2600;
    assert_eq!(
        U256::from_be_slice(&child.returndata),
        U256::from(available - available / 64 - 2)
    );
    assert_eq!(p.gas.remaining(), 50_000 - 2600 - child.gas_used);
}

#[test]
fn caught_invalid_opcode_is_not_free() {
    let mut ctx = context(&[0xfe]);
    let mut p = provider(&mut ctx, 50_000);
    let child = p.sub_call(input(1000)).unwrap();
    assert!(matches!(
        child.status,
        SubCallStatus::Halt(SubCallError::EvmHalt(_))
    ));
    assert_eq!(child.gas_used, 1000);
    assert_eq!(p.gas.remaining(), 50_000 - 2600 - 1000);
}

#[test]
fn admission_oog_exhausts_parent_and_cannot_be_hidden_by_try_call() {
    let mut ctx = context(GAS_WORD);
    let mut p = provider(&mut ctx, 99);
    assert!(matches!(
        p.sub_call(input(1000)),
        Err(SubCallError::ParentOutOfGas)
    ));
    assert!(
        p.subcall_out_of_gas,
        "dispatcher must override a handler that catches this error"
    );
    assert_eq!(p.gas.remaining(), 0);
}

#[test]
fn insufficient_balance_returns_unused_child_allowance() {
    let mut ctx = context(GAS_WORD);
    let mut p = provider(&mut ctx, 50_000);
    let mut call = input(1000);
    call.value = U256::ONE;
    let child = p.sub_call(call).unwrap();
    assert!(matches!(
        child.status,
        SubCallStatus::Halt(SubCallError::EvmHalt(
            revm::context_interface::result::HaltReason::OutOfFunds
        ))
    ));
    assert_eq!(child.gas_used, 0);
    assert_eq!(p.gas.remaining(), 50_000 - 2600 - 9000);
}

#[test]
fn static_value_is_rejected_before_entry_but_zero_value_call_can_read() {
    let mut ctx = context(GAS_WORD);
    let mut p = provider(&mut ctx, 50_000);
    p.is_static = true;
    let mut call = input(1000);
    call.value = U256::ONE;
    assert!(matches!(
        p.sub_call(call).unwrap().status,
        SubCallStatus::Halt(SubCallError::StateChangeDuringStaticCall)
    ));
    assert_eq!(p.gas.remaining(), 50_000);
    assert!(matches!(
        p.sub_call(input(1000)).unwrap().status,
        SubCallStatus::Success
    ));
}

#[test]
fn child_memory_obeys_configured_limit() {
    let mut ctx = context(&[0x60, 0, 0x60, 64, 0x52, 0]);
    ctx.cfg.memory_limit = 32;
    let mut p = provider(&mut ctx, 50_000);
    let child = p.sub_call(input(1000)).unwrap();
    assert!(matches!(
        child.status,
        SubCallStatus::Halt(SubCallError::OutOfGas)
    ));
    assert_eq!(child.gas_used, 1000);
}

#[test]
fn refund_and_storage_survive_success_only() {
    for revert in [false, true] {
        let mut code = vec![0x60, 0, 0x60, 0, 0x55];
        code.extend_from_slice(if revert {
            &[0x60, 0, 0x60, 0, 0xfd]
        } else {
            &[0]
        });
        let mut ctx = context(&code);
        ctx.journaled_state
            .database
            .insert_account_storage(TARGET, U256::ZERO, U256::ONE)
            .unwrap();
        let mut p = provider(&mut ctx, 50_000);
        let child = p.sub_call(input(20_000)).unwrap();
        assert_eq!(child.gas_refunded, if revert { 0 } else { 4800 });
        assert_eq!(p.gas.refunded(), child.gas_refunded);
        assert_eq!(
            p.sload(TARGET, U256::ZERO).unwrap(),
            if revert { U256::ONE } else { U256::ZERO }
        );
    }
}

#[test]
fn zero_value_accounting_matches_native_call_after_wrapper_instructions() {
    for code in [GAS_WORD, &[0x60, 0, 0x60, 0, 0xfd][..], &[0xfe][..]] {
        let mut ctx = context(code);
        let mut p = provider(&mut ctx, 50_000);
        p.sub_call(input(1000)).unwrap();
        let provider_charge = 50_000 - p.gas.remaining();
        let mut ctx = context(code);
        // Seven PUSHes, CALL, POP, STOP. Native CALL catches child failure.
        let mut native = vec![0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x73];
        native.extend_from_slice(TARGET.as_slice());
        native.extend_from_slice(&[0x61, 0x03, 0xe8, 0xf1, 0x50, 0]);
        let native = Bytecode::new_raw(native.into());
        let address = Address::repeat_byte(0xde);
        ctx.journaled_state.database.insert_account_info(
            address,
            AccountInfo {
                code_hash: native.hash_slow(),
                code: Some(native),
                ..Default::default()
            },
        );
        let output = sub_call::run(
            &mut ctx,
            sub_call::SubCallEnvironment {
                self_address: CALLER,
                outer_is_static: false,
                spec: SpecId::PRAGUE,
                runtime_body_readers: None,
                execution_scope: Arc::new(ExecutionScope::default()),
            },
            SubCallInput {
                target: address,
                ..input(50_000)
            },
        )
        .unwrap();
        assert!(matches!(output.status, SubCallStatus::Success));
        assert_eq!(
            output.gas_used,
            provider_charge + 23,
            "native PUSH/POP overhead"
        );
    }
}

#[test]
fn delegated_target_is_priced_and_warmed_before_execution() {
    let mut ctx = context(GAS_WORD);
    let delegated = Address::repeat_byte(0xdd);
    let code = Bytecode::new_raw(Bytes::copy_from_slice(GAS_WORD));
    ctx.journaled_state.database.insert_account_info(
        delegated,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
    let designation = Bytecode::new_eip7702(delegated);
    ctx.journaled_state.database.insert_account_info(
        TARGET,
        AccountInfo {
            code_hash: designation.hash_slow(),
            code: Some(designation),
            ..Default::default()
        },
    );
    let mut p = provider(&mut ctx, 50_000);
    for access_cost in [5200, 200] {
        let before = p.gas.remaining();
        let child = p.sub_call(input(1000)).unwrap();
        assert!(matches!(child.status, SubCallStatus::Success));
        assert_eq!(before - p.gas.remaining(), access_cost + child.gas_used);
    }
}

#[test]
fn value_transfer_uses_explicit_allowance_without_automatic_stipend() {
    let mut ctx = context(GAS_WORD);
    ctx.journal_mut()
        .load_account_mut(CALLER)
        .unwrap()
        .set_balance(U256::from(10));
    let mut p = provider(&mut ctx, 50_000);
    let mut call = input(1000);
    call.value = U256::ONE;
    let child = p.sub_call(call).unwrap();
    assert!(matches!(child.status, SubCallStatus::Success));
    assert_eq!(U256::from_be_slice(&child.returndata), U256::from(998));
    assert_eq!(p.gas.remaining(), 50_000 - 2600 - 9000 - child.gas_used);
    assert_eq!(p.account_info(TARGET).unwrap().balance, U256::ONE);
}

#[test]
fn depth_rejection_returns_reserved_allowance() {
    let mut ctx = context(GAS_WORD);
    while ctx.journal().depth() < 1025 {
        ctx.journal_mut().checkpoint();
    }
    let mut p = provider(&mut ctx, 50_000);
    let child = p.sub_call(input(1000)).unwrap();
    assert!(matches!(
        child.status,
        SubCallStatus::Halt(SubCallError::DepthLimitExceeded)
    ));
    assert_eq!(child.gas_used, 0);
    assert_eq!(p.gas.remaining(), 50_000 - 2600);
}

#[test]
fn native_calls_inside_child_keep_the_enclosing_depth_budget() {
    let leaf = Address::repeat_byte(0xac);
    let empty = Address::repeat_byte(0xad);
    // TARGET forwards the leaf's word; leaf returns CALL(empty)'s success bit.
    let mut forward = vec![0x60, 32, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x73];
    forward.extend_from_slice(leaf.as_slice());
    forward.extend_from_slice(&[0x5a, 0xf1, 0x50, 0x60, 32, 0x60, 0, 0xf3]);
    let mut probe = vec![0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x73];
    probe.extend_from_slice(empty.as_slice());
    probe.extend_from_slice(&[0x5a, 0xf1, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    for (enclosing_depth, expected) in [(1022, 1), (1023, 0)] {
        let mut ctx = context(&forward);
        let code = Bytecode::new_raw(Bytes::copy_from_slice(&probe));
        ctx.journaled_state.database.insert_account_info(
            leaf,
            AccountInfo {
                code_hash: code.hash_slow(),
                code: Some(code),
                ..Default::default()
            },
        );
        while ctx.journal().depth() < enclosing_depth {
            ctx.journal_mut().checkpoint();
        }
        let mut p = provider(&mut ctx, 100_000);
        let child = p.sub_call(input(50_000)).unwrap();
        assert!(matches!(child.status, SubCallStatus::Success));
        assert_eq!(
            U256::from_be_slice(&child.returndata),
            U256::from(expected),
            "nested native CALL must respect enclosing depth {enclosing_depth}"
        );
    }
}

#[test]
fn child_entry_at_the_native_depth_limit_is_allowed() {
    let mut ctx = context(GAS_WORD);
    while ctx.journal().depth() < 1024 {
        ctx.journal_mut().checkpoint();
    }
    let mut p = provider(&mut ctx, 50_000);
    let child = p.sub_call(input(1000)).unwrap();
    assert!(matches!(child.status, SubCallStatus::Success), "{child:?}");
}

#[derive(Debug)]
struct FailedDatabase {
    at_entry: bool,
}

#[derive(Debug)]
struct FailedRead;
impl std::fmt::Display for FailedRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("database unavailable")
    }
}
impl std::error::Error for FailedRead {}
impl revm::database_interface::DBErrorMarker for FailedRead {}

impl Database for FailedDatabase {
    type Error = FailedRead;
    fn basic(&mut self, address: Address) -> std::result::Result<Option<AccountInfo>, FailedRead> {
        if address != TARGET {
            return Ok(None);
        }
        if self.at_entry {
            return Err(FailedRead);
        }
        let code = Bytecode::new_raw(Bytes::from_static(&[0x60, 0, 0x54, 0]));
        Ok(Some(AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        }))
    }
    fn code_by_hash(&mut self, _: B256) -> std::result::Result<Bytecode, FailedRead> {
        Ok(Bytecode::default())
    }
    fn storage(&mut self, _: Address, _: U256) -> std::result::Result<U256, FailedRead> {
        Err(FailedRead)
    }
    fn block_hash(&mut self, _: u64) -> std::result::Result<B256, FailedRead> {
        Ok(B256::ZERO)
    }
}

#[test]
fn account_and_child_storage_database_failures_remain_fatal() {
    for at_entry in [false, true] {
        let mut ctx = Context::mainnet().with_db(FailedDatabase { at_entry });
        let mut p = provider(&mut ctx, 50_000);
        let error = p
            .sub_call(input(10_000))
            .expect_err("DB failure has no VM status");
        assert!(matches!(
            error,
            SubCallError::DatabaseError(_) | SubCallError::Fatal(_)
        ));
        let output = crate::precompiles::map_outbe_precompile_result(Err(error.into()), 10);
        assert!(
            output.is_err(),
            "DB failure must not produce a user receipt"
        );
    }
}

#[test]
fn inbox_getter_charges_actual_work_once_and_retains_its_child_cap() {
    use alloy_sol_types::SolCall;
    use outbe_primitives::{
        block::BlockContext,
        storage::{direct::DirectStorageProvider, StorageHandle},
    };
    for code in [GAS_WORD, &[0xfe][..]] {
        let mut ctx = context(code);
        let mut seed = DirectStorageProvider::new(
            &mut ctx.journaled_state.database,
            BlockContext::new(1, 1, 1, CALLER, vec![CALLER]),
        );
        StorageHandle::enter(&mut seed, |storage| {
            outbe_l2registry::schema::L2RegistryContract::new(storage)
                .register_network(42, TARGET, &[])
                .unwrap();
        });
        seed.flush().unwrap();
        drop(seed);
        let mut p = provider(&mut ctx, 200_000);
        let calldata =
            outbe_l2registry::precompile::IL2Registry::getNetworkCall { chainId: 42 }.abi_encode();
        let result = outbe_l2registry::precompile::dispatch(
            StorageHandle::new(&mut p),
            &calldata,
            CALLER,
            U256::ZERO,
        );
        // GAS_WORD is not a valid ABI key; INVALID becomes InboxKeyCallFailed.
        // Neither is a node failure or a reason to precharge the entire cap.
        assert!(matches!(result, Err(PrecompileError::Revert(_))));
        let charge = p.gas.limit() - p.gas.remaining();
        if code == GAS_WORD {
            assert!(
                (3000..3500).contains(&charge),
                "getter charged {charge} instead of its small actual work"
            );
        } else {
            assert!(
                (103_000..103_500).contains(&charge),
                "child must spend its explicit 100000 cap, once: {charge}"
            );
        }
    }
}

#[test]
fn unsupported_state_gas_is_rejected_before_execution() {
    let mut ctx = context(GAS_WORD);
    ctx.cfg.enable_amsterdam_eip8037 = true;
    let mut p = provider(&mut ctx, 50_000);
    assert!(matches!(
        p.sub_call(input(1000)),
        Err(SubCallError::Fatal(_))
    ));
    assert_eq!(p.gas.remaining(), 50_000);
}

#[test]
fn reserved_system_command_checks_static_authority_before_decode() {
    use outbe_primitives::{
        addresses::SYSTEM_ADDRESS,
        storage::{hashmap::HashMapStorageProvider, StorageHandle},
    };
    let mut p = HashMapStorageProvider::new(1);
    p.set_static(true);
    let result = crate::begin_block_precompile::dispatch(
        StorageHandle::new(&mut p),
        b"not a system command",
        SYSTEM_ADDRESS,
        U256::ZERO,
    );
    assert!(matches!(result, Err(PrecompileError::WriteProtection)));
}
