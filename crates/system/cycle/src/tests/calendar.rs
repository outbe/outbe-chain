//! UTC-day calendar transitions: contiguous settlement and 1-day-per-block catch-up (`OIP-00343`).

use super::*;
use crate::handler::MAX_REWARD_CATCHUP_DAYS;

#[test]
fn protocol_cycle_catches_up_one_oldest_day_per_block_after_a_multi_day_halt() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 32);
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let anchor = genesis_block(handle.clone(), anchor_ts);
        run_cycle_lifecycle(&anchor).unwrap();
    });

    // Block 2 after a 3-day halt settles only the oldest unallocated day (20240101).
    storage.set_block_number(2);
    storage.enter(|handle| {
        let fire_ts = GENESIS_TS + 3 * SECONDS_PER_DAY + 3_600;
        let fire = dispatch_at(handle, 2, fire_ts);

        assert!(is_day_settled(&fire, 20_240_101));
        assert!(!is_day_settled(&fire, 20_240_102));
        assert!(!is_day_settled(&fire, 20_240_103));
        assert_eq!(active_utc_day(&fire), 20_240_102);

        assert!(
            outbe_metadosis::api::worldwide_day(
                fire.storage.clone(),
                outbe_primitives::time::WorldwideDay::new(20_240_104),
            )
            .unwrap()
            .is_some(),
            "the current WWD flow must still run after the gap"
        );
    });

    // Block 3 at the next hourly boundary settles 20240102.
    storage.set_block_number(3);
    storage.enter(|handle| {
        let fire_ts = GENESIS_TS + 3 * SECONDS_PER_DAY + 2 * 3_600;
        let fire = dispatch_at(handle, 3, fire_ts);
        assert!(is_day_settled(&fire, 20_240_102));
        assert!(!is_day_settled(&fire, 20_240_103));
        assert_eq!(active_utc_day(&fire), 20_240_103);
    });

    // Block 4 at the next hourly boundary settles 20240103 and catches up to 20240104.
    storage.set_block_number(4);
    storage.enter(|handle| {
        let fire_ts = GENESIS_TS + 3 * SECONDS_PER_DAY + 3 * 3_600;
        let fire = dispatch_at(handle, 4, fire_ts);
        assert!(is_day_settled(&fire, 20_240_103));
        assert_eq!(active_utc_day(&fire), 20_240_104);
    });
}

#[test]
fn catchup_horizon_bounds_backward_scan_to_max_reward_catchup_days() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 16);
    storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_lifecycle(&anchor).unwrap();
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        // 70 completed days pass with no block (> MAX_REWARD_CATCHUP_DAYS = 64).
        let halt_days = u64::from(MAX_REWARD_CATCHUP_DAYS) + 6;
        let restart_ts = GENESIS_TS + halt_days * SECONDS_PER_DAY + 3_600;
        let ctx = runtime_context(block_ctx(2, restart_ts), handle.clone());
        let yesterday = outbe_primitives::time::previous_date_key(
            outbe_primitives::time::timestamp_to_date_key(restart_ts),
        )
        .unwrap();
        let expected_floor = outbe_primitives::time::timestamp_to_date_key(
            outbe_primitives::time::date_key_to_utc_timestamp(yesterday).unwrap()
                - u64::from(MAX_REWARD_CATCHUP_DAYS - 1) * SECONDS_PER_DAY,
        );
        assert_eq!(active_utc_day(&ctx), expected_floor);
    });
}

#[test]
fn isolated_step_checkpoints_route_failed_allocation_and_failed_metadosis_to_promis_limit() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 32);
    storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_lifecycle(&anchor).unwrap();
    });

    // Forcing `DistributionOverflow` (`RevertBytes`) inside `distribute_reward_pools`
    // triggers the Step 2 inner checkpoint rollback and `record_failed_day_allocation` fallback.
    storage.set_block_number(2);
    storage.enter(|handle| {
        let promis_before = outbe_promislimit::PromisLimitContract::new(handle.clone())
            .promis_limit()
            .unwrap();
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 3_600;
        let ctx = runtime_context(block_ctx(2, fire_ts), handle.clone());
        account_parent(&ctx, 2);
        let ar = outbe_agentreward::AgentRewardContract::new(handle.clone());
        ar.waa_total_count.write(&20_240_101u32.into(), 1).unwrap();
        ar.pending_total_minor
            .write(
                &(outbe_agentreward::schema::RewardPool::Waa as u8),
                U256::MAX,
            )
            .unwrap();

        dispatch_triggers(&ctx).unwrap();

        let rec = outbe_emission::api::get_emission_day_record(handle.clone(), 20_240_101)
            .unwrap()
            .unwrap();
        let expected_cap = outbe_emission::day_emission::day_emission_limit(0);
        assert_eq!(
            rec.state,
            outbe_emission::schema::emission_day_state::COMPLETED
        );
        assert_eq!(
            rec.metadosis_state,
            outbe_emission::schema::metadosis_state::CANCELLED
        );
        assert_eq!(U256::from(rec.promis_limit), expected_cap);
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(handle.clone())
                .promis_limit()
                .unwrap(),
            promis_before + expected_cap
        );
    });
}
