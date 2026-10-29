//! Hourly protocol-cycle execution of due `EmissionDay` records.

use super::*;

#[test]
fn hourly_protocol_cycle_executes_due_emission_day_at_metadosis_start() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 16);
    let (emission_day, fire_at) = storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_lifecycle(&anchor).unwrap();
        let wwd = outbe_primitives::time::WorldwideDay::new(20_240_101);
        let wwd_proj = outbe_metadosis::api::worldwide_day(handle.clone(), wwd)
            .unwrap()
            .unwrap();
        let ed = outbe_metadosis::api::emission_day(handle, wwd_proj.day)
            .unwrap()
            .unwrap();
        assert_eq!(ed.wwd, wwd);
        assert_eq!(ed.state, outbe_metadosis::emission_day_state::NONE);
        (ed, ed.metadosis_start.div_ceil(3_600) * 3_600)
    });

    storage.enter(|handle| {
        seed_trigger_clock(&handle, fire_at);
        let prev_day =
            outbe_primitives::time::previous_date_key(emission_day.day).expect("date step");
        outbe_emission::api::record_failed_day_allocation(handle, prev_day, U256::ZERO).unwrap();
    });
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 16);
    storage.set_block_number(20);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), 20, fire_at);

        let updated = outbe_metadosis::api::emission_day(ctx.storage.clone(), emission_day.day)
            .unwrap()
            .unwrap();
        assert_eq!(
            updated.state,
            outbe_metadosis::emission_day_state::SUCCEEDED
        );

        let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle
                .last_executed_at
                .read(&TriggerId::ProtocolCycle.as_u32())
                .unwrap(),
            fire_at
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&TriggerId::ProtocolCycle.as_u32())
                .unwrap(),
            20
        );
    });
}

pub(super) fn seed_trigger_clock(handle: &StorageHandle<'_>, fire_at: u64) {
    let previous_hour = fire_at - 3_600;
    let cycle: Cycle<'_> = handle.contract::<Cycle<'_>>();
    for spec in ACTIVE_TRIGGERS {
        let last = if spec.id == TriggerId::ProtocolCycle.as_u32() {
            previous_hour
        } else {
            fire_at
        };
        cycle.last_executed_at.write(&spec.id, last).unwrap();
    }
}
