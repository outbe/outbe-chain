//! Explicit test budgets exercise the gated scheduler without production constants.

use super::*;
use crate::{
    runtime::{dispatch_triggers_with_specs, handle_cycle_tick_with_triggers},
    triggers::{TriggerHandler, TriggerSpec},
    ICycle,
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
    TriggerSpec::new(id, "test", (60, 0), budget, TriggerHandler::Test(handler)).unwrap()
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

fn dispatch(ctx: &BlockRuntimeContext, specs: &[TriggerSpec]) -> Result<()> {
    dispatch_triggers_with_specs(ctx, specs)
}

#[test]
fn over_budget_rolls_back_and_propagates_pending_then_retries_once_next_block() {
    let mut storage = storage(&[0, 1, 2, 4]);
    let mut advance = spec(2, 100, independent);
    advance.depends_on = &[TriggerId::ProtocolCycle];
    let mut clearing = spec(4, 100, independent);
    clearing.depends_on = &[TriggerId::AdvanceAuctions];
    let mut specs = [
        spec(0, 50, write_then_spend),
        spec(1, 100, independent),
        advance,
        clearing,
    ];
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
        dispatch(&ctx, &specs).unwrap();
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
    assert!(deferred
        .iter()
        .all(|event| event.reason == 2 && event.scheduledAt == GENESIS_TS + 60));

    specs[0] = spec(0, 140, write_then_spend);
    storage.set_block_number(3);
    storage.set_gas_limit(10_000_000);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(3, GENESIS_TS + 62), handle);
        dispatch(&ctx, &specs).unwrap();
        dispatch(&ctx, &specs).unwrap();
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
fn ordinary_reverts_consume_dependencies_but_fatal_errors_stop_the_walk() {
    let mut storage = storage(&[0, 1, 2]);
    let mut dependent = spec(2, 100, independent);
    dependent.depends_on = &[TriggerId::ProtocolCycle];
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
        dispatch(&ctx, &[spec(0, 100, ordinary_revert), dependent]).unwrap();
        assert_eq!(ctx.storage.balance(RECIPIENT).unwrap(), U256::ZERO);
        assert_eq!(ctx.storage.balance(INDEPENDENT).unwrap(), U256::ZERO);
        let cycle = ctx.storage.contract::<Cycle>();
        for id in [0, 2] {
            assert_eq!(cycle.last_executed_block_number.read(&id).unwrap(), 2);
        }
        let error = dispatch(&ctx, &[spec(1, 100, fatal)]).unwrap_err();
        assert!(matches!(error, PrecompileError::Fatal(message) if message == "backend"));
        assert_eq!(cycle.last_executed_at.read(&1).unwrap(), GENESIS_TS + 1);
    });
}

#[test]
fn rejects_zero_budget() {
    assert!(matches!(
        TriggerSpec::new(0, "test", (60, 0), 0, TriggerHandler::Test(independent)),
        Err(crate::errors::CycleError::ZeroTriggerWorkBudget)
    ));
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
        assert!(is_day_allocated(&ctx, 20_240_101));
    });
    assert!(complete_gas > 1);
    let protocol = |budget| {
        TriggerSpec::new(
            0,
            "protocol",
            (3_600, 0),
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
        dispatch(&ctx, &[protocol(complete_gas - 1)]).unwrap();
        assert_eq!(cycle.last_executed_at.read(&0).unwrap(), timestamp_before);
        assert_eq!(
            cycle.last_executed_block_number.read(&0).unwrap(),
            height_before
        );
        assert_eq!(active_utc_day(&ctx), 20_240_101);
        assert!(!is_day_allocated(&ctx, 20_240_101));
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
    assert!(storage
        .get_events(CYCLE_ADDRESS)
        .iter()
        .all(|log| ICycle::DayEmissionLimitAllocationFailed::decode_log_data(log).is_err()));

    storage.set_block_number(3);
    storage.set_gas_limit(100_000_000);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(3, fire_ts + 1), handle);
        account_parent(&ctx, 3);
        let triggers = [protocol(50_000_000)];
        dispatch(&ctx, &triggers).unwrap();
        assert!(is_day_allocated(&ctx, 20_240_101));
        assert!(!is_day_allocated(&ctx, 20_240_102));
        assert_eq!(active_utc_day(&ctx), 20_240_102);
        let settled =
            outbe_emission::api::get_emission_day_record(ctx.storage.clone(), 20_240_101).unwrap();
        dispatch(&ctx, &triggers).unwrap();
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
        (SECONDS_PER_DAY, 0),
        100,
        TriggerHandler::Test(fatal),
    )
    .unwrap();
    let mut dependent = spec(2, 100, independent);
    dependent.depends_on = &[TriggerId::ProtocolCycle];
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
        dispatch(&ctx, &[prerequisite, dependent]).unwrap();
        assert_eq!(ctx.storage.balance(INDEPENDENT).unwrap(), U256::ONE);
    });
    let mut unsupported = HashMapStorageProvider::new(CHAIN_ID);
    unsupported.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + 61), handle);
        assert!(dispatch(&ctx, &[]).is_err());
    });
}

#[test]
fn bootstrap_without_triggers_creates_genesis_worldwide_day() {
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
        handle_cycle_tick_with_triggers(&ctx, 1, &[]).unwrap();
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
fn canonical_body_observers_keep_every_scoped_handler_sample_without_prefix() {
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
            vec![0, 1, 2, 3, 4, 5, 6]
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
