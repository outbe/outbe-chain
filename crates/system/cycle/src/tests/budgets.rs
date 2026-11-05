//! Explicit test budgets exercise the gated scheduler without production constants.

use super::*;
use crate::{
    ICycle,
    runtime::{CycleWorkCapacity, dispatch_triggers_with_budgets, handle_cycle_tick_with_budgets},
    triggers::{TriggerHandler, TriggerSpec},
};
use alloy_sol_types::SolEvent;
use outbe_primitives::{
    addresses::CYCLE_ADDRESS,
    error::{PrecompileError, Result},
};

const RECIPIENT: Address = Address::repeat_byte(0x91);
const INDEPENDENT: Address = Address::repeat_byte(0x92);

fn write_then_spend(ctx: &BlockRuntimeContext) -> Result<()> {
    ctx.storage.increase_balance(RECIPIENT, U256::from(7))?;
    ctx.storage.deduct_gas(40)?;
    ctx.storage.deduct_gas(100)
}

fn independent(ctx: &BlockRuntimeContext) -> Result<()> {
    ctx.storage.increase_balance(INDEPENDENT, U256::ONE)?;
    ctx.storage.deduct_gas(100)
}

fn ordinary_revert(ctx: &BlockRuntimeContext) -> Result<()> {
    ctx.storage.increase_balance(RECIPIENT, U256::ONE)?;
    Err(PrecompileError::Revert("ordinary".into()))
}

fn fatal(_: &BlockRuntimeContext) -> Result<()> {
    Err(PrecompileError::Fatal("backend".into()))
}

fn spec(id: u32, budget: u64, handler: fn(&BlockRuntimeContext<'_>) -> Result<()>) -> TriggerSpec {
    TriggerSpec::new(id, "test", 60, 0, budget, TriggerHandler::Test(handler)).unwrap()
}

fn storage(ids: &[u32]) -> HashMapStorageProvider {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let cycle = handle.contract::<Cycle>();
        for id in ids {
            cycle.last_executed_at.write(id, GENESIS_TS + 1).unwrap();
            cycle.last_executed_block_number.write(id, 1).unwrap();
        }
    });
    storage.set_block_number(2);
    storage.enable_production_storage_gas_metering();
    storage.set_gas_limit(10_000_000);
    storage
}

fn dispatch(ctx: &BlockRuntimeContext, specs: &[TriggerSpec], remaining_gas: u64) -> Result<()> {
    dispatch_triggers_with_budgets(
        ctx,
        specs,
        CycleWorkCapacity {
            remaining_gas,
            gas_used_at_entry: ctx.storage.gas_used()?,
            reserve_after_cycle: 10_000,
        },
        500_000,
    )
}

