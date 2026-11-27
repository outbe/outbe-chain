use super::*;

use crate::test_support::{
    meta_with_hash, record_genesis_day_parent, with_funded_genesis_block, with_genesis_block,
    FB_HASH_A, FB_HASH_B, VAL_X, VAL_Y, VAL_Z,
};

#[test]
fn get_day_fee_sum_native_returns_zero_when_unrecorded() {
    with_genesis_block(|ctx| {
        assert_eq!(get_day_fee_sum_native(&ctx, 20240101).unwrap(), U256::ZERO);
    });
}

#[test]
fn get_day_fee_sum_native_round_trips_after_finalized_metadata() {
    with_funded_genesis_block(300, |ctx| {
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_A, 1), 101, &[VAL_X, VAL_Y]);
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_B, 2), 199, &[VAL_X, VAL_Y]);

        // 101 + 199 = 300 raw.
        assert_eq!(
            get_day_fee_sum_native(&ctx, 20240101).unwrap(),
            U256::from(300u64)
        );
        // Untouched day stays zero.
        assert_eq!(get_day_fee_sum_native(&ctx, 20240102).unwrap(), U256::ZERO);
    });
}

fn weight(ctx: &BlockRuntimeContext, day: u32, voter: Address) -> U256 {
    validator_weight(&ctx.storage, day, voter).unwrap()
}

fn voter_count(ctx: &BlockRuntimeContext, day: u32) -> u32 {
    ctx.storage
        .contract::<Rewards>()
        .day_voter_count
        .read(&day)
        .unwrap()
}

#[test]
fn each_voter_counts_once_per_block() {
    with_funded_genesis_block(400, |ctx| {
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_A, 1), 100, &[VAL_Y, VAL_X]);
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_B, 2), 100, &[VAL_X, VAL_Z]);

        assert_eq!(voter_count(&ctx, 20240101), 3);
        assert_eq!(weight(&ctx, 20240101, VAL_Y), U256::from(1));
        assert_eq!(weight(&ctx, 20240101, VAL_X), U256::from(2));
        assert_eq!(weight(&ctx, 20240101, VAL_Z), U256::from(1));
    });
}

/// Two entries for one validator would collide on the reward Gem's id.
#[test]
fn a_voter_is_counted_once_however_often_they_vote() {
    with_funded_genesis_block(400, |ctx| {
        // Twice inside one block, then again in the next one.
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_A, 1), 100, &[VAL_X, VAL_X]);
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_B, 2), 100, &[VAL_X]);

        assert_eq!(voter_count(&ctx, 20240101), 1, "one entry per voter");
        assert_eq!(
            weight(&ctx, 20240101, VAL_X),
            U256::from(2),
            "a repeat inside one block is not a vote"
        );
    });
}

#[test]
fn a_day_with_no_metadata_has_no_voter() {
    with_genesis_block(|ctx| {
        assert_eq!(voter_count(&ctx, 20240101), 0);
    });
}

#[test]
fn participation_after_the_day_is_sealed_is_refused() {
    use crate::test_support::{with_block, GENESIS_TS, SECONDS_PER_DAY};
    with_block(20, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
        seal_votes_and_fees(&ctx, 20240101).unwrap();
        let err = crate::finalized_metadata_hook::record_votes_and_fees(
            &ctx,
            &meta_with_hash(FB_HASH_A, 1),
            U256::from(100u64),
            crate::test_support::GENESIS_TS,
            &[VAL_X],
        )
        .unwrap_err();
        assert!(err.to_string().contains("after its day was sealed"));
    });
}

#[test]
fn sealed_inputs_require_completed_day_and_later_than_window_close() {
    use crate::test_support::{with_block, GENESIS_TS, SECONDS_PER_DAY};
    with_block(20, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
        let day = 20240101;
        assert!(validator_day_weights(&ctx.storage, day).is_err());
        let rewards = ctx.storage.contract::<Rewards>();
        rewards.day_votes_sealed_at_block.write(&day, 20).unwrap();
        assert!(seal_votes_and_fees(&ctx, day).is_err());
        assert!(validator_day_weights(&ctx.storage, day).is_err());
        rewards.day_votes_sealed_at_block.write(&day, 19).unwrap();
        seal_votes_and_fees(&ctx, day).unwrap();
        assert_eq!(
            validator_day_weights(&ctx.storage, day).unwrap(),
            (0, U256::ZERO)
        );
        assert!(seal_votes_and_fees(&ctx, 20240102).is_err());
        assert!(validator_day_weights(&ctx.storage, 20240102).is_err());
    });
}

