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

#[test]
fn get_day_voters_orders_by_first_seen() {
    with_funded_genesis_block(400, |ctx| {
        // FB_HASH_A: Y first, then X (first-seen-on-day = Y, X)
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_A, 1), 100, &[VAL_Y, VAL_X]);
        // FB_HASH_B (same day): Z is new, X already seen
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_B, 2), 100, &[VAL_X, VAL_Z]);

        let voters = get_day_voters(&ctx, 20240101).unwrap();
        // First-seen order: Y (block A), X (block A), Z (block B).
        assert_eq!(voters.len(), 3);
        assert_eq!(voters[0].0, VAL_Y);
        assert_eq!(voters[0].1, 1);
        assert_eq!(voters[1].0, VAL_X);
        assert_eq!(voters[1].1, 2); // X participated in both A and B
        assert_eq!(voters[2].0, VAL_Z);
        assert_eq!(voters[2].1, 1);
    });
}

/// Two entries for one validator would collide on the reward Gem's id.
#[test]
fn a_voter_is_listed_once_however_often_they_vote() {
    with_funded_genesis_block(400, |ctx| {
        // Twice inside one block, then again in the next one.
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_A, 1), 100, &[VAL_X, VAL_X]);
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_B, 2), 100, &[VAL_X]);

        let voters = get_day_voters(&ctx, 20240101).unwrap();
        assert_eq!(voters.len(), 1, "one entry per voter");
        assert_eq!(voters[0].0, VAL_X);
        assert_eq!(voters[0].1, 2, "a repeat inside one block is not a vote");
    });
}

#[test]
fn get_day_voters_empty_when_no_metadata() {
    with_genesis_block(|ctx| {
        let voters = get_day_voters(&ctx, 20240101).unwrap();
        assert!(voters.is_empty());
    });
}

