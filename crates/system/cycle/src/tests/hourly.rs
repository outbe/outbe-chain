//! Hourly protocol-cycle outcomes and capacity forfeiture.

use super::*;

#[test]
fn hourly_protocol_cycle_commits_the_same_typed_missed_offering_outcome() {
    let wwd = outbe_primitives::time::WorldwideDay::new(20_240_105);
    let day_limit = U256::from(109);
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    let offering_end =
        storage.enter(|handle| form_worldwide_day(handle, 10, wwd, day_limit).offering_end);
    let fire_at = offering_end.div_ceil(3_600) * 3_600;
    storage.enter(|handle| seed_trigger_clock(&handle, fire_at));
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), 20, fire_at);

        let projection = outbe_metadosis::api::worldwide_day(ctx.storage.clone(), wwd)
            .unwrap()
            .unwrap();
        assert_eq!(projection.status, outbe_metadosis::WwdStatus::Failed);
        assert_eq!(
            projection.membership,
            outbe_metadosis::WwdMembership::Closed
        );
        let receipt = outbe_metadosis::api::missed_offering_receipt(ctx.storage.clone(), wwd)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.value_routed, day_limit);
        assert_eq!(receipt.carry_over_before, U256::ZERO);
        assert_eq!(receipt.carry_over_after, day_limit);
        assert_eq!(
            receipt.retirement,
            outbe_compressed_entities::RetirementOutcome::NotPresent
        );
        assert_eq!(receipt.block_number, 20);

        let desis = ctx.storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&wwd).unwrap(),
            outbe_desis::schema::AuctionStage::None as u8
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

#[test]
fn hourly_protocol_cycle_applies_exact_capacity_forfeiture_to_the_new_due_candidate() {
    let _enclave = outbe_tribute::enclave_client::test_enclave::scope();
    use outbe_metadosis::constants::MAX_RETAINED_WWDS;
    use outbe_primitives::time::WorldwideDay;

    let victim = WorldwideDay::new(20_260_910);
    let retained = retained_days_before(victim, MAX_RETAINED_WWDS);
    let day_limit = U256::from(100);
    let CapacityScenario {
        mut storage,
        victim: victim_projection,
        next_block,
    } = capacity_scenario(&retained, victim, day_limit);

    let fire_at = victim_projection.scheduled_process_time.div_ceil(3_600) * 3_600;
    storage.enter(|handle| seed_trigger_clock(&handle, fire_at));

    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(next_block, fire_at), handle.clone());
        account_parent(&ctx, next_block);
        dispatch_triggers(&ctx).unwrap();

        let projection = outbe_metadosis::api::worldwide_day(handle.clone(), victim)
            .unwrap()
            .unwrap();
        assert_eq!(projection.status, outbe_metadosis::WwdStatus::Failed);
        assert_eq!(
            projection.membership,
            outbe_metadosis::WwdMembership::Closed
        );

        let receipt = outbe_metadosis::api::capacity_forfeiture_receipt(handle.clone(), victim)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.max_retained_wwds, MAX_RETAINED_WWDS as u32);
        assert_eq!(receipt.retained_count_before, MAX_RETAINED_WWDS as u32);
        assert_eq!(receipt.value_routed, day_limit);
        assert_eq!(receipt.carry_over_before, U256::ZERO);
        assert_eq!(receipt.carry_over_after, day_limit);
        assert_eq!(receipt.forfeited_count, 0);
        assert_eq!(receipt.forfeited_nominal, U256::ZERO);
        assert_eq!(
            receipt.retirement,
            outbe_compressed_entities::RetirementOutcome::NotPresent
        );

        let cycle: Cycle<'_> = handle.contract::<Cycle<'_>>();
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
            next_block
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
    cycle
        .active_utc_day
        .write(outbe_primitives::time::timestamp_to_date_key(fire_at))
        .unwrap();
}
