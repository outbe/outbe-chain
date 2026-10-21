use super::*;
use crate::test_support::{block_ctx, with_block, GENESIS_TS, SECONDS_PER_DAY};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::time::date_key_to_utc_timestamp;

fn seed_day(ctx: &BlockRuntimeContext, day: u32, count: u32) {
    let rewards = ctx.storage.contract::<Rewards>();
    for index in 0..count {
        let voter = Address::from_word(U256::from(index + 1).into());
        rewards
            .daily_voter_at
            .get_nested(&day)
            .write(&index, voter)
            .unwrap();
        rewards
            .daily_participation
            .get_nested(&day)
            .write(&voter, 2)
            .unwrap();
    }
    rewards.daily_voter_count.write(&day, count).unwrap();
    rewards
        .daily_fee_sum_raw
        .write(&day, U256::from(123))
        .unwrap();
    rewards.daily_last_window_close.write(&day, 9).unwrap();
    close_day_participation(ctx, day).unwrap();
    mark_day_settled(ctx, day).unwrap();
}

#[test]
fn retention_boundary_crosses_leap_day_and_year_and_preserves_tombstones() {
    for day in [20240225, 20241228] {
        let midnight = date_key_to_utc_timestamp(day);
        with_block(10, midnight + 8 * SECONDS_PER_DAY - 1, |ctx| {
            seed_day(&ctx, day, 1);
            let original = read_voters_for_day(&ctx, day).unwrap();
            prune_settled_days(&ctx).unwrap();
            assert_eq!(read_voters_for_day(&ctx, day).unwrap(), original);
            assert_eq!(read_daily_fee_sum_raw(&ctx, day).unwrap(), U256::from(123));
            let expired = BlockRuntimeContext::new(
                block_ctx(11, midnight + 8 * SECONDS_PER_DAY),
                ctx.storage.clone(),
            );
            prune_settled_days(&expired).unwrap();
            let rewards = ctx.storage.contract::<Rewards>();
            assert!(read_voters_for_day(&ctx, day).unwrap().is_empty());
            assert_eq!(
                rewards
                    .daily_participation
                    .get_nested(&day)
                    .read(&original[0].0)
                    .unwrap(),
                0
            );
            assert_eq!(
                rewards.daily_voter_at.get_nested(&day).read(&0).unwrap(),
                Address::ZERO
            );
            assert_eq!(read_daily_fee_sum_raw(&ctx, day).unwrap(), U256::ZERO);
            assert_eq!(rewards.daily_last_window_close.read(&day).unwrap(), 0);
            assert!(rewards.daily_settled.read(&day).unwrap());
            assert!(rewards.daily_participation_closed.read(&day).unwrap());
            assert_eq!(rewards.settled_day_cleanup_head.read().unwrap(), 1);
            assert_eq!(rewards.settled_day_cleanup_day_at.read(&0).unwrap(), 0);
        });
    }
}

#[test]
fn cleanup_resumes_a_daily_prefix_and_processes_only_one_day_even_if_empty() {
    with_block(10, GENESIS_TS + 9 * SECONDS_PER_DAY, |ctx| {
        seed_day(&ctx, 20240101, 600);
        seed_day(&ctx, 20240102, 0);
        let original = read_voters_for_day(&ctx, 20240101).unwrap();
        let rewards = ctx.storage.contract::<Rewards>();
        for remaining in [344, 88] {
            prune_settled_days(&ctx).unwrap();
            assert_eq!(
                read_voters_for_day(&ctx, 20240101).unwrap(),
                original[..remaining]
            );
            assert_eq!(rewards.settled_day_cleanup_head.read().unwrap(), 0);
            assert_eq!(
                read_daily_fee_sum_raw(&ctx, 20240102).unwrap(),
                U256::from(123)
            );
        }
        prune_settled_days(&ctx).unwrap();
        assert!(read_voters_for_day(&ctx, 20240101).unwrap().is_empty());
        assert_eq!(rewards.settled_day_cleanup_head.read().unwrap(), 1);
        assert_eq!(
            read_daily_fee_sum_raw(&ctx, 20240102).unwrap(),
            U256::from(123)
        );
        prune_settled_days(&ctx).unwrap();
        assert_eq!(rewards.settled_day_cleanup_head.read().unwrap(), 2);
        assert_eq!(read_daily_fee_sum_raw(&ctx, 20240102).unwrap(), U256::ZERO);
        prune_settled_days(&ctx).unwrap();
        mark_day_settled(&ctx, 20240101).unwrap();
        assert_eq!(rewards.settled_day_cleanup_tail.read().unwrap(), 2);
    });
}

