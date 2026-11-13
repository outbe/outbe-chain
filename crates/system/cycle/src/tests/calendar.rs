//! UTC-day calendar transitions: contiguous settlement and one-day-per-hourly-run catch-up (`OIP-00343`).

use super::*;
use crate::handler::MAX_REWARD_CATCHUP_DAYS;

#[test]
fn process_emission_day_catches_up_one_oldest_day_per_hourly_run_after_a_multi_day_halt() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 32);
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let anchor = genesis_block(handle.clone(), anchor_ts);
        run_cycle_tick(&anchor).unwrap();
    });

    // Block 2 after a 3-day halt settles only the oldest unallocated day (20240101).
    storage.set_block_number(2);
    storage.enter(|handle| {
        let fire_ts = GENESIS_TS + 3 * SECONDS_PER_DAY + 3_600;
        let fire = dispatch_at(handle, 2, fire_ts);

        assert!(is_day_allocated(&fire, 20_240_101));
        assert!(!is_day_allocated(&fire, 20_240_102));
        assert!(!is_day_allocated(&fire, 20_240_103));
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
        assert!(is_day_allocated(&fire, 20_240_102));
        assert!(!is_day_allocated(&fire, 20_240_103));
        assert_eq!(active_utc_day(&fire), 20_240_103);
    });

    // Block 4 at the next hourly boundary settles 20240103 and catches up to 20240104.
    storage.set_block_number(4);
    storage.enter(|handle| {
        let fire_ts = GENESIS_TS + 3 * SECONDS_PER_DAY + 3 * 3_600;
        let fire = dispatch_at(handle, 4, fire_ts);
        assert!(is_day_allocated(&fire, 20_240_103));
        assert_eq!(active_utc_day(&fire), 20_240_104);
    });
}

#[test]
fn catchup_horizon_bounds_backward_scan_to_max_reward_catchup_days() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 16);
    storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_tick(&anchor).unwrap();
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
        account_parent(&ctx, 2);
        run_due_tasks(&ctx).unwrap();
        assert_eq!(
            outbe_rewards::api::get_last_sealed_day(&handle).unwrap(),
            expected_floor
        );
        assert!(is_day_allocated(&ctx, expected_floor));
        assert!(
            outbe_emission::api::get_emission_day_record(handle.clone(), 20_240_101)
                .unwrap()
                .is_none()
        );
        assert!(!handle
            .contract::<outbe_rewards::schema::Rewards>()
            .day_cleanup_scheduled
            .read(&20_240_101)
            .unwrap());
        assert!(outbe_agentreward::AgentRewardContract::new(handle)
            .increment_waa_tribute(20_240_101u32.into(), Address::repeat_byte(0x46))
            .is_err());
    });
}

#[test]
fn forward_seal_jump_waits_for_skipped_parent_late_votes() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let anchor = genesis_block(handle, GENESIS_TS + 60);
        run_cycle_tick(&anchor).unwrap();
    });
    let restart_ts = GENESIS_TS + 70 * SECONDS_PER_DAY + 3_600;
    let hash = B256::repeat_byte(0x78);
    let early = Address::repeat_byte(0xB2);
    let late = Address::repeat_byte(0xB3);
    for height in 11..=13 {
        storage.set_block_number(height);
        storage.enter(|handle| {
            let ctx = runtime_context(block_ctx(height, restart_ts + height - 11), handle.clone());
            if height == 11 {
                // The first resumed block accounts an old parent outside the catch-up horizon.
                super::dispatch::record_parent_with_late_voter(&ctx, hash, early, late);
            }
            account_parent(&ctx, height);
            run_due_tasks(&ctx).unwrap();
            assert_eq!(outbe_rewards::api::get_last_sealed_day(&handle).unwrap(), 0);
            if height == 12 {
                outbe_rewards::block_fees::record_late_vote(&ctx, hash, late, 2).unwrap();
            }
            if height == 13 {
                ctx.storage
                    .with_checkpoint(|| outbe_rewards::block_fees::seal_block_votes(&ctx, 13, 3))
                    .unwrap();
            }
            assert!(
                outbe_emission::api::get_emission_day_record(handle, 20_240_101)
                    .unwrap()
                    .is_none()
            );
        });
    }
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    storage.set_block_number(14);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), 14, restart_ts + 3);
        let floor = outbe_primitives::time::timestamp_to_date_key(GENESIS_TS + 6 * SECONDS_PER_DAY);
        assert_eq!(
            outbe_rewards::api::get_last_sealed_day(&handle).unwrap(),
            floor
        );
        assert!(is_day_allocated(&ctx, floor));
        assert_eq!(
            outbe_rewards::api::get_day_voters(&ctx, 20_240_101).unwrap(),
            vec![(early, 1), (late, 1)]
        );
        assert!(!handle
            .contract::<outbe_rewards::schema::Rewards>()
            .day_cleanup_scheduled
            .read(&20_240_101)
            .unwrap());
    });
}