#[test]
fn seal_watermark_validates_days_and_retains_calendar_catchup_policy() {
    use crate::test_support::{block_ctx, with_block, GENESIS_TS, SECONDS_PER_DAY};
    use outbe_system_common::time::{date_key_to_utc_timestamp, next_date_key};

    with_block(20, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
        assert_eq!(get_last_sealed_day(&ctx.storage).unwrap(), 0);
        for day in [0, 20231231, 20240100, 20240102, 20240103] {
            assert!(seal_votes_and_fees(&ctx, day).is_err(), "day {day}");
            assert_eq!(get_last_sealed_day(&ctx.storage).unwrap(), 0);
        }
        let rewards = ctx.storage.contract::<Rewards>();
        rewards
            .day_votes_sealed_at_block
            .write(&20240101, 20)
            .unwrap();
        for height in [19, 20] {
            let boundary = BlockRuntimeContext::new(
                block_ctx(height, GENESIS_TS + SECONDS_PER_DAY),
                ctx.storage.clone(),
            );
            assert!(!is_voting_complete(&boundary, 20240101).unwrap());
            assert!(seal_votes_and_fees(&boundary, 20240101).is_err());
        }
        let ready = BlockRuntimeContext::new(
            block_ctx(21, GENESIS_TS + SECONDS_PER_DAY),
            ctx.storage.clone(),
        );
        assert!(is_voting_complete(&ready, 20240101).unwrap());
        seal_votes_and_fees(&ready, 20240101).unwrap();
        assert!(seal_votes_and_fees(&ready, 20240101).is_err());

        // A long halt does not force processing days outside Cycle's catch-up window.
        let restart = BlockRuntimeContext::new(
            block_ctx(22, date_key_to_utc_timestamp(20250102).unwrap()),
            ctx.storage.clone(),
        );
        assert!(seal_votes_and_fees(&restart, 20240230).is_err());
        seal_votes_and_fees(&restart, 20241231).unwrap();
        assert_eq!(get_last_sealed_day(&ctx.storage).unwrap(), 20241231);
        assert!(seal_votes_and_fees(&restart, 20240102).is_err());
        seal_votes_and_fees(&restart, next_date_key(20241231).unwrap()).unwrap();
        assert_eq!(get_last_sealed_day(&ctx.storage).unwrap(), 20250101);
        assert!(crate::finalized_metadata_hook::record_reward_participation(
            &restart, FB_HASH_A, 20240102, VAL_X,
        )
        .is_err());
    });
}

#[test]
fn sealing_rollback_preserves_inputs_and_allows_retry() {
    use crate::test_support::{with_block, GENESIS_TS, SECONDS_PER_DAY};
    with_block(20, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
        crate::finalized_metadata_hook::record_reward_participation(
            &ctx, FB_HASH_A, 20240101, VAL_X,
        )
        .unwrap();
        let error = ctx.storage.with_checkpoint(|| {
            seal_votes_and_fees(&ctx, 20240101)?;
            Err::<(), _>(crate::errors::RewardsError::DayNotReadyToSeal.into())
        });
        assert!(error.is_err());
        assert_eq!(get_last_sealed_day(&ctx.storage).unwrap(), 0);
        assert_eq!(weight(&ctx, 20240101, VAL_X), U256::ONE);
        seal_votes_and_fees(&ctx, 20240101).unwrap();
        assert_eq!(
            validator_day_weights(&ctx.storage, 20240101).unwrap(),
            (1, U256::ONE)
        );
    });
}

#[test]
fn forward_seal_waits_for_an_older_parents_late_votes_without_processing_skipped_days() {
    use crate::block_fees::{record_late_vote, seal_block_votes};
    use crate::test_support::{block_ctx, CHAIN_ID, GENESIS_TS, SECONDS_PER_DAY};
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enter(|storage| {
        let ctx =
            BlockRuntimeContext::new(block_ctx(11, GENESIS_TS + 70 * SECONDS_PER_DAY), storage);
        let old_day = 20240101;
        let catchup_day = 20240107;
        let mut metadata = meta_with_hash(FB_HASH_A, 10);
        metadata.ordered_committee = vec![VAL_X, VAL_Y];
        crate::finalized_metadata_hook::record_votes_and_fees(
            &ctx,
            &metadata,
            U256::ZERO,
            GENESIS_TS,
            &[VAL_X],
        )
        .unwrap();
        assert!(!is_voting_complete(&ctx, catchup_day).unwrap());
        assert!(seal_votes_and_fees(&ctx, catchup_day).is_err());
        assert_eq!(get_last_sealed_day(&ctx.storage).unwrap(), 0);

        let late =
            BlockRuntimeContext::new(block_ctx(12, ctx.block.timestamp), ctx.storage.clone());
        record_late_vote(&late, FB_HASH_A, VAL_Y, 2).unwrap();
        assert_eq!(weight(&ctx, old_day, VAL_X), U256::ONE);
        assert_eq!(weight(&ctx, old_day, VAL_Y), U256::ONE);
        assert!(!is_voting_complete(&late, catchup_day).unwrap());
        let closed =
            BlockRuntimeContext::new(block_ctx(13, ctx.block.timestamp), ctx.storage.clone());
        // The real LateFinalizeCredits phase removes this window before CycleTick.
        seal_block_votes(&closed, 13, 3).unwrap();
        assert!(is_voting_complete(&closed, catchup_day).unwrap());
        seal_votes_and_fees(&closed, catchup_day).unwrap();
        assert_eq!(get_last_sealed_day(&ctx.storage).unwrap(), catchup_day);
    });
}