#[test]
fn enqueue_requires_closed_complete_day_and_is_replay_safe_without_writes() {
    let mut storage = HashMapStorageProvider::new(1);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(10, GENESIS_TS), handle);
        assert!(mark_day_settled(&ctx, 20240101).is_err());
        close_day_participation(&ctx, 20240101).unwrap();
        let rewards = ctx.storage.contract::<Rewards>();
        rewards
            .daily_last_window_close
            .write(&20240101, 10)
            .unwrap();
        assert!(mark_day_settled(&ctx, 20240101).is_err());
        assert_eq!(rewards.settled_day_cleanup_tail.read().unwrap(), 0);
        rewards.daily_last_window_close.write(&20240101, 9).unwrap();
        mark_day_settled(&ctx, 20240101).unwrap();
    });
    storage.fail_mutation_at(0);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(10, GENESIS_TS), handle);
        mark_day_settled(&ctx, 20240101).unwrap();
    });
    assert_eq!(storage.clear_mutation_failure(), 0);
}

#[test]
fn invalid_queue_markers_and_incomplete_windows_do_not_discard_records() {
    for invalid in 0..5 {
        with_block(10, GENESIS_TS + 8 * SECONDS_PER_DAY, |ctx| {
            seed_day(&ctx, 20240101, 1);
            let rewards = ctx.storage.contract::<Rewards>();
            match invalid {
                0 => rewards.daily_settled.write(&20240101, false).unwrap(),
                1 => rewards
                    .daily_participation_closed
                    .write(&20240101, false)
                    .unwrap(),
                2 => rewards
                    .daily_last_window_close
                    .write(&20240101, 10)
                    .unwrap(),
                3 => rewards.settled_day_cleanup_day_at.write(&0, 0).unwrap(),
                _ => rewards.settled_day_cleanup_head.write(2).unwrap(),
            }
            assert!(prune_settled_days(&ctx)
                .unwrap_err()
                .to_string()
                .contains("inconsistent"));
            assert_eq!(rewards.daily_voter_count.read(&20240101).unwrap(), 1);
            assert_eq!(
                rewards.daily_fee_sum_raw.read(&20240101).unwrap(),
                U256::from(123)
            );
        });
    }
}

#[test]
fn enqueue_overflow_and_mid_transition_failure_are_atomic() {
    let mut storage = HashMapStorageProvider::new(1);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(10, GENESIS_TS), handle);
        close_day_participation(&ctx, 20240101).unwrap();
        let rewards = ctx.storage.contract::<Rewards>();
        rewards.settled_day_cleanup_tail.write(u64::MAX).unwrap();
        assert!(mark_day_settled(&ctx, 20240101)
            .unwrap_err()
            .to_string()
            .contains("overflow"));
        assert!(!is_day_settled(&ctx, 20240101).unwrap());
        rewards.settled_day_cleanup_tail.write(0).unwrap();
    });
    storage.fail_after_mutation_at(1);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(10, GENESIS_TS), handle);
        assert!(mark_day_settled(&ctx, 20240101).is_err());
    });
    storage.clear_mutation_failure();
    storage.enter(|handle| {
        let rewards = handle.contract::<Rewards>();
        assert_eq!(rewards.settled_day_cleanup_tail.read().unwrap(), 0);
        assert_eq!(rewards.settled_day_cleanup_day_at.read(&0).unwrap(), 0);
        assert!(!rewards.daily_settled.read(&20240101).unwrap());
    });
}