#[test]
fn over_budget_rolls_back_and_propagates_pending_then_retries_once_next_block() {
    let mut storage = storage(&[0, 1, 2, 4]);
    let mut advance = spec(2, 100, independent);
    advance.depends_on = &[TriggerId::ProtocolCycle];
    let mut clearing = spec(4, 100, independent);
    clearing.depends_on = &[TriggerId::AuctionAdvance];
    let mut specs = [
        spec(0, 50, write_then_spend),
        spec(1, 100, independent),
        advance,
        clearing,
    ];
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
        dispatch(&ctx, &specs, 9_000_000).unwrap();
        assert_eq!(ctx.storage.balance(RECIPIENT).unwrap(), U256::ZERO);
        assert_eq!(ctx.storage.balance(INDEPENDENT).unwrap(), U256::ONE);
        let cycle = ctx.storage.contract::<Cycle>();
        for id in [0, 2, 4] {
            assert_eq!(cycle.last_executed_at.read(&id).unwrap(), GENESIS_TS + 1);
            assert_eq!(cycle.last_executed_block_number.read(&id).unwrap(), 1);
        }
        assert_eq!(cycle.last_executed_block_number.read(&1).unwrap(), 2);
    });
    let logs = storage.get_events(CYCLE_ADDRESS);
    let over: Vec<_> = logs
        .iter()
        .filter_map(|log| ICycle::CycleTriggerOverBudget::decode_log_data(log).ok())
        .collect();
    assert_eq!(over.len(), 1);
    assert_eq!(
        (over[0].id, over[0].consumedGas, over[0].budgetGas),
        (0, 40, 50)
    );
    assert_eq!(
        logs.iter()
            .filter(|log| ICycle::CycleTriggerFailed::decode_log_data(log).is_ok())
            .count(),
        0
    );
    assert_eq!(
        logs.iter()
            .filter(|log| ICycle::CycleTriggerExecuted::decode_log_data(log).is_ok())
            .count(),
        1
    );
    let deferred: Vec<_> = logs
        .iter()
        .filter_map(|log| ICycle::CycleTriggerDeferred::decode_log_data(log).ok())
        .collect();
    assert_eq!(deferred.len(), 2);
    assert!(
        deferred
            .iter()
            .all(|event| event.reason == 2 && event.scheduledAt == GENESIS_TS + 60)
    );

    specs[0] = spec(0, 140, write_then_spend);
    storage.set_block_number(3);
    storage.set_gas_limit(10_000_000);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(3, GENESIS_TS + 62), handle);
        dispatch(&ctx, &specs, 9_000_000).unwrap();
        dispatch(&ctx, &specs, 9_000_000).unwrap();
        assert_eq!(ctx.storage.balance(RECIPIENT).unwrap(), U256::from(7));
        assert_eq!(ctx.storage.balance(INDEPENDENT).unwrap(), U256::from(3));
        let cycle = ctx.storage.contract::<Cycle>();
        for id in [0, 2, 4] {
            assert_eq!(cycle.last_executed_at.read(&id).unwrap(), GENESIS_TS + 60);
            assert_eq!(cycle.last_executed_block_number.read(&id).unwrap(), 3);
        }
    });
}

#[test]
fn capacity_deferred_work_is_not_attempted_and_independent_small_work_fits() {
    let mut storage = storage(&[0, 1]);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
        dispatch(
            &ctx,
            &[
                spec(0, 1_000_000, write_then_spend),
                spec(1, 100, independent),
            ],
            600_000,
        )
        .unwrap();
        assert_eq!(ctx.storage.balance(RECIPIENT).unwrap(), U256::ZERO);
        assert_eq!(ctx.storage.balance(INDEPENDENT).unwrap(), U256::ONE);
        assert_eq!(
            ctx.storage
                .contract::<Cycle>()
                .last_executed_at
                .read(&0)
                .unwrap(),
            GENESIS_TS + 1
        );
    });
    let logs = storage.get_events(CYCLE_ADDRESS);
    let deferred: Vec<_> = logs
        .iter()
        .filter_map(|log| ICycle::CycleTriggerDeferred::decode_log_data(log).ok())
        .collect();
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].reason, 1);
    assert!(
        logs.iter()
            .all(|log| ICycle::CycleTriggerOverBudget::decode_log_data(log).is_err())
    );
}

#[test]
fn ordinary_reverts_consume_dependencies_but_fatal_errors_stop_the_walk() {
    let mut storage = storage(&[0, 1, 2]);
    let mut dependent = spec(2, 100, independent);
    dependent.depends_on = &[TriggerId::ProtocolCycle];
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
        dispatch(&ctx, &[spec(0, 100, ordinary_revert), dependent], 9_000_000).unwrap();
        assert_eq!(ctx.storage.balance(RECIPIENT).unwrap(), U256::ZERO);
        assert_eq!(ctx.storage.balance(INDEPENDENT).unwrap(), U256::ZERO);
        let cycle = ctx.storage.contract::<Cycle>();
        for id in [0, 2] {
            assert_eq!(cycle.last_executed_block_number.read(&id).unwrap(), 2);
        }
        let error = dispatch(&ctx, &[spec(1, 100, fatal)], 9_000_000).unwrap_err();
        assert!(matches!(error, PrecompileError::Fatal(message) if message == "backend"));
        assert_eq!(cycle.last_executed_at.read(&1).unwrap(), GENESIS_TS + 1);
    });
}