#[test]
fn votes_and_fees_after_the_day_is_sealed_are_refused() {
    with_funded_genesis_block(100, |ctx| {
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
fn validator_allocation_preserves_native_fee_precision_and_bounds() {
    use crate::test_support::{with_block, GENESIS_TS, SECONDS_PER_DAY};
    use outbe_primitives::units::checked_protocol_to_native;
    with_block(20, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
        let day = 20240101;
        seal_votes_and_fees(&ctx, day).unwrap();
        let rewards = ctx.storage.contract::<Rewards>();
        let share = U256::from(10);
        let native = checked_protocol_to_native(share).unwrap();
        let unit = checked_protocol_to_native(U256::from(1)).unwrap();
        for (fees, expected_topup) in [
            (U256::ZERO, share),
            (U256::from(1), U256::from(9)),
            (unit, U256::from(9)),
            (unit + U256::from(1), U256::from(8)),
            (native, U256::ZERO),
            (native + unit, U256::ZERO),
        ] {
            rewards.day_fee_sum_native.write(&day, fees).unwrap();
            let split = allocate_day_validator_rewards(&ctx, day, share).unwrap();
            assert_eq!(split.topup, expected_topup);
            assert_eq!(split.topup + split.fee_covered, share);
        }
        assert_eq!(
            allocate_day_validator_rewards(&ctx, day, U256::ZERO).unwrap(),
            ValidatorRewardAllocation {
                topup: U256::ZERO,
                fee_covered: U256::ZERO
            }
        );
        let error = allocate_day_validator_rewards(&ctx, day, U256::MAX).unwrap_err();
        let selector = alloy_primitives::keccak256("NativeValidatorBudgetOverflow()");
        assert!(
            matches!(error, outbe_primitives::error::PrecompileError::RevertBytes(bytes)
            if bytes.as_ref() == &selector[..4])
        );
    });
}

#[test]
fn sealed_inputs_require_sealed_completed_day_and_complete_voting() {
    use crate::test_support::{with_block, GENESIS_TS, SECONDS_PER_DAY};
    with_block(20, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
        let day = 20240101;
        assert!(sealed_day_inputs(&ctx, day).is_err());
        seal_votes_and_fees(&ctx, day).unwrap();
        let rewards = ctx.storage.contract::<Rewards>();
        rewards.day_votes_sealed_at_block.write(&day, 20).unwrap();
        assert!(sealed_day_inputs(&ctx, day).is_err());
        assert!(allocate_day_validator_rewards(&ctx, day, U256::ZERO).is_err());
        rewards.day_votes_sealed_at_block.write(&day, 19).unwrap();
        assert_eq!(sealed_day_inputs(&ctx, day).unwrap(), (0, U256::ZERO));
        assert!(reward_voter_at(&ctx, day, 0).is_err());
        seal_votes_and_fees(&ctx, 20240102).unwrap();
        assert!(sealed_day_inputs(&ctx, 20240102).is_err());
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
        assert_eq!(sealed_day_inputs(&ctx, day).unwrap(), (2, U256::from(3)));
        assert_eq!(
            reward_voter_at(&ctx, day, 0).unwrap(),
            (VAL_X, U256::from(2))
        );
        assert_eq!(
            reward_voter_at(&ctx, day, 1).unwrap(),
            (VAL_Y, U256::from(1))
        );

        let rewards = ctx.storage.contract::<Rewards>();
        // A later day: votes for a sealed day are refused before any overflow check.
        let other_day = 20240103;
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
        assert_eq!(
            rewards
                .day_voter_at
                .get_nested(&other_day)
                .read(&u32::MAX)
                .unwrap(),
            Address::ZERO
        );
    });
}

#[test]
fn cleanup_resumes_in_bounded_slices_and_keeps_the_day_sealed() {
    use crate::finalized_metadata_hook::record_reward_participation;
    use crate::test_support::{block_ctx, with_block, GENESIS_TS, SECONDS_PER_DAY};
    with_block(20, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
        let day = 20240101;
        for i in 1..=300u64 {
            record_reward_participation(
                &ctx,
                FB_HASH_A,
                day,
                Address::from_word(U256::from(i).into()),
            )
            .unwrap();
        }
        let rewards = ctx.storage.contract::<Rewards>();
        rewards
            .day_fee_sum_native
            .write(&day, U256::from(17))
            .unwrap();
        assert!(enqueue_day_cleanup(&ctx, day).is_err());
        seal_votes_and_fees(&ctx, day).unwrap();
        enqueue_day_cleanup(&ctx, day).unwrap();
        enqueue_day_cleanup(&ctx, day).unwrap();
        assert_eq!(rewards.day_cleanup_tail.read().unwrap(), 1);
        assert_eq!(
            sealed_day_inputs(&ctx, day).unwrap(),
            (300, U256::from(300))
        );
        cleanup(&ctx).unwrap();
        assert_eq!(rewards.cleanup_next_index.read(&day).unwrap(), 256);
        assert_eq!(
            rewards.day_fee_sum_native.read(&day).unwrap(),
            U256::from(17)
        );
        cleanup(&ctx).unwrap();
        assert_eq!(rewards.cleanup_next_index.read(&day).unwrap(), 256);
        let next = BlockRuntimeContext::new(
            block_ctx(21, GENESIS_TS + SECONDS_PER_DAY),
            ctx.storage.clone(),
        );
        cleanup(&next).unwrap();
        assert_eq!(rewards.day_cleanup_head.read().unwrap(), 1);
        assert_eq!(sealed_day_inputs(&next, day).unwrap(), (0, U256::ZERO));
        assert_eq!(rewards.day_fee_sum_native.read(&day).unwrap(), U256::ZERO);
        assert_eq!(
            rewards.day_voter_at.get_nested(&day).read(&299).unwrap(),
            Address::ZERO
        );
        enqueue_day_cleanup(&next, day).unwrap();
        assert_eq!(rewards.day_cleanup_tail.read().unwrap(), 1);
        assert!(record_reward_participation(&next, FB_HASH_B, day, VAL_X).is_err());
    });
}

#[test]
fn sealed_voter_reader_rejects_missing_source_entries() {
    use crate::test_support::{with_block, GENESIS_TS, SECONDS_PER_DAY};
    with_block(20, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
        let day = 20240101;
        let rewards = ctx.storage.contract::<Rewards>();
        rewards.day_voter_count.write(&day, 1).unwrap();
        rewards.day_total_votes.write(&day, U256::from(1)).unwrap();
        seal_votes_and_fees(&ctx, day).unwrap();
        assert!(reward_voter_at(&ctx, day, 0)
            .unwrap_err()
            .to_string()
            .contains("entry is missing"));
        rewards
            .day_voter_at
            .get_nested(&day)
            .write(&0, VAL_X)
            .unwrap();
        assert!(reward_voter_at(&ctx, day, 0)
            .unwrap_err()
            .to_string()
            .contains("entry is missing"));
        rewards.day_total_votes.write(&day, U256::ZERO).unwrap();
        assert_eq!(reward_voter_at(&ctx, day, 0).unwrap(), (VAL_X, U256::ZERO));
        rewards.day_total_votes.write(&day, U256::from(1)).unwrap();
        rewards.day_votes.get_nested(&day).write(&VAL_X, 1).unwrap();
        assert_eq!(
            reward_voter_at(&ctx, day, 0).unwrap(),
            (VAL_X, U256::from(1))
        );
    });
}

