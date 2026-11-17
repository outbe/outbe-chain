//! Tail workers roll back recoverable failures without masking corruption.

use super::*;
use crate::runtime::{handle_cycle_tick_with_tasks, run_isolated_cycle_worker};
use crate::tasks::CycleWorkerId;
use crate::ICycle;
use alloy_sol_types::SolEvent;
use outbe_agentreward::schema::{AgentRewardContract, RewardPool};
use outbe_desis::{AuctionStage, DesisContract};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_system_common::{
    addresses::CYCLE_ADDRESS,
    time::{EmissionDate, WorldwideDay},
};

const RECIPIENT: Address = Address::repeat_byte(0x93);
const DAY: WorldwideDay = WorldwideDay::new(20_240_101);
const CLEANUP_DAY: EmissionDate = EmissionDate::new(20_231_113);

fn worker_failures(storage: &HashMapStorageProvider) -> Vec<(u8, u64)> {
    storage
        .get_events(CYCLE_ADDRESS)
        .iter()
        .filter_map(|log| ICycle::CycleWorkerFailed::decode_log_data(log).ok())
        .map(|event| (event.workerId, event.blockNumber))
        .collect()
}

fn seed_jobs(ctx: &BlockRuntimeContext) {
    let mut rewards = ctx.storage.contract::<AgentRewardContract>();
    rewards.increment_waa_tribute(DAY, RECIPIENT).unwrap();
    outbe_rewards::api::seal_votes_and_fees(ctx, DAY.value()).unwrap();
    let allocation = outbe_agentreward::api::allocate_day_agent_rewards(
        ctx,
        DAY,
        outbe_agentreward::api::DayAgentPools {
            waa: U256::from(10),
            ..Default::default()
        },
    )
    .unwrap();
    outbe_emission::api::record_day_allocation(
        ctx.storage.clone(),
        DAY.value(),
        outbe_emission::api::DayAllocationAmounts {
            emission_limit: U256::from(10),
            metadosis_limit: allocation.total_excess,
            validators: allocation.validators_allocated,
            waa: allocation.waa_allocated,
            sra: allocation.sra_allocated,
            cca: allocation.cca_allocated,
            promis_limit: U256::ZERO,
        },
    )
    .unwrap();
    let desis = ctx.storage.contract::<DesisContract>();
    desis
        .auction_stage
        .write(&CLEANUP_DAY, AuctionStage::Completed as u8)
        .unwrap();
    desis.queue_intake_cleanup(CLEANUP_DAY).unwrap();
}

#[test]
fn tail_worker_missing_job_reverts_and_cleanup_continues_then_next_block_retries() {
    let mut storage = cycle_storage();
    storage.set_block_number(2);
    let mut missing = None;
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY), handle);
        seed_jobs(&ctx);
        let rewards = ctx.storage.contract::<AgentRewardContract>();
        missing = rewards.accrual_jobs.get(1).unwrap();
        rewards.accrual_jobs.delete(1).unwrap();
        handle_cycle_tick_with_tasks(&ctx, 1, &[]).unwrap();
        assert_eq!(rewards.accrual_head.read().unwrap(), 0);
        assert_eq!(rewards.worker_height.read().unwrap(), 0);
        assert!(!rewards.worker_initialized.read().unwrap());
        assert_eq!(rewards.pending_jobs_by_day.read(&DAY.value()).unwrap(), 4);
        assert_eq!(rewards.get_claimable_reward(RECIPIENT).unwrap(), U256::ZERO);
        let desis = ctx.storage.contract::<DesisContract>();
        assert_eq!(desis.cleanup_head.read().unwrap(), 0);
        assert_eq!(desis.cleanup_pending.read(&CLEANUP_DAY).unwrap(), 2);
    });
    assert_eq!(worker_failures(&storage), vec![(1, 2)]);
    storage.set_block_number(3);
    storage.set_gas_limit(100_000_000);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(3, GENESIS_TS + SECONDS_PER_DAY + 1), handle);
        let rewards = ctx.storage.contract::<AgentRewardContract>();
        rewards.accrual_jobs.create(&missing.unwrap()).unwrap();
        handle_cycle_tick_with_tasks(&ctx, 1, &[]).unwrap();
        assert_eq!(rewards.accrual_head.read().unwrap(), 4);
        assert_eq!(rewards.worker_height.read().unwrap(), 3);
        assert_eq!(
            rewards
                .get_pool_claimable_reward(RewardPool::Waa, RECIPIENT)
                .unwrap(),
            U256::from(10)
        );
    });
    assert_eq!(worker_failures(&storage), vec![(1, 2)]);
}

