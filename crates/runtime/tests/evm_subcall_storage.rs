//! Storage and subcall API tests using real revm frames.
use alloy_evm::eth::EthEvmContext;
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolError;
use core::fmt::Debug;
use outbe_evm::gas::SubcallGasMeter;
use outbe_evm::storage::{CtxStorageProvider, CtxStorageProviderConfig, ReentrancyStack};
use outbe_evm::EvmRuntimeHost;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::{
    MutationEntitlements, PrecompileStorageProvider, SubCallError, SubCallInput, SubCallStatus,
};
use revm::context_interface::journaled_state::account::JournaledAccountTr as _;
use revm::context_interface::ContextTr as _;
use revm::context_interface::JournalTr as _;
use revm::primitives::hardfork::SpecId;
use revm::state::{AccountInfo, Bytecode};
use revm::{
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    Context, Database,
};

const TARGET: Address = Address::repeat_byte(0xab);
const CALLER: Address = Address::repeat_byte(0xc0);
const GAS_WORD: &[u8] = &[0x5a, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3];

#[test]
fn trigger_budget_attributes_only_locally_reduced_child_oog() {
    use outbe_primitives::storage::StorageHandle;
    for explicit_cap in [100, u64::MAX] {
        let mut ctx = context(&[0x5b, 0x60, 0, 0x56]); // Infinite jump loop.
        let mut p = provider(&mut ctx, 50_000);
        let result = StorageHandle::enter(&mut p, |storage| {
            storage.with_trigger_budget(3600, || {
                let child =
                    storage.try_call_with_gas(TARGET, U256::ZERO, Bytes::new(), explicit_cap);
                if explicit_cap == 100 {
                    assert!(matches!(
                        child.unwrap().status,
                        SubCallStatus::Halt(SubCallError::OutOfGas)
                    ));
                } else {
                    assert!(matches!(
                        child,
                        Err(SubCallError::TriggerBudgetExceeded {
                            consumed_gas: 3600,
                            budget_gas: 3600
                        })
                    ));
                }
                // Deliberately swallow the child result.
                Ok(())
            })
        });
        if explicit_cap == 100 {
            result.unwrap();
            assert_eq!(p.gas.remaining(), 47_300);
        } else {
            assert!(matches!(
                result,
                Err(PrecompileError::TriggerBudgetExceeded {
                    consumed_gas: 3600,
                    budget_gas: 3600
                })
            ));
            assert_eq!(p.gas.remaining(), 46_400);
        }
        assert!(!p.subcall_out_of_gas);
        assert!(p.trigger_budget.is_none());
    }
}

#[test]
fn trigger_budget_does_not_attribute_fixed_memory_limit_to_local_allowance() {
    use outbe_primitives::storage::StorageHandle;
    let mut ctx = context(&[0x60, 0, 0x60, 64, 0x52, 0]);
    ctx.cfg.memory_limit = 32;
    let mut p = provider(&mut ctx, 50_000);
    StorageHandle::enter(&mut p, |storage| {
        storage.with_trigger_budget(4000, || {
            let child = storage.try_call(TARGET, U256::ZERO, Bytes::new())?;
            assert!(matches!(
                child.status,
                SubCallStatus::Halt(SubCallError::OutOfGas)
            ));
            Ok(())
        })
    })
    .unwrap();
    assert_eq!(p.gas.remaining(), 46_000);
    assert!(!p.subcall_out_of_gas);
}

#[test]
fn trigger_budget_admission_and_exact_success_do_not_exhaust_parent() {
    use outbe_primitives::storage::StorageHandle;
    for budget in [2599, 2600] {
        let mut ctx = context(&[0]);
        let mut p = provider(&mut ctx, 50_000);
        let result = StorageHandle::enter(&mut p, |storage| {
            storage.with_trigger_budget(budget, || {
                storage.call(TARGET, U256::ZERO, Bytes::new())?;
                Ok(())
            })
        });
        if budget == 2599 {
            assert!(matches!(
                result,
                Err(PrecompileError::TriggerBudgetExceeded {
                    consumed_gas: 100,
                    budget_gas: 2599
                })
            ));
            assert_eq!(p.gas.remaining(), 49_900);
        } else {
            result.unwrap();
            assert_eq!(p.gas.remaining(), 47_400);
        }
        assert!(!p.subcall_out_of_gas);
    }
}

#[test]
fn trigger_budget_remaining_reads_the_local_allowance_not_the_parent_meter() {
    use outbe_primitives::storage::StorageHandle;
    let mut ctx = context(&[0]);
    let mut p = provider(&mut ctx, 50_000);
    StorageHandle::enter(&mut p, |storage| {
        assert_eq!(storage.trigger_budget_remaining()?, None);
        storage.with_trigger_budget(5000, || {
            assert_eq!(storage.trigger_budget_remaining()?, Some(5000));
            storage.call(TARGET, U256::ZERO, Bytes::new())?;
            assert_eq!(storage.trigger_budget_remaining()?, Some(2400));
            assert_eq!(storage.metered_gas_remaining()?, Some(47_400));
            Ok(())
        })?;
        assert_eq!(storage.trigger_budget_remaining()?, None);
        Ok::<_, PrecompileError>(())
    })
    .unwrap();
}

