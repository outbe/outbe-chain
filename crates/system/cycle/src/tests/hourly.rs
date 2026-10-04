//! Hourly protocol-cycle outcomes and capacity forfeiture.

use super::*;

#[test]
fn hourly_protocol_cycle_commits_the_same_typed_missed_offering_outcome() {
    let wwd = outbe_primitives::time::WorldwideDay::new(20_240_105);
    let day_limit = U256::from(109);
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    let offering_end = storage.enter(|handle| {
        let formation_ctx = BlockRuntimeContext::new(
            block_ctx(10, wwd.start_timestamp() + 2 * 3_600),
            handle.clone(),
        );
        outbe_metadosis::commands::apply_cycle_day_limit(&formation_ctx, day_limit).unwrap();
        outbe_metadosis::api::worldwide_day(handle, wwd)
            .unwrap()
            .unwrap()
            .offering_end
    });
    let fire_at = offering_end.div_ceil(3_600) * 3_600;
    let previous_hour = fire_at - 3_600;

    storage.enter(|handle| {
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
    });
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(20, fire_at), handle.clone());
        account_parent(&ctx, 20);
        dispatch_triggers(&ctx).unwrap();

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
    use outbe_metadosis::constants::MAX_RETAINED_WWDS;
    use outbe_primitives::time::WorldwideDay;

    let victim = WorldwideDay::new(20_260_910);
    let retained = retained_days_before(victim, MAX_RETAINED_WWDS);
    let day_limit = U256::from(100);
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        outbe_tribute::TributeContract::new(handle)
            .initialize_fresh_ocomp_profile()
            .unwrap();
    });

    storage.enter(|handle| {
        outbe_metadosis::test_support::seed_ready_worldwide_days_for_capacity(
            handle.clone(),
            &retained,
        )
        .unwrap();
        let mut tribute = outbe_tribute::TributeContract::new(handle);
        for day in &retained {
            tribute.seal_day(*day).unwrap();
        }
    });

    let mut next_block = 2_u64;
    storage.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);
    let victim_projection = storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(
            block_ctx(next_block, victim.start_timestamp() + 2 * 3_600),
            handle.clone(),
        );
        outbe_metadosis::commands::apply_cycle_day_limit(&ctx, day_limit).unwrap();
        outbe_metadosis::api::worldwide_day(handle, victim)
            .unwrap()
            .unwrap()
    });
    next_block += 1;
    for boundary in [
        victim_projection.forming_end,
        victim_projection.lookback_end,
        victim_projection.offering_end,
    ] {
        advance_metadosis_only(&mut storage, next_block, boundary).unwrap();
        next_block += 1;
    }

    let fire_at = victim_projection.scheduled_process_time.div_ceil(3_600) * 3_600;
    let previous_hour = fire_at - 3_600;
    storage.enter(|handle| {
        let cycle: Cycle<'_> = handle.contract::<Cycle<'_>>();
        for spec in ACTIVE_TRIGGERS {
            cycle
                .last_executed_at
                .write(
                    &spec.id,
                    if spec.id == TriggerId::ProtocolCycle.as_u32() {
                        previous_hour
                    } else {
                        fire_at
                    },
                )
                .unwrap();
        }
        cycle
            .active_utc_day
            .write(outbe_primitives::time::timestamp_to_date_key(fire_at))
            .unwrap();
    });

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