#[test]
fn cleanup_bounds_empty_headers_and_rolls_back_corrupt_pass() {
    use crate::test_support::{block_ctx, with_block, GENESIS_TS, SECONDS_PER_DAY};
    with_block(0, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
        let rewards = ctx.storage.contract::<Rewards>();
        for day in 20231225..=20231230 {
            seal_votes_and_fees(&ctx, day).unwrap();
            enqueue_day_cleanup(&ctx, day).unwrap();
        }
        // Corrupt the second header: the first deletion must also roll back.
        rewards.day_cleanup_day_at.write(&1, 0).unwrap();
        assert!(cleanup(&ctx).is_err());
        assert_eq!(rewards.day_cleanup_head.read().unwrap(), 0);
        assert_eq!(rewards.day_cleanup_day_at.read(&0).unwrap(), 20231225);
        assert!(!rewards.cleanup_processed_initialized.read().unwrap());
        rewards.day_cleanup_day_at.write(&1, 20231226).unwrap();
        cleanup(&ctx).unwrap();
        assert_eq!(rewards.day_cleanup_head.read().unwrap(), 4);
        cleanup(&ctx).unwrap();
        assert_eq!(rewards.day_cleanup_head.read().unwrap(), 4);
        let next = BlockRuntimeContext::new(
            block_ctx(1, GENESIS_TS + SECONDS_PER_DAY),
            ctx.storage.clone(),
        );
        cleanup(&next).unwrap();
        assert_eq!(rewards.day_cleanup_head.read().unwrap(), 6);
        rewards.day_cleanup_tail.write(u64::MAX).unwrap();
        seal_votes_and_fees(&next, 20231231).unwrap();
        assert!(enqueue_day_cleanup(&next, 20231231).is_err());
        assert!(!rewards.cleanup_scheduled.read(&20231231).unwrap());
    });
}

#[test]
fn seal_votes_and_fees_moves_last_sealed_day_forward_only() {
    with_genesis_block(|ctx| {
        assert_eq!(get_last_sealed_day(&ctx.storage).unwrap(), 0);
        seal_votes_and_fees(&ctx, 20240101).unwrap();
        assert_eq!(get_last_sealed_day(&ctx.storage).unwrap(), 20240101);
        for day in [20231231, 20240101] {
            let err = seal_votes_and_fees(&ctx, day).unwrap_err();
            assert!(err.to_string().contains("is not after the last sealed day"));
        }
        assert_eq!(get_last_sealed_day(&ctx.storage).unwrap(), 20240101);
        // Days need not be consecutive.
        seal_votes_and_fees(&ctx, 20240103).unwrap();
        assert_eq!(get_last_sealed_day(&ctx.storage).unwrap(), 20240103);
    });
}

#[test]
fn is_voting_complete_waits_for_the_block_after_the_last_vote_seal() {
    use crate::test_support::{block_ctx, with_block, GENESIS_TS, SECONDS_PER_DAY};
    with_block(20, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
        let day = 20240101;
        assert!(is_voting_complete(&ctx, day).unwrap());
        let rewards = ctx.storage.contract::<Rewards>();
        rewards.day_votes_sealed_at_block.write(&day, 21).unwrap();
        assert!(!is_voting_complete(&ctx, day).unwrap());
        let at_seal = BlockRuntimeContext::new(
            block_ctx(21, GENESIS_TS + SECONDS_PER_DAY),
            ctx.storage.clone(),
        );
        assert!(!is_voting_complete(&at_seal, day).unwrap());
        let after_seal = BlockRuntimeContext::new(
            block_ctx(22, GENESIS_TS + SECONDS_PER_DAY),
            ctx.storage.clone(),
        );
        assert!(is_voting_complete(&after_seal, day).unwrap());
    });
}
