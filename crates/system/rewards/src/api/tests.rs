use super::*;

use crate::test_support::{
    meta_with_hash, record_genesis_day_parent, with_funded_genesis_block, with_genesis_block,
    FB_HASH_A, FB_HASH_B, VAL_X, VAL_Y, VAL_Z,
};

#[test]
fn read_daily_fee_sum_raw_returns_zero_when_unrecorded() {
    with_genesis_block(|ctx| {
        assert_eq!(read_daily_fee_sum_raw(&ctx, 20240101).unwrap(), U256::ZERO);
    });
}

#[test]
fn read_daily_fee_sum_raw_round_trips_after_finalized_metadata() {
    with_funded_genesis_block(300, |ctx| {
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_A, 1), 101, &[VAL_X, VAL_Y]);
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_B, 2), 199, &[VAL_X, VAL_Y]);

        // 101 + 199 = 300 raw.
        assert_eq!(
            read_daily_fee_sum_raw(&ctx, 20240101).unwrap(),
            U256::from(300u64)
        );
        // Untouched day stays zero.
        assert_eq!(read_daily_fee_sum_raw(&ctx, 20240102).unwrap(), U256::ZERO);
    });
}

#[test]
fn read_voters_for_day_orders_by_first_seen() {
    with_funded_genesis_block(400, |ctx| {
        // FB_HASH_A: Y first, then X (first-seen-on-day = Y, X)
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_A, 1), 100, &[VAL_Y, VAL_X]);
        // FB_HASH_B (same day): Z is new, X already seen
        record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_B, 2), 100, &[VAL_X, VAL_Z]);

        let voters = read_voters_for_day(&ctx, 20240101).unwrap();
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

        let voters = read_voters_for_day(&ctx, 20240101).unwrap();
        assert_eq!(voters.len(), 1, "one entry per voter");
        assert_eq!(voters[0].0, VAL_X);
        assert_eq!(voters[0].1, 2, "a repeat inside one block is not a vote");
    });
}

#[test]
fn read_voters_for_day_empty_when_no_metadata() {
    with_genesis_block(|ctx| {
        let voters = read_voters_for_day(&ctx, 20240101).unwrap();
        assert!(voters.is_empty());
    });
}

#[test]
fn participation_after_the_day_is_closed_is_refused() {
    with_funded_genesis_block(100, |ctx| {
        close_day_participation(&ctx, 20240101).unwrap();
        let err = crate::finalized_metadata_hook::on_finalized_metadata(
            &ctx,
            &meta_with_hash(FB_HASH_A, 1),
            U256::from(100u64),
            crate::test_support::GENESIS_TS,
            &[VAL_X],
        )
        .unwrap_err();
        assert!(err.to_string().contains("after its day was credited"));
    });
}