#[test]
fn mid_slice_failure_restores_entries_count_fee_and_queue_then_retry_succeeds() {
    let mut storage = HashMapStorageProvider::new(1);
    fn context(handle: outbe_primitives::storage::StorageHandle<'_>) -> BlockRuntimeContext<'_> {
        BlockRuntimeContext::new(block_ctx(10, GENESIS_TS + 8 * SECONDS_PER_DAY), handle)
    }
    storage.enter(|handle| seed_day(&context(handle), 20240101, 300));
    storage.fail_after_mutation_at(255);
    storage.enter(|handle| assert!(prune_settled_days(&context(handle)).is_err()));
    storage.clear_mutation_failure();
    storage.enter(|handle| {
        let ctx = context(handle);
        assert_eq!(read_voters_for_day(&ctx, 20240101).unwrap().len(), 300);
        let rewards = ctx.storage.contract::<Rewards>();
        for index in 0..300 {
            let voter = Address::from_word(U256::from(index + 1).into());
            assert_eq!(
                rewards
                    .daily_voter_at
                    .get_nested(&20240101)
                    .read(&index)
                    .unwrap(),
                voter
            );
            assert_eq!(
                rewards
                    .daily_participation
                    .get_nested(&20240101)
                    .read(&voter)
                    .unwrap(),
                2
            );
        }
        assert_eq!(
            rewards.daily_fee_sum_raw.read(&20240101).unwrap(),
            U256::from(123)
        );
        assert_eq!(rewards.daily_last_window_close.read(&20240101).unwrap(), 9);
        assert_eq!(rewards.settled_day_cleanup_head.read().unwrap(), 0);
        assert_eq!(
            rewards.settled_day_cleanup_day_at.read(&0).unwrap(),
            20240101
        );
        prune_settled_days(&ctx).unwrap();
        assert_eq!(read_voters_for_day(&ctx, 20240101).unwrap().len(), 44);
    });
}

#[test]
fn dequeue_failure_restores_the_entire_final_slice() {
    let mut storage = HashMapStorageProvider::new(1);
    fn context(handle: outbe_primitives::storage::StorageHandle<'_>) -> BlockRuntimeContext<'_> {
        BlockRuntimeContext::new(block_ctx(10, GENESIS_TS + 8 * SECONDS_PER_DAY), handle)
    }
    storage.enter(|handle| seed_day(&context(handle), 20240101, 1));
    // Fail after advancing the head, after every detail in this last slice was cleared.
    storage.fail_after_mutation_at(6);
    storage.enter(|handle| assert!(prune_settled_days(&context(handle)).is_err()));
    storage.clear_mutation_failure();
    storage.enter(|handle| {
        let ctx = context(handle);
        let rewards = ctx.storage.contract::<Rewards>();
        let voter = Address::from_word(U256::from(1).into());
        assert_eq!(
            read_voters_for_day(&ctx, 20240101).unwrap(),
            vec![(voter, 2)]
        );
        assert_eq!(
            rewards.daily_fee_sum_raw.read(&20240101).unwrap(),
            U256::from(123)
        );
        assert_eq!(rewards.daily_last_window_close.read(&20240101).unwrap(), 9);
        assert_eq!(rewards.settled_day_cleanup_head.read().unwrap(), 0);
        assert_eq!(rewards.settled_day_cleanup_tail.read().unwrap(), 1);
        assert_eq!(
            rewards.settled_day_cleanup_day_at.read(&0).unwrap(),
            20240101
        );
        prune_settled_days(&ctx).unwrap();
        assert!(read_voters_for_day(&ctx, 20240101).unwrap().is_empty());
        assert_eq!(rewards.settled_day_cleanup_head.read().unwrap(), 1);
    });
}

#[test]
fn closed_reclaimed_day_rejects_new_metadata_even_without_voters() {
    with_block(10, GENESIS_TS + 8 * SECONDS_PER_DAY, |ctx| {
        seed_day(&ctx, 20240101, 0);
        prune_settled_days(&ctx).unwrap();
        for voters in [vec![], vec![VAL_X]] {
            let err = crate::finalized_metadata_hook::on_finalized_metadata(
                &ctx,
                &meta_with_hash(FB_HASH_A, 1),
                U256::from(100),
                GENESIS_TS,
                &voters,
            )
            .unwrap_err();
            assert!(err.to_string().contains("after its day was credited"));
            let rewards = ctx.storage.contract::<Rewards>();
            assert_eq!(read_daily_fee_sum_raw(&ctx, 20240101).unwrap(), U256::ZERO);
            assert_eq!(rewards.pending_fees.read(&FB_HASH_A).unwrap(), U256::ZERO);
            assert_eq!(rewards.pending_reward_day.read(&FB_HASH_A).unwrap(), 0);
            assert_eq!(rewards.daily_last_window_close.read(&20240101).unwrap(), 0);
            assert!(!rewards.block_metadata_counted.read(&FB_HASH_A).unwrap());
            assert!(read_voters_for_day(&ctx, 20240101).unwrap().is_empty());
        }
    });
}