#[test]
fn aggregate_counts_base_and_late_votes_once_and_prevalidates_overflow() {
    use crate::finalized_metadata_hook::record_reward_participation;
    use crate::test_support::{with_block, GENESIS_TS, SECONDS_PER_DAY};
    with_block(20, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
        let day = 20240101;
        record_reward_participation(&ctx, FB_HASH_A, day, VAL_X).unwrap();
        record_reward_participation(&ctx, FB_HASH_A, day, VAL_X).unwrap();
        record_reward_participation(&ctx, FB_HASH_A, day, VAL_Y).unwrap();
        record_reward_participation(&ctx, FB_HASH_B, day, VAL_X).unwrap();
        seal_votes_and_fees(&ctx, day).unwrap();
        assert_eq!(
            validator_day_weights(&ctx.storage, day).unwrap(),
            (2, U256::from(3))
        );
        assert_eq!(weight(&ctx, day, VAL_X), U256::from(2));
        assert_eq!(weight(&ctx, day, VAL_Y), U256::from(1));

        let rewards = ctx.storage.contract::<Rewards>();
        let other_day = 20240102;
        rewards
            .day_total_votes
            .write(&other_day, U256::MAX)
            .unwrap();
        assert!(record_reward_participation(&ctx, FB_HASH_B, other_day, VAL_Z).is_err());
        assert_eq!(rewards.day_voter_count.read(&other_day).unwrap(), 0);
        assert!(!rewards
            .participation_counted_for_block
            .get_nested(&FB_HASH_B)
            .read(&VAL_Z)
            .unwrap());
        rewards
            .day_total_votes
            .write(&other_day, U256::ZERO)
            .unwrap();
        rewards.day_voter_count.write(&other_day, u32::MAX).unwrap();
        assert!(record_reward_participation(&ctx, FB_HASH_B, other_day, VAL_Z).is_err());
        assert_eq!(weight(&ctx, other_day, VAL_Z), U256::ZERO);
    });
}

#[test]
fn a_claim_takes_the_weight_and_the_last_one_clears_the_day() {
    use crate::finalized_metadata_hook::record_reward_participation;
    use crate::test_support::{with_block, GENESIS_TS, SECONDS_PER_DAY};
    with_block(20, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
        let day = 20240101;
        record_reward_participation(&ctx, FB_HASH_A, day, VAL_X).unwrap();
        record_reward_participation(&ctx, FB_HASH_B, day, VAL_X).unwrap();
        record_reward_participation(&ctx, FB_HASH_A, day, VAL_Y).unwrap();
        let rewards = ctx.storage.contract::<Rewards>();
        rewards
            .day_fee_sum_native
            .write(&day, U256::from(17))
            .unwrap();
        assert!(take_validator_weight(&ctx.storage, day, VAL_X).is_err());
        assert!(clear_day_seal(&ctx.storage, day).is_err());
        seal_votes_and_fees(&ctx, day).unwrap();

        clear_day_seal(&ctx.storage, day).unwrap();
        assert_eq!(rewards.day_fee_sum_native.read(&day).unwrap(), U256::ZERO);
        assert_eq!(
            take_validator_weight(&ctx.storage, day, VAL_X).unwrap(),
            U256::from(2)
        );
        assert_eq!(
            take_validator_weight(&ctx.storage, day, VAL_X).unwrap(),
            U256::ZERO
        );
        assert_eq!(weight(&ctx, day, VAL_Y), U256::ONE);
        clear_validator_day(&ctx.storage, day).unwrap();
        assert_eq!(
            validator_day_weights(&ctx.storage, day).unwrap(),
            (0, U256::ZERO)
        );
        assert!(record_reward_participation(&ctx, FB_HASH_B, day, VAL_Z).is_err());
    });
}