fn write_worker_page(ctx: &BlockRuntimeContext) -> Result<()> {
    let rewards = ctx.storage.contract::<AgentRewardContract>();
    rewards.worker_height.write(ctx.block.block_number)?;
    rewards.accrual_head.write(1)?;
    ctx.storage.increase_balance(RECIPIENT, U256::ONE)?;
    ctx.storage
        .contract::<Cycle>()
        .emit(ICycle::CycleTaskExecuted {
            id: 99,
            scheduledAt: 0,
            blockTimestamp: 0,
            blockNumber: 0,
        })
}

#[test]
fn tail_worker_revert_budget_exceeded_and_panic_emit_cycle_worker_failed_and_preserve_cursor_for_retry(
) {
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
                    0 => Err(
                        outbe_agentreward::errors::AgentRewardError::MissingPreviousDayPrice(840)
                            .into(),
                    ),
                    1 => Ok(()),
                    2 => panic!("worker panic"),
                    3 => Err(PrecompileError::Revert("worker revert".into())),
                    4 => Err(PrecompileError::WriteProtection),
                    _ => Err(outbe_primitives::storage::SubCallError::InvalidTarget.into()),
                }
            })
            .unwrap();
            assert_eq!(ctx.storage.balance(RECIPIENT).unwrap(), U256::ZERO);
            let rewards = ctx.storage.contract::<AgentRewardContract>();
            assert_eq!(rewards.accrual_head.read().unwrap(), 0);
            assert_eq!(rewards.worker_height.read().unwrap(), 0);
        }
    });
    assert_eq!(worker_failures(&storage), vec![(2, 1); 6]);
    assert_eq!(storage.get_events(CYCLE_ADDRESS).len(), 6);
}

#[test]
fn corrupt_cleanup_or_accrual_job_state_stays_fatal_and_rejects_cycle_tick() {
    for corrupt_rewards in [false, true] {
        let mut storage = cycle_storage();
        storage.set_block_number(2);
        storage.enter(|handle| {
            let ctx = runtime_context(block_ctx(2, GENESIS_TS + 60), handle);
            if corrupt_rewards {
                ctx.storage
                    .contract::<AgentRewardContract>()
                    .accrual_head
                    .write(1)
                    .unwrap();
            } else {
                ctx.storage
                    .contract::<DesisContract>()
                    .cleanup_tail
                    .write(CLEANUP_DAY.value())
                    .unwrap();
            }
            assert!(matches!(
                handle_cycle_tick_with_tasks(&ctx, 1, &[]),
                Err(PrecompileError::Fatal(_))
            ));
        });
        assert!(worker_failures(&storage).is_empty());
    }
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
                run_isolated_cycle_worker(&ctx, CycleWorkerId::AgentRewards, 100_000, |ctx| {
                    write_worker_page(ctx)?;
                    Err(error)
                })
                .unwrap_err();
            assert_eq!(std::mem::discriminant(&actual), expected);
            assert_eq!(ctx.storage.balance(RECIPIENT).unwrap(), U256::ZERO);
        }
    });
    assert!(worker_failures(&storage).is_empty());
}

#[test]
fn real_accrual_page_one_gas_short_rolls_back_credits_logs_and_cursors() {
    let mut measured = cycle_storage();
    measured.set_block_number(2);
    let mut page_cost = 0;
    measured.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY), handle);
        seed_jobs(&ctx);
        let before = ctx.storage.gas_used().unwrap();
        ctx.storage
            .with_trigger_budget(10_000_000, || crate::handler::process_agent_rewards(&ctx))
            .unwrap();
        page_cost = ctx.storage.gas_used().unwrap() - before;
    });
    assert!(page_cost > 1);
    let mut storage = cycle_storage();
    storage.set_block_number(2);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY), handle);
        seed_jobs(&ctx);
    });
    let before = storage.events.clone();
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY), handle);
        run_isolated_cycle_worker(
            &ctx,
            CycleWorkerId::AgentRewards,
            page_cost - 1,
            crate::handler::process_agent_rewards,
        )
        .unwrap();
        let rewards = ctx.storage.contract::<AgentRewardContract>();
        assert_eq!(rewards.accrual_head.read().unwrap(), 0);
        assert_eq!(rewards.worker_height.read().unwrap(), 0);
        assert!(!rewards.worker_initialized.read().unwrap());
        assert_eq!(rewards.pending_jobs_by_day.read(&DAY.value()).unwrap(), 4);
        assert_eq!(rewards.get_claimable_reward(RECIPIENT).unwrap(), U256::ZERO);
    });
    assert_eq!(worker_failures(&storage), vec![(1, 2)]);
    let mut without_failure = storage.events.clone();
    without_failure.remove(&CYCLE_ADDRESS);
    assert_eq!(without_failure, before);
}
