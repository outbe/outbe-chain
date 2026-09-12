use super::{
    apply_unix_time_offset_millis, clamp_proposed_timestamp_millis, proposal_timestamp_millis,
};
use alloy_primitives::Bytes;
use outbe_primitives::OutbeHeader;
use reth_ethereum::{primitives::SealedBlock, Block};

const BAND: u64 = 60 * 60 * 1_000; // 1h, matches MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS
const MIN: u64 = 1_000; // matches MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS

#[test]
fn injected_clock_offset_is_explicit_and_checked() {
    assert_eq!(
        apply_unix_time_offset_millis(1_000_000, 60).unwrap(),
        1_060_000
    );
    assert_eq!(
        apply_unix_time_offset_millis(1_000_000, -60).unwrap(),
        940_000
    );
    assert!(apply_unix_time_offset_millis(0, -1).is_err());
}

#[test]
fn genesis_child_uses_wall_clock_not_band() {
    // Regression: at genesis the finalization_view is unseeded (parent==0).
    // The real wall-clock (~=1.78e12 ms) must NOT be clamped to 0+band, which
    // would put block 1 before the genesis timestamp and stall the chain. The
    // min-advance lower bound is also skipped at genesis (monotonic-only).
    let now = 1_781_255_987_000u64;
    assert_eq!(clamp_proposed_timestamp_millis(0, now, BAND, MIN), now);
}

#[test]
fn real_parent_applies_band() {
    let parent = 1_781_255_987_000u64;
    // within band, above min advance -> wall clock used
    assert_eq!(
        clamp_proposed_timestamp_millis(parent, parent + 2_000, BAND, MIN),
        parent + 2_000
    );
    // far-future now -> capped at parent + band
    assert_eq!(
        clamp_proposed_timestamp_millis(parent, parent + 10 * BAND, BAND, MIN),
        parent + BAND
    );
}

#[test]
fn lagging_clock_clamps_up_to_min_advance() {
    // when the proposer's clock has not advanced `MIN` past the parent
    // (or is in the past), the timestamp is clamped UP to `parent + MIN` so
    // the block satisfies the validator minimum-advance rule and is accepted,
    // rather than emitting `parent + 1` which validators would now reject.
    let parent = 1_781_255_987_000u64;
    // now in the past -> parent + MIN (not parent + 1).
    assert_eq!(
        clamp_proposed_timestamp_millis(parent, parent - 5, BAND, MIN),
        parent + MIN
    );
    // now between parent+1 and parent+MIN -> clamped up to parent + MIN.
    assert_eq!(
        clamp_proposed_timestamp_millis(parent, parent + 500, BAND, MIN),
        parent + MIN
    );
    // now exactly at the min-advance boundary -> unchanged.
    assert_eq!(
        clamp_proposed_timestamp_millis(parent, parent + MIN, BAND, MIN),
        parent + MIN
    );
}

#[test]
fn proposal_timestamp_is_derived_from_the_exact_parent_block() {
    let parent_timestamp = 1_781_255_987_000u64;
    let mut parent = Block::default();
    parent.header.number = 42;
    parent.header.timestamp = parent_timestamp / 1_000;
    parent.header.extra_data = Bytes::from_static(b"exact-parent");
    let parent = parent.map_header(OutbeHeader::new);
    let parent = super::ConsensusBlock::from_sealed(SealedBlock::seal_slow(parent));

    assert_eq!(
        proposal_timestamp_millis(Some(&parent), parent_timestamp + 10 * BAND, BAND, MIN,),
        parent_timestamp + BAND,
    );
}
