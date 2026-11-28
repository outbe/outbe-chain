//! UTC-day calendar transitions: contiguous settlement and one-day-per-hourly-run catch-up (`OIP-00343`).

use super::*;
use outbe_system_common::emission::MAX_CATCH_UP_DAYS;

#[test]
fn process_emission_day_catches_up_one_oldest_day_per_hourly_run_after_a_multi_day_halt() {
    let mut storage = cycle_storage();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 32);
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
            outbe_worldwideday::api::get_worldwide_day(
                fire.storage.clone(),
                outbe_system_common::time::WorldwideDay::new(20_240_104),
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
fn the_daily_step_adds_base_fees_and_burn_addresses_after_the_seal_and_before_metadosis() {
    use alloy_sol_types::SolEvent;
    use outbe_metadosis::precompile::IMetadosis;
    use outbe_promislimit::{precompile::IPromisLimit, PromisLimitContract, DEAD_ADDRESS};
    use outbe_system_common::addresses::PROMIS_LIMIT_ADDRESS;

    let minor = outbe_system_common::units::UNITS_PER_PROTOCOL_MINOR;
    let mut storage = cycle_storage();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 32);
    storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_tick(&anchor).unwrap();
    });
    let events_before = storage.get_ordered_events().len();

    storage.set_block_number(2);
    storage.enter(|handle| {
        PromisLimitContract::new(handle.clone())
            .count_base_fees(minor * U256::from(5) / U256::from(2))
            .unwrap();
        handle
            .increase_balance(Address::ZERO, minor * U256::from(3) + U256::from(5))
            .unwrap();
        handle.increase_balance(DEAD_ADDRESS, minor).unwrap();
        let fire = dispatch_at(handle.clone(), 2, GENESIS_TS + SECONDS_PER_DAY + 3_600);
        assert!(is_day_allocated(&fire, 20_240_101));
        assert_eq!(
            outbe_rewards::api::get_last_sealed_day(&handle).unwrap(),
            20_240_101
        );
        assert_eq!(
            PromisLimitContract::new(handle.clone())
                .base_fee_count()
                .unwrap(),
            minor / U256::from(2)
        );
        assert_eq!(handle.balance(Address::ZERO).unwrap(), U256::from(5));
        assert_eq!(handle.balance(DEAD_ADDRESS).unwrap(), U256::ZERO);
    });
    storage.set_block_number(3);
    storage.enter(|handle| {
        dispatch_at(handle, 3, GENESIS_TS + SECONDS_PER_DAY + 12 * 3_600 + 1);
    });

    let events = &storage.get_ordered_events()[events_before..];
    let position =
        |found: &dyn Fn(&alloy_primitives::Log) -> bool| events.iter().position(found).unwrap();
    let added = |code: u8, amount: u64| {
        move |log: &alloy_primitives::Log| {
            log.address == PROMIS_LIMIT_ADDRESS
                && IPromisLimit::PromisLimitAdded::decode_log(log).is_ok_and(|ev| {
                    ev.data.source == code && ev.data.amountMinor == U256::from(amount)
                })
        }
    };
    let allocated = position(&|log| {
        outbe_agentreward::precompile::IAgentReward::AgentRewardRateSet::decode_log(log)
            .is_ok_and(|ev| ev.data.emissionDay == 20_240_101)
    });
    let base_fees = position(&added(41, 2));
    let burn_addresses = position(&added(46, 4));
    let metadosis = position(&|log| {
        IMetadosis::MetadosisCompleted::decode_log(log)
            .is_ok_and(|ev| ev.data.emissionDay == 20_240_101)
    });
    assert!(allocated < base_fees && base_fees < burn_addresses && burn_addresses < metadosis);
}

