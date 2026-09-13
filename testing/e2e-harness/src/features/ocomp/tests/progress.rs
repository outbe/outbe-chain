use super::*;

#[test]
fn capacity_completion_window_returns_as_soon_as_every_validator_is_done() {
    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(300);

    assert_eq!(
        bounded_completion_decision(true, started, deadline),
        BoundedCompletionDecision::Complete
    );
    assert_eq!(
        bounded_completion_decision(false, started, deadline),
        BoundedCompletionDecision::Continue
    );
}

#[test]
fn capacity_completion_window_fails_only_when_the_shared_budget_expires() {
    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(300);

    assert_eq!(
        bounded_completion_decision(false, deadline, deadline),
        BoundedCompletionDecision::TimedOut
    );
    assert_eq!(
        bounded_completion_decision(true, deadline, deadline),
        BoundedCompletionDecision::Complete,
        "an observed completed result wins at the deadline boundary"
    );
}

#[test]
fn job_intent_schedule_waits_while_canonical_time_keeps_advancing() {
    let now = std::time::Instant::now();
    let progress_deadline = now + std::time::Duration::from_secs(120);

    assert_eq!(
        monotonic_progress_decision(200, 600, 199, now, progress_deadline),
        ProgressWaitDecision::Progressed
    );
    assert_eq!(
        monotonic_progress_decision(200, 600, 200, now, progress_deadline),
        ProgressWaitDecision::Waiting
    );
}

#[test]
fn job_intent_schedule_reaches_boundary_and_rejects_a_real_stall() {
    let now = std::time::Instant::now();

    assert_eq!(
        monotonic_progress_decision(600, 600, 599, now, now),
        ProgressWaitDecision::Reached,
        "the exact schedule boundary wins over the stall deadline"
    );
    assert_eq!(
        monotonic_progress_decision(599, 600, 599, now, now),
        ProgressWaitDecision::Stalled
    );
}