#[test]
fn rejects_zero_budget_and_insufficient_essential_capacity() {
    assert!(matches!(
        TriggerSpec::new(0, "test", 60, 0, 0, TriggerHandler::Test(independent)),
        Err(crate::errors::CycleError::ZeroTriggerWorkBudget)
    ));
    let mut storage = storage(&[]);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
        assert!(matches!(
            dispatch(&ctx, &[], 1),
            Err(PrecompileError::Fatal(_))
        ));
    });
}

fn calendar_storage() -> HashMapStorageProvider {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 32);
    storage.enter(|handle| {
        let ctx = genesis_block(handle, GENESIS_TS + 60);
        seed_fresh_reward_oracle(&ctx);
        run_cycle_tick(&ctx).unwrap();
    });
    storage.set_block_number(2);
    storage.enable_production_storage_gas_metering();
    storage.set_gas_limit(100_000_000);
    storage
}

#[test]
fn late_protocol_exhaustion_rolls_back_calendar_and_allocation_then_retries_across_midnight() {
    // Measure this exact fixture's complete successful handler, then reject its
    // final metered operation. These are test budgets, not production constants.
    let fire_ts = GENESIS_TS + 2 * SECONDS_PER_DAY - 1;
    let mut measured = calendar_storage();
    let mut complete_gas = 0;
    measured.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, fire_ts), handle);
        account_parent(&ctx, 2);
        let before = ctx.storage.gas_used().unwrap();
        ctx.storage
            .with_trigger_budget(50_000_000, || crate::handler::run_protocol_cycle(&ctx))
            .unwrap();
        complete_gas = ctx.storage.gas_used().unwrap() - before;
        assert!(is_day_settled(&ctx, 20_240_101));
    });
    assert!(complete_gas > 1);
    let protocol = |budget| {
        TriggerSpec::new(
            0,
            "protocol",
            3_600,
            0,
            budget,
            TriggerHandler::ProtocolCycle,
        )
        .unwrap()
    };
    let mut storage = calendar_storage();
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, fire_ts), handle);
        account_parent(&ctx, 2);
        let promis_before = outbe_promislimit::PromisLimitContract::new(ctx.storage.clone())
            .promis_limit()
            .unwrap();
        let world_before =
            outbe_metadosis::api::worldwide_day(ctx.storage.clone(), 20_240_102u32.into()).unwrap();
        let cycle = ctx.storage.contract::<Cycle>();
        let timestamp_before = cycle.last_executed_at.read(&0).unwrap();
        let height_before = cycle.last_executed_block_number.read(&0).unwrap();
        dispatch(&ctx, &[protocol(complete_gas - 1)], 90_000_000).unwrap();
        assert_eq!(cycle.last_executed_at.read(&0).unwrap(), timestamp_before);
        assert_eq!(
            cycle.last_executed_block_number.read(&0).unwrap(),
            height_before
        );
        assert_eq!(active_utc_day(&ctx), 20_240_101);
        assert!(!is_day_settled(&ctx, 20_240_101));
        assert!(
            outbe_emission::api::get_emission_day_record(ctx.storage.clone(), 20_240_101)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            outbe_metadosis::api::worldwide_day(ctx.storage.clone(), 20_240_102u32.into()).unwrap(),
            world_before
        );
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(ctx.storage.clone())
                .promis_limit()
                .unwrap(),
            promis_before
        );
    });
    assert_eq!(
        storage
            .get_events(CYCLE_ADDRESS)
            .iter()
            .filter(|log| ICycle::CycleTriggerOverBudget::decode_log_data(log).is_ok())
            .count(),
        1
    );
    assert!(
        storage
            .get_events(CYCLE_ADDRESS)
            .iter()
            .all(|log| ICycle::DayEmissionLimitAllocationFailed::decode_log_data(log).is_err())
    );

    storage.set_block_number(3);
    storage.set_gas_limit(100_000_000);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(3, fire_ts + 1), handle);
        account_parent(&ctx, 3);
        let triggers = [protocol(50_000_000)];
        dispatch(&ctx, &triggers, 90_000_000).unwrap();
        assert!(is_day_settled(&ctx, 20_240_101));
        assert!(!is_day_settled(&ctx, 20_240_102));
        assert_eq!(active_utc_day(&ctx), 20_240_102);
        let settled =
            outbe_emission::api::get_emission_day_record(ctx.storage.clone(), 20_240_101).unwrap();
        dispatch(&ctx, &triggers, 90_000_000).unwrap();
        assert_eq!(
            outbe_emission::api::get_emission_day_record(ctx.storage.clone(), 20_240_101).unwrap(),
            settled
        );
    });
}