#[test]
fn catchup_horizon_bounds_backward_scan_to_max_reward_catchup_days() {
    let mut storage = cycle_storage();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 16);
    storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_tick(&anchor).unwrap();
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        // 70 completed days pass with no block (> MAX_CATCH_UP_DAYS = 64).
        let halt_days = u64::from(MAX_CATCH_UP_DAYS) + 6;
        let restart_ts = GENESIS_TS + halt_days * SECONDS_PER_DAY + 3_600;
        let ctx = runtime_context(block_ctx(2, restart_ts), handle.clone());
        let yesterday = outbe_system_common::time::previous_date_key(
            outbe_system_common::time::timestamp_to_date_key(restart_ts),
        )
        .unwrap();
        let expected_floor = outbe_system_common::time::timestamp_to_date_key(
            outbe_system_common::time::date_key_to_utc_timestamp(yesterday).unwrap()
                - u64::from(MAX_CATCH_UP_DAYS - 1) * SECONDS_PER_DAY,
        );
        assert_eq!(active_utc_day(&ctx), expected_floor);
        account_parent(&ctx, 2);
        run_due_tasks(&ctx).unwrap();
        assert_eq!(
            outbe_rewards::api::get_last_sealed_day(&handle).unwrap(),
            expected_floor
        );
        assert!(is_day_allocated(&ctx, expected_floor));
        assert!(!is_day_allocated(&ctx, 20_240_101));
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
            assert!(!is_day_allocated(&ctx, 20_240_101));
        });
    }
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 4);
    storage.set_block_number(14);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), 14, restart_ts + 3);
        let floor =
            outbe_system_common::time::timestamp_to_date_key(GENESIS_TS + 6 * SECONDS_PER_DAY);
        assert_eq!(
            outbe_rewards::api::get_last_sealed_day(&handle).unwrap(),
            floor
        );
        assert!(is_day_allocated(&ctx, floor));
        for voter in [early, late] {
            assert_eq!(
                outbe_rewards::api::validator_weight(&handle, 20_240_101, voter).unwrap(),
                U256::from(1)
            );
        }
    });
}

#[test]
fn a_failed_metadosis_sends_its_limit_to_the_promis_limit() {
    use alloy_sol_types::SolEvent;
    use outbe_metadosis::precompile::IMetadosis;

    let mut storage = cycle_storage();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 32);
    storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_tick(&anchor).unwrap();
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        dispatch_at(handle.clone(), 2, GENESIS_TS + SECONDS_PER_DAY + 3_600);
        assert!(is_day_allocated(
            &runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 3_600), handle),
            20_240_101
        ));
    });
    storage.set_block_number(3);
    storage.enter(|handle| {
        // A corrupt Desis stage makes the start of Desis revert, so Metadosis fails.
        outbe_desis::schema::DesisContract::new(handle.clone())
            .auction_stage
            .write(
                &outbe_system_common::time::EmissionDate::new(20_240_101),
                u8::MAX,
            )
            .unwrap();
        let promis_before = outbe_promislimit::PromisLimitContract::new(handle.clone())
            .promis_limit()
            .unwrap();
        dispatch_at(
            handle.clone(),
            3,
            GENESIS_TS + SECONDS_PER_DAY + 12 * 3_600 + 1,
        );
        let record = outbe_emission::api::get_emission_day(handle.clone(), 20_240_101)
            .unwrap()
            .unwrap();
        assert_eq!(
            record.metadosis_state,
            outbe_emission::schema::metadosis_state::FAILED
        );
        assert_eq!(record.nods, 0);
        assert!(
            outbe_promislimit::PromisLimitContract::new(handle)
                .promis_limit()
                .unwrap()
                >= promis_before + U256::from(record.metadosis_limit)
        );
    });
    let failed: Vec<_> = storage
        .get_ordered_events()
        .iter()
        .filter_map(|log| IMetadosis::MetadosisFailed::decode_log(log).ok())
        .map(|ev| (ev.data.emissionDay, ev.data.reason))
        .collect();
    assert_eq!(failed, vec![(20_240_101, 1)]);
}
