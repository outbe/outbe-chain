//! UTC-day calendar transitions: contiguous settlement and multi-day halt forfeiture.

use outbe_rewards::schema::Rewards;

use super::*;

#[test]
fn protocol_cycle_forfeits_every_completed_day_after_a_multi_day_halt() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 16);
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let anchor = genesis_block(handle.clone(), anchor_ts);
        run_cycle_lifecycle(&anchor).unwrap();

        let fire_ts = GENESIS_TS + 3 * SECONDS_PER_DAY + 3_600;
        let fire = dispatch_at(handle, 2, fire_ts);

        let rewards = fire.storage.contract::<Rewards<'_>>();
        for day in [20_240_101, 20_240_102, 20_240_103] {
            assert!(!rewards.daily_settled.read(&day).unwrap());
        }

        assert!(
            outbe_metadosis::api::worldwide_day(
                fire.storage.clone(),
                outbe_primitives::time::WorldwideDay::new(20_240_104),
            )
            .unwrap()
            .is_some(),
            "the current WWD flow must still run after the gap"
        );
        let cycle = fire.storage.contract::<Cycle<'_>>();
        assert_eq!(cycle.active_utc_day.read().unwrap(), 20_240_104);
    });
}

#[test]
fn a_multi_day_halt_issues_no_emission_and_credits_no_capacity_or_native_balance() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 16);
    storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_lifecycle(&anchor).unwrap();

        let promis_limit_before = outbe_promislimit::PromisLimitContract::new(handle.clone())
            .promis_limit()
            .unwrap();
        let rewards_before = handle
            .balance(outbe_primitives::addresses::REWARDS_ADDRESS)
            .unwrap();
        let metadosis_before = handle
            .balance(outbe_primitives::addresses::METADOSIS_ADDRESS)
            .unwrap();
        let promis_limit_native_before = handle
            .balance(outbe_primitives::addresses::PROMIS_LIMIT_ADDRESS)
            .unwrap();

        // Three completed UTC days pass with no block. The first block after the
        // halt forfeits them instead of synthesizing their emission.
        let fire = BlockRuntimeContext::new(
            block_ctx(2, GENESIS_TS + 3 * SECONDS_PER_DAY + 3_600),
            handle.clone(),
        );
        account_parent(&fire, 2);
        dispatch_triggers(&fire).unwrap();

        let rewards = fire
            .storage
            .contract::<outbe_rewards::schema::Rewards<'_>>();
        for day in [20_240_101, 20_240_102, 20_240_103] {
            assert!(
                !rewards.daily_settled.read(&day).unwrap(),
                "day {day}: no daily emission settlement"
            );
        }
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(handle.clone())
                .promis_limit()
                .unwrap(),
            promis_limit_before,
            "missed days must not credit Promis Limit capacity"
        );
        assert_eq!(
            handle
                .balance(outbe_primitives::addresses::REWARDS_ADDRESS)
                .unwrap(),
            rewards_before,
            "missed days must not mint native emission into the rewards escrow"
        );
        assert_eq!(
            handle
                .balance(outbe_primitives::addresses::METADOSIS_ADDRESS)
                .unwrap(),
            metadosis_before,
            "missed days must not route native emission to Metadosis"
        );
        assert_eq!(
            handle
                .balance(outbe_primitives::addresses::PROMIS_LIMIT_ADDRESS)
                .unwrap(),
            promis_limit_native_before
        );
    });
}