#[test]
fn a_failed_allocation_whose_limit_cannot_reach_the_promis_limit_fails_the_block() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let anchor = genesis_block(handle, GENESIS_TS + 60);
        run_cycle_tick(&anchor).unwrap();
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        let ctx = runtime_context(
            block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 3_600),
            handle.clone(),
        );
        account_parent(&ctx, 2);
        let mut agents = outbe_agentreward::AgentRewardContract::new(handle.clone());
        agents
            .increment_waa_tribute(20_240_101u32.into(), Address::repeat_byte(0x47))
            .unwrap();
        agents
            .pending_total_minor
            .write(
                &(outbe_agentreward::schema::RewardPool::Waa as u8),
                U256::MAX,
            )
            .unwrap();
        // Allocation overflows first; the full limit then overflows the Promis Limit in its fallback.
        outbe_promislimit::PromisLimitContract::new(handle.clone())
            .checked_credit(U256::MAX)
            .unwrap();
        let err = run_due_tasks(&ctx).unwrap_err();
        assert!(
            matches!(
                err,
                outbe_primitives::error::PrecompileError::FatalWithCause(_)
            ),
            "{err:?}"
        );
    });
}

#[test]
fn isolated_step_checkpoints_route_failed_allocation_and_failed_metadosis_to_promis_limit() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 32);
    storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_tick(&anchor).unwrap();
    });

    // Forcing `DistributionOverflow` (`RevertBytes`) inside reward allocation
    // tasks the Step 2 inner checkpoint rollback and `record_failed_day_allocation` fallback.
    storage.set_block_number(2);
    storage.enter(|handle| {
        let promis_before = outbe_promislimit::PromisLimitContract::new(handle.clone())
            .promis_limit()
            .unwrap();
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 3_600;
        let ctx = runtime_context(block_ctx(2, fire_ts), handle.clone());
        account_parent(&ctx, 2);
        let mut ar = outbe_agentreward::AgentRewardContract::new(handle.clone());
        ar.increment_waa_tribute(20_240_101u32.into(), Address::repeat_byte(0x44))
            .unwrap();
        ar.pending_total_minor
            .write(
                &(outbe_agentreward::schema::RewardPool::Waa as u8),
                U256::MAX,
            )
            .unwrap();

        run_due_tasks(&ctx).unwrap();

        let rec = outbe_emission::api::get_emission_day_record(handle.clone(), 20_240_101)
            .unwrap()
            .unwrap();
        let expected_cap = outbe_emission::day_emission::emission_limit_of_day(0);
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
            outbe_rewards::api::get_last_sealed_day(&handle).unwrap(),
            20_240_101
        );
        assert!(
            !outbe_agentreward::distribution::has_pending_accruals_for_day(&handle, 20_240_101,)
                .unwrap()
        );
        assert!(ar
            .increment_waa_tribute(20_240_101u32.into(), Address::repeat_byte(0x45))
            .is_err());
        crate::handler::process_agent_rewards(&ctx).unwrap();
        assert_eq!(ar.waa_address_count.read(&20_240_101u32.into()).unwrap(), 0);
        assert_eq!(
            outbe_rewards::api::get_last_sealed_day(&handle).unwrap(),
            20_240_101
        );
        crate::handler::allocate_emission_limit(&ctx, 20_240_101).unwrap();
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(handle.clone())
                .promis_limit()
                .unwrap(),
            promis_before + expected_cap
        );
    });

    use alloy_sol_types::SolEvent;
    use outbe_desis::precompile::IDesis;
    use outbe_metadosis::precompile::IMetadosis;

    let expected_cap = outbe_emission::day_emission::emission_limit_of_day(0);
    let alloc_failed: Vec<_> = storage
        .get_ordered_events()
        .iter()
        .filter_map(|log| crate::ICycle::DayEmissionLimitAllocationFailed::decode_log(log).ok())
        .map(|ev| ev.data)
        .collect();
    assert_eq!(alloc_failed.len(), 1);
    assert_eq!(alloc_failed[0].emissionDay, 20_240_101);
    assert_eq!(alloc_failed[0].emissionLimitMinor, expected_cap);
    assert_eq!(alloc_failed[0].reason, 1);

    let metadosis_canceled: Vec<_> = storage
        .get_ordered_events()
        .iter()
        .filter_map(|log| IMetadosis::MetadosisCanceled::decode_log(log).ok())
        .map(|ev| ev.data)
        .collect();
    assert_eq!(metadosis_canceled.len(), 1);
    assert_eq!(metadosis_canceled[0].emissionDay, 20_240_101);
    assert_eq!(metadosis_canceled[0].reason, 1);

    assert!(storage
        .get_ordered_events()
        .iter()
        .filter_map(|log| IMetadosis::MetadosisLimitSet::decode_log(log).ok())
        .all(|ev| ev.data.emissionDay != 20_240_101));
    assert!(storage
        .get_ordered_events()
        .iter()
        .filter_map(|log| IDesis::DesisLimitSet::decode_log(log).ok())
        .all(|ev| ev.data.emissionDay != 20_240_101));

    // Now test Step 3 (`start_metadosis`) recoverable revert fallback (`MetadosisFailed(E, 1)`):
    // Clear the WAA pending total so day 20240102 allocates cleanly, and pre-set Desis stage
    // for 20240102 so `start_metadosis` reverts with `InvalidStageTransition`.
    storage.set_block_number(3);
    storage.enter(|handle| {
        let ar = outbe_agentreward::AgentRewardContract::new(handle.clone());
        ar.pending_total_minor
            .write(
                &(outbe_agentreward::schema::RewardPool::Waa as u8),
                U256::ZERO,
            )
            .unwrap();
        outbe_desis::schema::DesisContract::new(handle.clone())
            .auction_stage
            .write(
                &outbe_primitives::time::EmissionDate::new(20_240_102),
                outbe_desis::schema::AuctionStage::Briefed as u8,
            )
            .unwrap();
        let promis_before_day2 = outbe_promislimit::PromisLimitContract::new(handle.clone())
            .promis_limit()
            .unwrap();
        let fire_ts = GENESIS_TS + 2 * SECONDS_PER_DAY + 3_600;
        let ctx = runtime_context(block_ctx(3, fire_ts), handle.clone());
        account_parent(&ctx, 3);
        run_due_tasks(&ctx).unwrap();

        let rec = outbe_emission::api::get_emission_day_record(handle.clone(), 20_240_102)
            .unwrap()
            .unwrap();
        assert_eq!(
            rec.metadosis_state,
            outbe_emission::schema::metadosis_state::FAILED
        );
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(handle)
                .promis_limit()
                .unwrap(),
            promis_before_day2 + U256::from(rec.metadosis_limit)
        );
    });

    let metadosis_failed: Vec<_> = storage
        .get_ordered_events()
        .iter()
        .filter_map(|log| IMetadosis::MetadosisFailed::decode_log(log).ok())
        .map(|ev| ev.data)
        .collect();
    assert_eq!(metadosis_failed.len(), 1);
    assert_eq!(metadosis_failed[0].emissionDay, 20_240_102);
    assert_eq!(metadosis_failed[0].reason, 1);
}
