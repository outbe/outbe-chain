//! Tail workers roll back recoverable failures without masking corruption.

use super::*;
use crate::runtime::{handle_cycle_tick_with_tasks, run_isolated_cycle_worker};
use crate::tasks::CycleWorkerId;
use crate::ICycle;
use outbe_desis::DesisContract;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_system_common::{addresses::CYCLE_ADDRESS, time::EmissionDate};

const RECIPIENT: Address = Address::repeat_byte(0x93);
const CLEANUP_DAY: EmissionDate = EmissionDate::new(20_231_113);

fn cycle_logs(storage: &HashMapStorageProvider) -> usize {
    storage.get_events(CYCLE_ADDRESS).len()
}

fn write_worker_page(ctx: &BlockRuntimeContext) -> Result<()> {
    ctx.storage
        .contract::<DesisContract>()
        .next_auction_day
        .write(CLEANUP_DAY.value())?;
    ctx.storage.increase_balance(RECIPIENT, U256::ONE)?;
    ctx.storage
        .contract::<Cycle>()
        .emit(ICycle::CycleTaskCompleted {
            task: ICycle::CycleTask::ScheduleNodCallPass,
        })
}

#[test]
fn tail_worker_revert_budget_exceeded_and_panic_roll_back_the_page_and_preserve_cursor_for_retry() {
    let mut measured = cycle_storage();
    let mut page_cost = 0;
    measured.enter(|handle| {
        let ctx = runtime_context(block_ctx(1, GENESIS_TS), handle);
        let before = ctx.storage.gas_used().unwrap();
        ctx.storage
            .with_trigger_budget(100_000, || write_worker_page(&ctx))
            .unwrap();
        page_cost = ctx.storage.gas_used().unwrap() - before;
    });
    assert!(page_cost > 1);
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(1, GENESIS_TS), handle);
        for failure in 0..6 {
            let budget = if failure == 1 { page_cost - 1 } else { 100_000 };
            run_isolated_cycle_worker(&ctx, CycleWorkerId::DesisCleanup, budget, |ctx| {
                write_worker_page(ctx)?;
                match failure {
                    0 => Err(outbe_desis::DesisError::InvalidTotalPages.into()),
                    1 => Ok(()),
                    2 => panic!("worker panic"),
                    3 => Err(PrecompileError::Revert("worker revert".into())),
                    4 => Err(PrecompileError::WriteProtection),
                    _ => Err(outbe_primitives::storage::SubCallError::InvalidTarget.into()),
                }
            })
            .unwrap();
            assert_eq!(ctx.storage.balance(RECIPIENT).unwrap(), U256::ZERO);
            assert_eq!(
                ctx.storage
                    .contract::<DesisContract>()
                    .next_auction_day
                    .read()
                    .unwrap(),
                0
            );
        }
    });
    assert_eq!(cycle_logs(&storage), 0);
}

#[test]
fn corrupt_cleanup_state_stays_fatal_and_rejects_cycle_tick() {
    let mut storage = cycle_storage();
    storage.set_block_number(2);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 60), handle);
        ctx.storage
            .contract::<DesisContract>()
            .cleanup_tail
            .write(CLEANUP_DAY.value())
            .unwrap();
        assert!(matches!(
            handle_cycle_tick_with_tasks(&ctx, 1, &[]),
            Err(PrecompileError::Fatal(_))
        ));
    });
    assert_eq!(cycle_logs(&storage), 0);
}

#[test]
fn fatal_node_local_and_enclosing_out_of_gas_workers_are_not_recoverable() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(1, GENESIS_TS), handle);
        for error in [
            PrecompileError::Fatal("fatal".into()),
            PrecompileError::FatalWithCause(Box::new(PrecompileError::revert("cause"))),
            PrecompileError::OutOfGas,
            PrecompileError::Storage("backend".into()),
            PrecompileError::BodyReadCorruption("body".into()),
            PrecompileError::EnclaveUnavailable("unavailable".into()),
            PrecompileError::SubCall(outbe_primitives::storage::SubCallError::NotAvailable),
            PrecompileError::Unsupported,
        ] {
            let expected = std::mem::discriminant(&error);
            let actual =
                run_isolated_cycle_worker(&ctx, CycleWorkerId::DesisCleanup, 100_000, |ctx| {
                    write_worker_page(ctx)?;
                    Err(error)
                })
                .unwrap_err();
            assert_eq!(std::mem::discriminant(&actual), expected);
            assert_eq!(ctx.storage.balance(RECIPIENT).unwrap(), U256::ZERO);
        }
    });
    assert_eq!(cycle_logs(&storage), 0);
}