#[test]
fn trigger_budget_returns_canonical_unused_allowance_without_double_charging() {
    let mut ctx = context(GAS_WORD);
    let mut p = provider(&mut ctx, 50_000);
    p.begin_trigger_budget(5000).unwrap();
    for admission in [2600, 100] {
        let local_before = p.trigger_budget.as_ref().unwrap().remaining;
        let parent_before = p.gas.remaining();
        let child = p.sub_call(input(u64::MAX)).unwrap();
        assert!(matches!(child.status, SubCallStatus::Success));
        let charged = admission + child.gas_used;
        assert_eq!(parent_before - p.gas.remaining(), charged);
        assert_eq!(
            local_before - p.trigger_budget.as_ref().unwrap().remaining,
            charged
        );
        p.trigger_budget_status().unwrap();
    }
    p.end_trigger_budget().unwrap();
}

#[test]
fn trigger_budget_returns_allowance_on_depth_rejection() {
    let mut ctx = context(GAS_WORD);
    while ctx.journal().depth() <= 1024 {
        ctx.journal_mut().checkpoint();
    }
    let mut p = provider(&mut ctx, 50_000);
    p.begin_trigger_budget(5000).unwrap();
    let child = p.sub_call(input(u64::MAX)).unwrap();
    assert!(matches!(
        child.status,
        SubCallStatus::Halt(SubCallError::DepthLimitExceeded)
    ));
    assert_eq!(p.trigger_budget.as_ref().unwrap().remaining, 2400);
    assert_eq!(p.gas.remaining(), 47_400);
    p.trigger_budget_status().unwrap();
}

#[test]
fn trigger_budget_coexists_with_legacy_ceiling_and_preserves_unscoped_refunds() {
    let mut ctx = context(&[0]);
    let mut p = provider(&mut ctx, 50_000);
    let checkpoint = p.checkpoint();
    p.refund_gas(9);
    p.checkpoint_revert(checkpoint).unwrap();
    assert_eq!(
        p.gas_refunded(),
        9,
        "unscoped checkpoint semantics are unchanged"
    );
    p.begin_gas_ceiling(100).unwrap();
    p.begin_trigger_budget(5000).unwrap();
    assert!(matches!(p.deduct_gas(101), Err(PrecompileError::OutOfGas)));
    assert_eq!(p.gas.remaining(), 49_900);
    assert_eq!(p.trigger_budget.as_ref().unwrap().remaining, 4900);
    p.trigger_budget_status().unwrap();
    p.gas_child_remaining = None;
    assert!(matches!(
        p.deduct_gas(4901),
        Err(PrecompileError::TriggerBudgetExceeded {
            consumed_gas: 100,
            budget_gas: 5000
        })
    ));
    assert_eq!(p.gas.remaining(), 49_900);
    assert!(!p.subcall_out_of_gas);
}

#[test]
fn trigger_budget_legacy_child_ceiling_caps_reservation_and_returns_unused() {
    let mut ctx = context(GAS_WORD);
    let mut p = provider(&mut ctx, 50_000);
    p.begin_gas_ceiling(5000).unwrap();
    p.begin_trigger_budget(20_000).unwrap();
    let child = p.sub_call(input(u64::MAX)).unwrap();
    assert!(matches!(child.status, SubCallStatus::Success));
    assert_eq!(U256::from_be_slice(&child.returndata), U256::from(2398));
    let charged = 2600 + child.gas_used;
    assert_eq!(p.gas.remaining(), 50_000 - charged);
    assert_eq!(
        p.trigger_budget.as_ref().unwrap().remaining,
        20_000 - charged
    );
    assert_eq!(p.gas_child_remaining, Some(5000 - charged));
    p.trigger_budget_status().unwrap();
    assert!(!p.subcall_out_of_gas);
}

#[test]
fn trigger_budget_nested_standard_precompile_consumes_only_actual_work() {
    let mut ctx = context(&[0]);
    let mut p = provider(&mut ctx, 50_000);
    p.begin_trigger_budget(5000).unwrap();
    let payload = Bytes::from_static(b"bounded identity precompile");
    let child = p
        .sub_call(SubCallInput {
            target: Address::with_last_byte(4),
            calldata: payload.clone(),
            ..input(u64::MAX)
        })
        .unwrap();
    assert!(matches!(child.status, SubCallStatus::Success));
    assert_eq!(child.returndata, payload);
    assert_eq!(child.gas_used, 18);
    assert_eq!(
        50_000 - p.gas.remaining(),
        5000 - p.trigger_budget.as_ref().unwrap().remaining
    );
    p.trigger_budget_status().unwrap();
}

