//! Unit tests for the parked-work cursor.

use crate::constants::MAX_PARKED_FAILURES_PER_FIRING;
use crate::parked::Cursor;

#[test]
fn the_cursor_only_passes_a_resolved_prefix() {
    let mut cursor = Cursor::new(0);
    cursor.resolved();
    cursor.at = 1;
    cursor.stuck();
    cursor.at = 2;
    cursor.resolved();

    // Index 1 stayed, so the next pass starts there and walks the rest again.
    assert_eq!(cursor.head, 1);
}

#[test]
fn a_run_of_failures_ends_the_pass() {
    let mut cursor = Cursor::new(0);
    for _ in 0..MAX_PARKED_FAILURES_PER_FIRING {
        assert!(!cursor.spent());
        cursor.stuck();
    }
    assert!(cursor.spent());

    // One success in between is enough to keep going: an empty float is the run we stop on.
    let mut mixed = Cursor::new(0);
    mixed.stuck();
    mixed.resolved();
    mixed.stuck();
    assert!(!mixed.spent());
}
