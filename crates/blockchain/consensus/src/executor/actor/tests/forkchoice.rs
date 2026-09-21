use super::*;

#[test]
fn update_head_returns_new_state() {
    let genesis = B256::repeat_byte(0x01);
    let state = LastCanonicalized::new(genesis);

    let state = state.update_head(Height::new(12), Digest(B256::repeat_byte(0x0C)));
    assert_eq!(state.head_height, Height::new(12));
    assert_eq!(state.forkchoice.head_block_hash, B256::repeat_byte(0x0C));

    let state = state.update_head(Height::new(11), Digest(B256::repeat_byte(0x0B)));
    assert_eq!(state.head_height, Height::new(11));
    assert_eq!(state.forkchoice.head_block_hash, B256::repeat_byte(0x0B));
}

#[test]
fn update_head_flip_flop_at_same_height_is_observable() {
    let genesis = B256::repeat_byte(0x01);
    let state = LastCanonicalized::new(genesis);

    let state = state.update_head(Height::new(7), Digest(B256::repeat_byte(0x70)));
    let flipped = state.update_head(Height::new(7), Digest(B256::repeat_byte(0x71)));

    assert_eq!(flipped.head_height, Height::new(7));
    assert_eq!(flipped.forkchoice.head_block_hash, B256::repeat_byte(0x71));
}

#[test]
fn update_finalized_same_height_different_digest_is_noop() {
    let genesis = B256::repeat_byte(0x01);
    let state = LastCanonicalized::new(genesis);
    let state = state.update_finalized(Height::new(9), Digest(B256::repeat_byte(0x90)));

    let conflicting = state.update_finalized(Height::new(9), Digest(B256::repeat_byte(0x99)));

    assert_eq!(conflicting.finalized_height, Height::new(9));
    assert_eq!(
        conflicting.forkchoice.finalized_block_hash,
        B256::repeat_byte(0x90)
    );
}

#[test]
fn update_finalized_same_height_conflict_does_not_mutate_head() {
    let genesis = B256::repeat_byte(0x01);
    let state = LastCanonicalized::new(genesis);
    let finalized_hash = B256::repeat_byte(0x09);
    let state = state.update_finalized(Height::new(9), Digest(finalized_hash));
    assert_eq!(state.forkchoice.head_block_hash, finalized_hash);

    let conflicting = state.update_finalized(Height::new(9), Digest(B256::repeat_byte(0x99)));
    assert_eq!(conflicting.forkchoice.finalized_block_hash, finalized_hash);
    assert_eq!(conflicting.forkchoice.head_block_hash, finalized_hash);
}

#[test]
fn update_finalized_lower_height_is_noop() {
    let genesis = B256::repeat_byte(0x01);
    let state = LastCanonicalized::new(genesis);
    let state = state.update_finalized(Height::new(20), Digest(B256::repeat_byte(0x20)));

    let stale = state.update_finalized(Height::new(10), Digest(B256::repeat_byte(0x10)));

    assert_eq!(stale.finalized_height, Height::new(20));
    assert_eq!(
        stale.forkchoice.finalized_block_hash,
        B256::repeat_byte(0x20)
    );
}

#[test]
fn update_head_rejects_below_finalized() {
    let genesis = B256::repeat_byte(0x01);
    let state = LastCanonicalized::new(genesis);
    let state = state.update_finalized(Height::new(5), Digest(B256::repeat_byte(0x05)));

    let same = state.update_head(Height::new(4), Digest(B256::repeat_byte(0x44)));
    assert_eq!(same.head_height, state.head_height);
    assert_eq!(
        same.forkchoice.head_block_hash,
        state.forkchoice.head_block_hash
    );
}

#[test]
fn update_head_rejects_finalized_height_conflicting_hash() {
    let genesis = B256::repeat_byte(0x01);
    let state = LastCanonicalized::new(genesis);
    let finalized_hash = B256::repeat_byte(0x05);
    let state = state.update_finalized(Height::new(5), Digest(finalized_hash));
    let state = state.update_head(Height::new(6), Digest(B256::repeat_byte(0x06)));

    let conflicting = B256::repeat_byte(0x55);
    assert_ne!(conflicting, finalized_hash);
    let rejected = state.update_head(Height::new(5), Digest(conflicting));
    assert_eq!(rejected.head_height, Height::new(6));
    assert_eq!(rejected.forkchoice.head_block_hash, B256::repeat_byte(0x06));
}

#[test]
fn update_head_rolls_back_to_finalized_hash() {
    let genesis = B256::repeat_byte(0x01);
    let state = LastCanonicalized::new(genesis);
    let finalized_hash = B256::repeat_byte(0x05);
    let state = state.update_finalized(Height::new(5), Digest(finalized_hash));
    let state = state.update_head(Height::new(6), Digest(B256::repeat_byte(0x06)));
    assert_eq!(state.head_height, Height::new(6));

    let rolled_back = state.update_head(Height::new(5), Digest(finalized_hash));
    assert_eq!(rolled_back.head_height, Height::new(5));
    assert_eq!(rolled_back.forkchoice.head_block_hash, finalized_hash);
}

#[test]
fn fresh_bootstrap_seeds_from_genesis() {
    let genesis = B256::repeat_byte(0xAA);
    let state = LastCanonicalized::from_recovered(genesis, 0, genesis);

    assert_eq!(state.finalized_height, Height::zero());
    assert_eq!(state.head_height, Height::zero());
    assert_eq!(state.forkchoice.finalized_block_hash, genesis);
    assert_eq!(state.forkchoice.head_block_hash, genesis);
}

#[test]
fn restart_seeds_from_recovered_finalized() {
    let genesis = B256::repeat_byte(0xAA);
    let finalized = B256::repeat_byte(0xBB);
    let state = LastCanonicalized::from_recovered(genesis, 100, finalized);

    assert_eq!(state.finalized_height, Height::new(100));
    assert_eq!(state.head_height, Height::new(100));
    assert_eq!(state.forkchoice.finalized_block_hash, finalized);
    assert_eq!(state.forkchoice.head_block_hash, finalized);
    assert_eq!(state.forkchoice.safe_block_hash, finalized);
}

#[test]
fn update_finalized_does_not_regress() {
    let genesis = B256::repeat_byte(0xAA);
    let finalized = B256::repeat_byte(0xBB);
    let state = LastCanonicalized::from_recovered(genesis, 100, finalized);

    let same = state.update_finalized(Height::new(50), Digest(B256::repeat_byte(0xCC)));
    assert_eq!(same.finalized_height, Height::new(100));

    let newer = B256::repeat_byte(0xDD);
    let advanced = state.update_finalized(Height::new(101), Digest(newer));
    assert_eq!(advanced.finalized_height, Height::new(101));
    assert_eq!(advanced.forkchoice.finalized_block_hash, newer);
}

#[test]
fn immutable_update_does_not_mutate_original() {
    let genesis = B256::repeat_byte(0x01);
    let original = LastCanonicalized::new(genesis);

    let updated = original.update_head(Height::new(10), Digest(B256::repeat_byte(0x0A)));
    assert_ne!(original, updated);
    assert_eq!(original.head_height, Height::zero());
    assert_eq!(updated.head_height, Height::new(10));
}