#[test]
fn trigger_budget_rollback_removes_real_child_state_logs_balance_and_transient() {
    use outbe_primitives::storage::StorageHandle;
    // SSTORE(0,1), TSTORE(0,1), LOG0(empty), STOP.
    let mut ctx = context(&[
        0x60, 1, 0x60, 0, 0x55, 0x60, 1, 0x60, 0, 0x5d, 0x60, 0, 0x60, 0, 0xa0, 0,
    ]);
    let mut p = provider(&mut ctx, 200_000);
    p.increase_balance(CALLER, U256::from(10)).unwrap();
    StorageHandle::enter(&mut p, |storage| {
        storage.refund_gas(9).unwrap();
        let result = storage.with_trigger_budget(100_000, || {
            storage.call(TARGET, U256::ONE, Bytes::new())?;
            assert_eq!(storage.sload(TARGET, U256::ZERO)?, U256::ONE);
            assert_eq!(storage.tload(TARGET, U256::ZERO)?, U256::ONE);
            storage.refund_gas(1000)?;
            let _ = storage.deduct_gas(100_001);
            Ok(())
        });
        assert!(matches!(
            result,
            Err(PrecompileError::TriggerBudgetExceeded { .. })
        ));
        assert_eq!(storage.sload(TARGET, U256::ZERO).unwrap(), U256::ZERO);
        assert_eq!(storage.tload(TARGET, U256::ZERO).unwrap(), U256::ZERO);
        assert_eq!(storage.balance(TARGET).unwrap(), U256::ZERO);
        assert_eq!(storage.balance(CALLER).unwrap(), U256::from(10));
        assert_eq!(storage.gas_refunded().unwrap(), 9);
        assert!(storage.gas_used().unwrap() > 22_000);
        storage
            .with_trigger_budget(100, || storage.deduct_gas(100))
            .unwrap();
    });
    assert!(p.ctx.journal().logs().is_empty());
    assert!(!p.subcall_out_of_gas);
}

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
            genesis_timestamp: 0,

            abort_bridge: Default::default(),

            scoped_mutation_entitlements: MutationEntitlements::NONE,
        },
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
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
        let output = outbe_evm::sub_call::run_with_host(
            &mut ctx,
            outbe_evm::sub_call::SubCallEnvironment {
                self_address: CALLER,
                outer_is_static: false,
                spec: SpecId::PRAGUE,
            },
            SubCallInput {
                target: address,
                ..input(50_000)
            },
            std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
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
        let output = outbe_evm::precompiles::map_outbe_precompile_result(Err(error.into()), 10);
        assert!(
            output.is_err(),
            "DB failure must not produce a user receipt"
        );
    }
}

#[test]
fn trigger_budget_caught_real_database_failure_stays_fatal() {
    use outbe_primitives::storage::StorageHandle;
    for at_entry in [false, true] {
        let mut ctx = Context::mainnet().with_db(FailedDatabase { at_entry });
        let mut p = provider(&mut ctx, 50_000);
        let result = StorageHandle::enter(&mut p, |storage| {
            storage.with_trigger_budget(20_000, || {
                assert!(storage
                    .try_call_with_gas(TARGET, U256::ZERO, Bytes::new(), 10_000)
                    .is_err());
                let _ = storage.deduct_gas(20_001);
                Ok(())
            })
        });
        assert!(matches!(result, Err(PrecompileError::Fatal(_))));
        assert!(!p.subcall_out_of_gas);
        assert!(p.trigger_budget.is_none());
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
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 1,
                timestamp: 1,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 1,
                proposer: CALLER,
                validators: vec![CALLER],
            },
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
        let expected = if code == GAS_WORD {
            outbe_l2registry::precompile::IL2Registry::InvalidPublicKey {}.abi_encode()
        } else {
            outbe_l2registry::precompile::IL2Registry::InboxKeyCallFailed {}.abi_encode()
        };
        assert!(
            matches!(result, Err(PrecompileError::RevertBytes(bytes)) if bytes.as_ref() == expected)
        );
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
    let result = dispatch(
        StorageHandle::new(&mut p),
        b"not a system command",
        SYSTEM_ADDRESS,
        U256::ZERO,
    );
    assert!(matches!(result, Err(PrecompileError::WriteProtection)));
}

fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> outbe_primitives::error::Result<alloy_primitives::Bytes> {
    outbe_runtime::OutbeRuntime::new().dispatch_system_tx(
        storage,
        data,
        caller,
        value,
        &outbe_tee::tee_attestation_activation::TeeAttestationChainSpecStateV1::Unbound,
    )
}
