use super::*;

#[test]
fn retry_schedule_is_scoped_by_journal_record_and_generation() {
    let now = Instant::now();
    let first = RetainedGcWorkId {
        key: B256::repeat_byte(0xa1),
        generation: 7,
    };
    let second = RetainedGcWorkId {
        key: B256::repeat_byte(0xa2),
        generation: 3,
    };
    let first_successor = RetainedGcWorkId {
        key: first.key,
        generation: first.generation + 1,
    };
    let mut schedule = RetainedGcRetrySchedule::default();

    // Different journal records remain independent even when their durable
    // records happen to refer to the same input lease.
    schedule.defer(first, now);
    assert!(schedule.is_eligible(second, now));
    schedule.defer(second, now);
    assert_eq!(schedule.deadlines.len(), 2);

    // Re-reading durable work drops the stale generation without carrying
    // its old deadline into the successor state.
    schedule.retain_pending(&[first_successor, second]);
    assert_eq!(schedule.deadlines.len(), 1);
    assert!(schedule.is_eligible(first_successor, now));
    assert!(!schedule.is_eligible(second, now + Duration::from_millis(100)));
    assert!(schedule.is_eligible(second, now + RETAINED_GC_RETRY_BACKOFF));

    schedule.clear(second);
    assert!(schedule.deadlines.is_empty());
}