#[test]
fn non_due_prerequisites_do_not_block_and_unmetered_provider_cannot_run_scopes() {
    let mut storage = storage(&[0, 2]);
    let prerequisite = TriggerSpec::new(
        0,
        "not_due",
        SECONDS_PER_DAY,
        0,
        100,
        TriggerHandler::Test(fatal),
    )
    .unwrap();
    let mut dependent = spec(2, 100, independent);
    dependent.depends_on = &[TriggerId::ProtocolCycle];
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
        dispatch(&ctx, &[prerequisite, dependent], 9_000_000).unwrap();
        assert_eq!(ctx.storage.balance(INDEPENDENT).unwrap(), U256::ONE);
    });
    let mut unsupported = HashMapStorageProvider::new(CHAIN_ID);
    unsupported.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
        assert!(dispatch(&ctx, &[], 9_000_000).is_err());
    });
}

fn successful_dispatch_overhead() -> u64 {
    let mut storage = storage(&[1]);
    let mut overhead = 0;
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
        let before = ctx.storage.gas_used().unwrap();
        dispatch(&ctx, &[spec(1, 100, independent)], 9_000_000).unwrap();
        overhead = ctx.storage.gas_used().unwrap() - before - 100;
    });
    assert!(overhead > 0);
    overhead
}

#[test]
fn external_baseline_admits_exact_allowance_and_defers_one_gas_short_on_either_limit() {
    let dispatcher_overhead = successful_dispatch_overhead();
    let prefix = 7_000;
    let after_baseline = 71;
    let overhead = dispatcher_overhead + after_baseline;
    let later_reserve = 3_000;
    for aggregate_limited in [true, false] {
        for shortfall in [0, 1] {
            let mut storage = storage(&[1]);
            if !aggregate_limited {
                storage.set_gas_limit(prefix + overhead + 100 - shortfall);
            }
            storage.enter(|handle| {
                let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
                ctx.storage.deduct_gas(prefix).unwrap();
                let baseline = ctx.storage.gas_used().unwrap();
                // The caller captures capacity before this piece of Cycle setup.
                // Prefix work is already excluded from its aggregate allowance.
                ctx.storage.deduct_gas(after_baseline).unwrap();
                let remaining_gas = if aggregate_limited {
                    overhead + 100 + later_reserve - shortfall
                } else {
                    9_000_000
                };
                dispatch_triggers_with_budgets(
                    &ctx,
                    &[spec(1, 100, independent)],
                    CycleWorkCapacity {
                        remaining_gas,
                        gas_used_at_entry: baseline,
                        reserve_after_cycle: later_reserve,
                    },
                    overhead,
                )
                .unwrap();
                if shortfall == 0 {
                    assert_eq!(ctx.storage.gas_used().unwrap() - baseline, overhead + 100);
                }
            });
            let logs = storage.get_events(CYCLE_ADDRESS);
            let executed = logs
                .iter()
                .filter(|log| ICycle::CycleTriggerExecuted::decode_log_data(log).is_ok())
                .count();
            let deferred: Vec<_> = logs
                .iter()
                .filter_map(|log| ICycle::CycleTriggerDeferred::decode_log_data(log).ok())
                .collect();
            assert_eq!(
                executed,
                usize::from(shortfall == 0),
                "aggregate_limited={aggregate_limited}, shortfall={shortfall}"
            );
            assert_eq!(deferred.len(), usize::from(shortfall != 0));
            if let Some(event) = deferred.first() {
                assert_eq!(
                    (event.id, event.reason, event.scheduledAt),
                    (1, 1, GENESIS_TS + 60)
                );
            }
            storage.set_gas_limit(10_000_000);
            storage.enter(|handle| {
                assert_eq!(
                    handle.balance(INDEPENDENT).unwrap(),
                    U256::from(u64::from(shortfall == 0))
                );
                let cycle = handle.contract::<Cycle>();
                assert_eq!(
                    cycle.last_executed_block_number.read(&1).unwrap(),
                    if shortfall == 0 { 2 } else { 1 }
                );
            });
        }
    }
}

