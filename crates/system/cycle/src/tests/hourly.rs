//! Hourly protocol-cycle execution of due `EmissionDay` records.

use super::*;
use alloy_sol_types::SolEvent;

#[test]
fn hourly_process_emission_day_executes_due_emission_day_at_metadosis_start() {
    let mut storage = cycle_storage();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 16);
    let (emission_day, fire_at) = storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_tick(&anchor).unwrap();
        let wwd = outbe_system_common::time::WorldwideDay::new(20_240_101);
        let wwd_record = outbe_worldwideday::api::get_worldwide_day(handle.clone(), wwd)
            .unwrap()
            .unwrap();
        let ed = outbe_emission::api::get_emission_day(handle, wwd_record.emission_day)
            .unwrap()
            .unwrap();
        assert_eq!(ed.wwd, wwd);
        assert_eq!(
            ed.metadosis_state,
            outbe_emission::schema::metadosis_state::PENDING
        );
        (
            ed,
            ed.metadosis_start + crate::tasks::PROCESS_EMISSION_DAY_START_OFFSET_SECONDS,
        )
    });

    storage.enter(|handle| {
        seed_task_clock(&handle, fire_at);
        let prev_day =
            outbe_system_common::time::previous_date_key(emission_day.day).expect("date step");
        close_failed_allocation(handle, prev_day);
    });
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 16);
    storage.set_block_number(20);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), 20, fire_at);

        let updated = outbe_emission::api::get_emission_day(ctx.storage.clone(), emission_day.day)
            .unwrap()
            .unwrap();
        assert_eq!(
            updated.metadosis_state,
            outbe_emission::schema::metadosis_state::COMPLETED
        );
        assert_eq!(
            updated.state,
            outbe_emission::schema::emission_day_state::PROCESSING,
            "the record completes when Desis of the day ends"
        );

        let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle
                .last_executed_at
                .read(&TaskId::ProcessEmissionDay.as_u32())
                .unwrap(),
            fire_at
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&TaskId::ProcessEmissionDay.as_u32())
                .unwrap(),
            20
        );
    });
}

pub(super) fn seed_task_clock(handle: &StorageHandle<'_>, fire_at: u64) {
    let previous_hour = fire_at - 3_600;
    let cycle: Cycle<'_> = handle.contract::<Cycle<'_>>();
    for spec in ACTIVE_TASKS {
        let last = if spec.id == TaskId::ProcessEmissionDay.as_u32() {
            previous_hour
        } else {
            fire_at
        };
        cycle.last_executed_at.write(&spec.id, last).unwrap();
    }
}

fn due_emission_day_fixture() -> (HashMapStorageProvider, u32, u64) {
    let mut storage = cycle_storage();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 16);
    let (day, fire_at) = storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_tick(&anchor).unwrap();
        let wwd = outbe_system_common::time::WorldwideDay::new(20_240_101);
        let wwd_record = outbe_worldwideday::api::get_worldwide_day(handle.clone(), wwd)
            .unwrap()
            .unwrap();
        let ed = outbe_emission::api::get_emission_day(handle, wwd_record.emission_day)
            .unwrap()
            .unwrap();
        (ed.day, ed.metadosis_start.div_ceil(3_600) * 3_600)
    });
    storage.enter(|handle| {
        seed_task_clock(&handle, fire_at);
        let prev_day = outbe_system_common::time::previous_date_key(day).expect("date step");
        close_failed_allocation(handle, prev_day);
    });
    (storage, day, fire_at)
}

/// A completed day with no amounts, so the Cycle starts the day after it.
fn close_failed_allocation(handle: StorageHandle<'_>, day: u32) {
    use outbe_emission::schema::{desis_state, emission_day_state, metadosis_state};
    let mut record = outbe_emission::EmissionDay::new(
        day,
        outbe_system_common::time::WorldwideDay::new(0),
        outbe_emission::api::compute_metadosis_start(day).unwrap(),
        0,
        [0; 5],
    );
    record.state = emission_day_state::COMPLETED;
    record.metadosis_state = metadosis_state::COMPLETED;
    record.desis_state = desis_state::COMPLETED;
    outbe_emission::api::put_emission_day(handle, &record).unwrap();
}

fn run_hour(
    storage: &mut HashMapStorageProvider,
    block_number: u64,
    timestamp: u64,
) -> outbe_primitives::error::Result<()> {
    storage.set_block_number(block_number);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(block_number, timestamp), handle);
        account_parent(&ctx, block_number);
        run_due_tasks(&ctx)
    })
}

fn emission_day_record(
    storage: &mut HashMapStorageProvider,
    day: u32,
) -> Option<outbe_emission::schema::EmissionDay> {
    storage.enter(|handle| outbe_emission::api::get_emission_day(handle, day).unwrap())
}

#[test]
fn step_error_takes_its_fallback() {
    use outbe_emission::schema::{emission_day_state, metadosis_state};

    // With no mutation frame, Metadosis reverts and its fallback closes it.
    let (mut metadosis, day, fire_at) = due_emission_day_fixture();
    metadosis.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 0);
    run_hour(&mut metadosis, 20, fire_at).unwrap();
    let failed = emission_day_record(&mut metadosis, day).unwrap();
    assert_eq!(failed.state, emission_day_state::PROCESSING);
    assert_eq!(failed.metadosis_state, metadosis_state::FAILED);
    assert_ne!(failed.metadosis_limit, 0, "the limit stays in the record");

    metadosis.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 16);
    run_hour(&mut metadosis, 21, fire_at + 3_600).unwrap();
    let later = emission_day_record(&mut metadosis, day).unwrap();
    assert_eq!(
        (later.metadosis_state, later.nods, later.metadosis_limit),
        (failed.metadosis_state, failed.nods, failed.metadosis_limit),
        "the failed Metadosis does not run again"
    );
    let failed_events = metadosis
        .get_events(outbe_system_common::addresses::METADOSIS_ADDRESS)
        .iter()
        .filter(|log| {
            outbe_metadosis::precompile::IMetadosis::MetadosisFailed::decode_log_data(log).is_ok()
        })
        .count();
    assert_eq!(failed_events, 1);

    let (mut node_fault, day, fire_at) = due_emission_day_fixture();
    let before = emission_day_record(&mut node_fault, day);
    node_fault.fail_mutation_at_address(outbe_system_common::addresses::METADOSIS_ADDRESS);
    assert!(run_hour(&mut node_fault, 20, fire_at).is_err());
    node_fault.clear_mutation_failure();
    assert_eq!(emission_day_record(&mut node_fault, day), before);
}

#[test]
fn conservation_mismatch_fails_the_block() {
    let (mut storage, day, fire_at) = due_emission_day_fixture();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 16);
    storage.set_block_number(20);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), 20, fire_at);
        let mut record = outbe_emission::api::get_emission_day(ctx.storage.clone(), day)
            .unwrap()
            .unwrap();
        assert_eq!(
            record.metadosis_state,
            outbe_emission::schema::metadosis_state::COMPLETED
        );
        record.validators += 1;
        outbe_emission::api::put_emission_day(ctx.storage.clone(), &record).unwrap();

        let error =
            outbe_emission::api::record_desis_canceled(ctx.storage.clone(), day).unwrap_err();
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::FatalWithCause(_)
        ));
        assert_eq!(
            outbe_emission::api::get_emission_day(ctx.storage.clone(), day)
                .unwrap()
                .unwrap()
                .state,
            outbe_emission::schema::emission_day_state::PROCESSING
        );
    });
}
