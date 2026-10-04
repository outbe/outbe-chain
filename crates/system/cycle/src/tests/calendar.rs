//! UTC-day calendar transitions: contiguous settlement and multi-day halt forfeiture.

use super::*;

#[test]
fn protocol_cycle_forfeits_every_completed_day_after_a_multi_day_halt() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 16);
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&anchor);
        run_cycle_lifecycle(&anchor).unwrap();

        let fire_ts = GENESIS_TS + 3 * SECONDS_PER_DAY + 3_600;
        let fire = BlockRuntimeContext::new(block_ctx(2, fire_ts), handle);
        account_parent(&fire, 2);
        dispatch_triggers(&fire).unwrap();

        let rewards = fire
            .storage
            .contract::<outbe_rewards::schema::Rewards<'_>>();
        for day in [20_240_101, 20_240_102, 20_240_103] {
            assert!(!rewards.daily_settled.read(&day).unwrap());
            assert!(!rewards.daily_topup_settled.read(&day).unwrap());
            assert!(
                outbe_metadosis::api::day_limit_formation_receipt(
                    fire.storage.clone(),
                    outbe_primitives::time::WorldwideDay::new(day),
                )
                .unwrap()
                .is_none(),
                "forfeited day {day} must not gain a formation receipt"
            );
        }

        for day in [20_240_102, 20_240_103] {
            let wwd = outbe_primitives::time::WorldwideDay::new(day);
            assert!(
                outbe_metadosis::api::worldwide_day(fire.storage.clone(), wwd)
                    .unwrap()
                    .is_none(),
                "missed day {day} must not gain the WWD identity required by downstream OCOMP or Promis work"
            );
            assert!(
                outbe_metadosis::api::missed_offering_receipt(fire.storage.clone(), wwd)
                    .unwrap()
                    .is_none(),
                "missed day {day} must not gain a Promis-bearing terminal receipt"
            );
            assert!(
                outbe_metadosis::api::capacity_forfeiture_receipt(fire.storage.clone(), wwd)
                    .unwrap()
                    .is_none(),
                "missed day {day} must not gain a capacity/Promis receipt"
            );
        }
        assert!(
            outbe_metadosis::api::worldwide_day(
                fire.storage.clone(),
                outbe_primitives::time::WorldwideDay::new(20_240_104),
            )
            .unwrap()
            .is_some(),
            "the one current WWD flow must still run after the gap"
        );
        assert_eq!(
            fire.storage
                .contract::<Cycle<'_>>()
                .active_utc_day
                .read()
                .unwrap(),
            20_240_104
        );
    });
}

#[test]
fn a_day_whose_limit_a_multi_day_halt_skipped_misses_its_offering() {
    let genesis_day = outbe_primitives::time::WorldwideDay::new(20_240_101);
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 16);
    let lookback_end = storage.enter(|handle| {
        let anchor = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle.clone());
        anchor_genesis(&anchor);
        run_cycle_lifecycle(&anchor).unwrap();

        let fire = BlockRuntimeContext::new(
            block_ctx(2, GENESIS_TS + 3 * SECONDS_PER_DAY + 3_600),
            handle.clone(),
        );
        account_parent(&fire, 2);
        dispatch_triggers(&fire).unwrap();

        let projection = outbe_metadosis::api::worldwide_day(handle, genesis_day)
            .unwrap()
            .unwrap();
        assert_eq!(projection.metadosis_limit_minor, U256::ZERO);
        projection.lookback_end
    });

    advance_metadosis_only(&mut storage, 3, lookback_end).unwrap();

    storage.enter(|handle| {
        let projection = outbe_metadosis::api::worldwide_day(handle.clone(), genesis_day)
            .unwrap()
            .unwrap();
        assert_eq!(projection.status, outbe_metadosis::WwdStatus::Failed);
        let receipt = outbe_metadosis::api::missed_offering_receipt(handle.clone(), genesis_day)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.value_routed, U256::ZERO);
        assert!(outbe_tribute::TributeContract::new(handle)
            .is_day_sealed(genesis_day)
            .unwrap());
    });
}

#[test]
fn contiguous_day_settlement_failure_preserves_the_calendar_cursor() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 16);
    storage.enter(|handle| {
        let anchor = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle.clone());
        anchor_genesis(&anchor);
        run_cycle_lifecycle(&anchor).unwrap();

        // Create only the Metadosis half of day 1's idempotency pair. The
        // contiguous transition must fail before advancing the cursor.
        let inconsistent = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + 60), handle.clone());
        outbe_metadosis::commands::apply_cycle_day_limit(&inconsistent, U256::from(17_u8)).unwrap();
        assert!(outbe_metadosis::api::day_limit_formation_receipt(
            handle.clone(),
            outbe_primitives::time::WorldwideDay::new(20_240_101),
        )
        .unwrap()
        .is_some());

        let fire =
            BlockRuntimeContext::new(block_ctx(3, GENESIS_TS + SECONDS_PER_DAY + 3_600), handle);
        account_parent(&fire, 3);
        assert!(matches!(
            dispatch_triggers(&fire),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));

        let cycle: Cycle<'_> = fire.storage.contract::<Cycle<'_>>();
        assert_eq!(cycle.active_utc_day.read().unwrap(), 20_240_101);
        let rewards = fire
            .storage
            .contract::<outbe_rewards::schema::Rewards<'_>>();
        assert!(!rewards.daily_settled.read(&20_240_101).unwrap());
    });
}