#[test]
fn dispatcher_rejects_actual_overhead_one_gas_above_declared_reserve() {
    let overhead = successful_dispatch_overhead();
    let mut storage = storage(&[1]);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
        let capacity = CycleWorkCapacity {
            remaining_gas: 9_000_000,
            gas_used_at_entry: ctx.storage.gas_used().unwrap(),
            reserve_after_cycle: 3_000,
        };
        // The enclosing transaction owns rollback of mandatory bookkeeping failures.
        let result = ctx.storage.with_checkpoint(|| {
            dispatch_triggers_with_budgets(
                &ctx,
                &[spec(1, 100, independent)],
                capacity,
                overhead - 1,
            )
        });
        assert!(matches!(result, Err(PrecompileError::Fatal(_))));
        assert_eq!(ctx.storage.balance(INDEPENDENT).unwrap(), U256::ZERO);
        let cycle = ctx.storage.contract::<Cycle>();
        assert_eq!(cycle.last_executed_at.read(&1).unwrap(), GENESIS_TS + 1);
        assert_eq!(cycle.last_executed_block_number.read(&1).unwrap(), 1);
    });
    assert!(storage.get_events(CYCLE_ADDRESS).is_empty());
}

fn queued_reward_storage() -> HashMapStorageProvider {
    use outbe_agentreward::api::{DayAgentPools, allocate_day_agent_rewards};
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, ANCHOR_TS + 10), handle.clone());
        let day = outbe_primitives::time::WorldwideDay::new(20_231_231);
        outbe_agentreward::AgentRewardContract::new(handle.clone())
            .increment_waa_tribute(day, RECIPIENT)
            .unwrap();
        outbe_rewards::api::close_day_participation(&ctx, day.value()).unwrap();
        allocate_day_agent_rewards(
            &ctx,
            day,
            DayAgentPools {
                waa: U256::from(1_000),
                ..Default::default()
            },
        )
        .unwrap();
        outbe_emission::api::record_day_allocation(
            handle,
            day.value(),
            outbe_emission::api::DayAllocationAmounts {
                emission_limit: U256::from(1_000),
                waa: U256::from(1_000),
                metadosis_limit: U256::ZERO,
                validators: U256::ZERO,
                sra: U256::ZERO,
                cca: U256::ZERO,
                promis_limit: U256::ZERO,
            },
        )
        .unwrap();
    });
    storage.set_block_number(2);
    storage.enable_production_storage_gas_metering();
    storage.set_gas_limit(10_000_000);
    storage
}

