//! Slot bookkeeping for poll-style triggers.

use outbe_cycle::triggers::{
    active_triggers, last_fire_at, next_fire_at, TriggerId, ACTIVE_TRIGGERS,
};

const PERIOD: u64 = 600;
const OFFSET: u64 = 0;

#[test]
fn a_coalesced_slot_cannot_fire_twice_at_one_timestamp() {
    // The gap a chain restart leaves: the last recorded slot is far behind.
    let stale = 1_700_000_000_u64;
    let block_ts = stale + 37 * PERIOD + 41;

    let recorded = last_fire_at(PERIOD, OFFSET, block_ts);
    assert!(recorded <= block_ts);
    assert!(block_ts - recorded < PERIOD);

    // The whole point: a second block at the same timestamp finds nothing due.
    assert!(next_fire_at(PERIOD, OFFSET, recorded) > block_ts);
}

#[test]
fn replaying_slots_one_per_block_is_what_coalescing_replaces() {
    let stale = 1_700_000_000_u64;
    let block_ts = stale + 3 * PERIOD;

    // Without coalescing each block advances a single slot, so the same
    // timestamp keeps firing until the backlog drains.
    let first = next_fire_at(PERIOD, OFFSET, stale);
    assert!(first <= block_ts);
    assert!(next_fire_at(PERIOD, OFFSET, first) <= block_ts);

    // With it, one firing settles the gap.
    assert!(next_fire_at(PERIOD, OFFSET, last_fire_at(PERIOD, OFFSET, block_ts)) > block_ts);
}

#[test]
fn a_slot_boundary_records_itself() {
    let boundary = 1_700_000_400_u64;
    assert_eq!(boundary % PERIOD, 0);
    assert_eq!(last_fire_at(PERIOD, OFFSET, boundary), boundary);
    assert_eq!(last_fire_at(PERIOD, OFFSET, boundary + PERIOD - 1), boundary);
}

#[test]
fn a_timestamp_before_the_first_slot_yields_the_offset() {
    assert_eq!(last_fire_at(PERIOD, 5_000, 4_999), 5_000);
}

#[test]
fn only_the_clearing_poll_coalesces() {
    for spec in ACTIVE_TRIGGERS {
        assert_eq!(
            spec.coalesces_backlog,
            spec.id == TriggerId::AuctionClearing.as_u32(),
            "{} must not change its backlog policy",
            spec.label
        );
    }
}

#[test]
fn the_clearing_poll_runs_after_the_stage_schedule() {
    let position = |id: u32| {
        ACTIVE_TRIGGERS
            .iter()
            .position(|spec| spec.id == id)
            .expect("trigger registered")
    };
    assert!(
        position(TriggerId::AuctionClearing.as_u32())
            > position(TriggerId::AuctionAdvance.as_u32()),
        "auction_clearing must dispatch after auction_advance so a day armed this slot can clear in it"
    );
}

#[test]
fn the_genesis_interval_leaves_the_clearing_poll_alone() {
    for interval in [10_u64, 600, 86_400] {
        let configured = active_triggers(interval);
        let clearing = configured
            .iter()
            .find(|spec| spec.id == TriggerId::AuctionClearing.as_u32())
            .expect("clearing trigger registered");
        assert_eq!(clearing.period_seconds, PERIOD);
        assert_eq!(clearing.start_offset_seconds, OFFSET);
        assert!(!clearing.requires_accounting_window);
    }
}