#[test]
fn budgeted_body_without_triggers_credits_rewards_at_exact_overhead_and_rejects_underfunding() {
    let mut measured = queued_reward_storage();
    let mut body_cost = 0;
    measured.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, ANCHOR_TS + 10), handle);
        let before = ctx.storage.gas_used().unwrap();
        handle_cycle_tick_with_budgets(
            &ctx,
            1,
            &[],
            CycleWorkCapacity {
                remaining_gas: 9_000_000,
                gas_used_at_entry: before,
                reserve_after_cycle: 3_000,
            },
            8_000_000,
        )
        .unwrap();
        body_cost = ctx.storage.gas_used().unwrap() - before;
    });
    assert!(body_cost > 0);
    // Exact measured funding succeeds. A one-gas underestimate fails after real
    // reward work; initial aggregate/enclosing shortages fail before the work.
    for case in 0..4 {
        let mut storage = queued_reward_storage();
        if case == 3 {
            storage.set_gas_limit(body_cost - 1);
        }
        storage.enter(|handle| {
            let ctx = runtime_context(block_ctx(2, ANCHOR_TS + 10), handle);
            let before = ctx.storage.gas_used().unwrap();
            let overhead = if case == 1 { body_cost - 1 } else { body_cost };
            let capacity = CycleWorkCapacity {
                remaining_gas: body_cost + 3_000 - u64::from(case == 2),
                gas_used_at_entry: before,
                reserve_after_cycle: 3_000,
            };
            let result = ctx.storage.with_checkpoint(|| {
                handle_cycle_tick_with_budgets(&ctx, 1, &[], capacity, overhead)
            });
            let spent = ctx.storage.gas_used().unwrap() - before;
            if case == 0 {
                result.unwrap();
                assert_eq!(spent, body_cost);
            } else {
                assert!(matches!(result, Err(PrecompileError::Fatal(_))));
                assert_eq!(spent, if case == 1 { body_cost } else { 0 });
            }
        });
        storage.set_gas_limit(10_000_000);
        storage.enter(|handle| {
            let reward = outbe_agentreward::AgentRewardContract::new(handle)
                .get_pool_claimable_reward(outbe_agentreward::schema::RewardPool::Waa, RECIPIENT)
                .unwrap();
            assert_eq!(
                reward,
                if case == 0 {
                    U256::from(1_000)
                } else {
                    U256::ZERO
                }
            );
        });
        assert!(storage.get_events(CYCLE_ADDRESS).is_empty());
    }
}

#[test]
fn budgeted_bootstrap_without_triggers_creates_genesis_worldwide_day() {
    let mut storage = cycle_storage();
    storage.enable_production_storage_gas_metering();
    storage.set_gas_limit(10_000_000);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(1, GENESIS_TS + 60), handle);
        assert!(
            outbe_metadosis::api::worldwide_day(ctx.storage.clone(), 20_240_101u32.into())
                .unwrap()
                .is_none()
        );
        let before = ctx.storage.gas_used().unwrap();
        handle_cycle_tick_with_budgets(
            &ctx,
            1,
            &[],
            CycleWorkCapacity {
                remaining_gas: 9_000_000,
                gas_used_at_entry: before,
                reserve_after_cycle: 3_000,
            },
            8_000_000,
        )
        .unwrap();
        assert!(ctx.storage.gas_used().unwrap() > before);
        assert!(
            outbe_metadosis::api::worldwide_day(ctx.storage.clone(), 20_240_101u32.into())
                .unwrap()
                .is_some()
        );
    });
    assert!(storage.get_events(CYCLE_ADDRESS).is_empty());
}

#[cfg(feature = "test-utils")]
#[test]
fn canonical_body_observers_keep_all_eight_scoped_handler_samples_without_prefix() {
    let mut storage = calendar_storage();
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        account_parent(&ctx, 2);
        crate::runtime::take_trigger_gas_samples();
        crate::runtime::take_cycle_gas_samples();
        ctx.storage.deduct_gas(7_000).unwrap();
        let before = ctx.storage.gas_used().unwrap();
        run_cycle_tick(&ctx).unwrap();
        let body_gas = ctx.storage.gas_used().unwrap() - before;
        let handlers = crate::runtime::take_trigger_gas_samples();
        assert_eq!(
            handlers.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5, 6, 8]
        );
        assert!(handlers.iter().all(|(_, gas)| *gas > 0));
        let handler_gas = handlers
            .iter()
            .try_fold(0u64, |sum, (_, gas)| sum.checked_add(*gas))
            .unwrap();
        assert_eq!(
            crate::runtime::take_cycle_gas_samples(),
            vec![(body_gas, handler_gas)]
        );
        assert!(body_gas > handler_gas);
    });
}
