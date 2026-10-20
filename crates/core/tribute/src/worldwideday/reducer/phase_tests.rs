use super::tests::{
    plan, projection, projection_for_day_type, FORMING_END, FORMING_START, LOOKBACK_END,
    OFFERING_END, PROCESS_AT,
};
use super::*;
use crate::worldwideday::aggregate::{WwdDayType, WwdMembership, WwdStatus};

#[test]
fn outer_wwd_state_event_matrix_is_exhaustive() {
    let statuses = [
        WwdStatus::Forming,
        WwdStatus::LookbackDelay,
        WwdStatus::Offering,
        WwdStatus::Waiting,
        WwdStatus::Ready,
        WwdStatus::Completed,
        WwdStatus::Failed,
    ];
    let events = [
        OuterWwdEvent::CreateDay,
        OuterWwdEvent::AdvanceDue {
            block_time: PROCESS_AT,
            retained_count: 0,
            admission_available: true,
            limit_final: true,
        },
        OuterWwdEvent::ProcessReady(ReadyDisposition::EmptyTributeDay),
        OuterWwdEvent::ProcessReady(ReadyDisposition::ZeroGratisAllocation),
        OuterWwdEvent::ProcessReady(ReadyDisposition::LysisCompleted),
        OuterWwdEvent::ProcessReady(ReadyDisposition::LysisFailed),
        OuterWwdEvent::EmergencyFail,
    ];

    for day_type in [WwdDayType::Green, WwdDayType::Red] {
        for status in statuses {
            for event in events {
                let actual =
                    reduce_outer_wwd(Some(&projection_for_day_type(status, day_type)), event);
                let expected = expected_transition_at_process_time(status, event);
                match (actual, expected) {
                    (Ok(actual), Some((target, membership, kind))) => {
                        assert_eq!(
                            actual.source(),
                            Some(status),
                            "day type {day_type:?}, state {status:?}, event {event:?}"
                        );
                        assert_eq!(
                            actual.target(),
                            target,
                            "day type {day_type:?}, state {status:?}, event {event:?}"
                        );
                        assert_eq!(
                            actual.membership_after(),
                            membership,
                            "day type {day_type:?}, state {status:?}, event {event:?}"
                        );
                        assert_eq!(
                            actual.kind(),
                            &kind,
                            "day type {day_type:?}, state {status:?}, event {event:?}"
                        );
                    }
                    (Err(_), None) => {}
                    (Ok(actual), None) => panic!(
                        "day type {day_type:?}, state {status:?}, event {event:?} \
                         unexpectedly accepted as {actual:?}"
                    ),
                    (Err(error), Some(expected)) => panic!(
                        "day type {day_type:?}, state {status:?}, event {event:?} \
                         rejected with {error}; expected {expected:?}"
                    ),
                }
            }
        }
    }

    for event in events {
        assert_eq!(
            reduce_outer_wwd(None, event).is_ok(),
            event == OuterWwdEvent::CreateDay,
            "absent state accepted {event:?}"
        );
    }
}

fn expected_transition_at_process_time(
    status: WwdStatus,
    event: OuterWwdEvent,
) -> Option<(WwdStatus, WwdMembership, OuterWwdTransitionKind)> {
    let (target, kind) = match event {
        OuterWwdEvent::CreateDay => (status, OuterWwdTransitionKind::Noop),
        OuterWwdEvent::AdvanceDue { .. } => match status {
            WwdStatus::Forming => (
                WwdStatus::Failed,
                OuterWwdTransitionKind::MissedOffering {
                    preceding_edges: vec![WwdAdvanceEdge::ResolveForming],
                },
            ),
            WwdStatus::LookbackDelay => (
                WwdStatus::Failed,
                OuterWwdTransitionKind::MissedOffering {
                    preceding_edges: Vec::new(),
                },
            ),
            WwdStatus::Offering => (
                WwdStatus::Ready,
                OuterWwdTransitionKind::Advance(vec![
                    WwdAdvanceEdge::CloseOffering,
                    WwdAdvanceEdge::BecomeReady,
                ]),
            ),
            WwdStatus::Waiting => (
                WwdStatus::Ready,
                OuterWwdTransitionKind::Advance(vec![WwdAdvanceEdge::BecomeReady]),
            ),
            WwdStatus::Ready | WwdStatus::Completed | WwdStatus::Failed => {
                (status, OuterWwdTransitionKind::Noop)
            }
        },
        OuterWwdEvent::ProcessReady(disposition) if status == WwdStatus::Ready => {
            let target = match disposition {
                ReadyDisposition::EmptyTributeDay
                | ReadyDisposition::ZeroGratisAllocation
                | ReadyDisposition::LysisCompleted => WwdStatus::Completed,
                ReadyDisposition::LysisFailed => WwdStatus::Failed,
            };
            (target, OuterWwdTransitionKind::ProcessReady(disposition))
        }
        OuterWwdEvent::EmergencyFail if !status.is_terminal() => {
            (WwdStatus::Failed, OuterWwdTransitionKind::EmergencyFail)
        }
        OuterWwdEvent::EmergencyFail if status == WwdStatus::Failed => {
            (WwdStatus::Failed, OuterWwdTransitionKind::Noop)
        }
        OuterWwdEvent::ProcessReady(_) | OuterWwdEvent::EmergencyFail => return None,
    };
    let membership = if target.is_terminal() {
        WwdMembership::Closed
    } else {
        WwdMembership::Active
    };
    Some((target, membership, kind))
}

#[test]
fn reducer_is_exact_at_every_phase_boundary() {
    use WwdAdvanceEdge::{BecomeReady, CloseOffering, OpenOffering, ResolveForming};
    use WwdTransitionPlan::{Advance, MissedOffering, Noop};

    assert_eq!(plan(WwdStatus::Forming, FORMING_END - 1), Noop);
    assert_eq!(
        plan(WwdStatus::Forming, FORMING_END),
        Advance(vec![ResolveForming])
    );
    assert_eq!(
        plan(WwdStatus::Forming, FORMING_END + 1),
        Advance(vec![ResolveForming])
    );
    assert_eq!(
        plan(WwdStatus::Forming, LOOKBACK_END - 1),
        Advance(vec![ResolveForming])
    );
    assert_eq!(
        plan(WwdStatus::Forming, LOOKBACK_END),
        Advance(vec![ResolveForming, OpenOffering])
    );
    assert_eq!(
        plan(WwdStatus::Forming, LOOKBACK_END + 1),
        Advance(vec![ResolveForming, OpenOffering])
    );

    assert_eq!(plan(WwdStatus::LookbackDelay, LOOKBACK_END - 1), Noop);
    assert_eq!(
        plan(WwdStatus::LookbackDelay, LOOKBACK_END),
        Advance(vec![OpenOffering])
    );
    assert_eq!(
        plan(WwdStatus::LookbackDelay, LOOKBACK_END + 1),
        Advance(vec![OpenOffering])
    );

    assert_eq!(plan(WwdStatus::Offering, OFFERING_END - 1), Noop);
    assert_eq!(
        plan(WwdStatus::Offering, OFFERING_END),
        Advance(vec![CloseOffering])
    );
    assert_eq!(
        plan(WwdStatus::Offering, OFFERING_END + 1),
        Advance(vec![CloseOffering])
    );

    assert_eq!(plan(WwdStatus::Waiting, PROCESS_AT - 1), Noop);
    assert_eq!(
        plan(WwdStatus::Waiting, PROCESS_AT),
        Advance(vec![BecomeReady])
    );
    assert_eq!(
        plan(WwdStatus::Waiting, PROCESS_AT + 1),
        Advance(vec![BecomeReady])
    );

    assert_eq!(
        plan(WwdStatus::Forming, OFFERING_END - 1),
        Advance(vec![ResolveForming, OpenOffering])
    );
    assert_eq!(plan(WwdStatus::Forming, OFFERING_END), MissedOffering);
    assert_eq!(plan(WwdStatus::Forming, OFFERING_END + 1), MissedOffering);
    assert_eq!(
        plan(WwdStatus::LookbackDelay, OFFERING_END - 1),
        Advance(vec![OpenOffering])
    );
    assert_eq!(plan(WwdStatus::LookbackDelay, OFFERING_END), MissedOffering);
    assert_eq!(
        plan(WwdStatus::LookbackDelay, OFFERING_END + 1),
        MissedOffering
    );
}

#[test]
fn reducer_catches_up_without_synthesizing_a_missed_offering_after_it_opened() {
    use WwdAdvanceEdge::{BecomeReady, CloseOffering};
    use WwdTransitionPlan::Advance;

    assert_eq!(
        plan(WwdStatus::Offering, PROCESS_AT - 1),
        Advance(vec![CloseOffering])
    );
    assert_eq!(
        plan(WwdStatus::Offering, PROCESS_AT),
        Advance(vec![CloseOffering, BecomeReady])
    );
    assert_eq!(
        plan(WwdStatus::Offering, PROCESS_AT + 1),
        Advance(vec![CloseOffering, BecomeReady])
    );
}

#[test]
fn reducer_handles_zero_length_lookback_as_resolve_then_open() {
    let mut current = projection(WwdStatus::Forming);
    current.lookback_end = current.forming_end;

    assert_eq!(
        plan_wwd_advance(&current, current.forming_end).unwrap(),
        WwdTransitionPlan::Advance(vec![
            WwdAdvanceEdge::ResolveForming,
            WwdAdvanceEdge::OpenOffering,
        ])
    );
}

#[test]
fn reducer_rejects_backward_time_for_every_nonterminal_phase() {
    for (status, before) in [
        (WwdStatus::Forming, FORMING_START - 1),
        (WwdStatus::LookbackDelay, FORMING_END - 1),
        (WwdStatus::Offering, LOOKBACK_END - 1),
        (WwdStatus::Waiting, OFFERING_END - 1),
        (WwdStatus::Ready, PROCESS_AT - 1),
    ] {
        assert!(
            plan_wwd_advance(&projection(status), before).is_err(),
            "{status:?} accepted backward time {before}"
        );
    }
}

#[test]
fn reducer_state_time_region_table_covers_every_persisted_status() {
    use WwdAdvanceEdge::{BecomeReady, CloseOffering, OpenOffering, ResolveForming};
    use WwdTransitionPlan::{Advance, MissedOffering, Noop};

    let cases = vec![
        (WwdStatus::Forming, FORMING_START - 1, None),
        (WwdStatus::Forming, FORMING_START, Some(Noop)),
        (WwdStatus::Forming, FORMING_END - 1, Some(Noop)),
        (
            WwdStatus::Forming,
            FORMING_END,
            Some(Advance(vec![ResolveForming])),
        ),
        (
            WwdStatus::Forming,
            LOOKBACK_END - 1,
            Some(Advance(vec![ResolveForming])),
        ),
        (
            WwdStatus::Forming,
            LOOKBACK_END,
            Some(Advance(vec![ResolveForming, OpenOffering])),
        ),
        (
            WwdStatus::Forming,
            OFFERING_END - 1,
            Some(Advance(vec![ResolveForming, OpenOffering])),
        ),
        (WwdStatus::Forming, OFFERING_END, Some(MissedOffering)),
        (WwdStatus::LookbackDelay, FORMING_END - 1, None),
        (WwdStatus::LookbackDelay, FORMING_END, Some(Noop)),
        (WwdStatus::LookbackDelay, LOOKBACK_END - 1, Some(Noop)),
        (
            WwdStatus::LookbackDelay,
            LOOKBACK_END,
            Some(Advance(vec![OpenOffering])),
        ),
        (
            WwdStatus::LookbackDelay,
            OFFERING_END - 1,
            Some(Advance(vec![OpenOffering])),
        ),
        (WwdStatus::LookbackDelay, OFFERING_END, Some(MissedOffering)),
        (WwdStatus::Offering, LOOKBACK_END - 1, None),
        (WwdStatus::Offering, LOOKBACK_END, Some(Noop)),
        (WwdStatus::Offering, OFFERING_END - 1, Some(Noop)),
        (
            WwdStatus::Offering,
            OFFERING_END,
            Some(Advance(vec![CloseOffering])),
        ),
        (
            WwdStatus::Offering,
            PROCESS_AT - 1,
            Some(Advance(vec![CloseOffering])),
        ),
        (
            WwdStatus::Offering,
            PROCESS_AT,
            Some(Advance(vec![CloseOffering, BecomeReady])),
        ),
        (WwdStatus::Waiting, OFFERING_END - 1, None),
        (WwdStatus::Waiting, OFFERING_END, Some(Noop)),
        (WwdStatus::Waiting, PROCESS_AT - 1, Some(Noop)),
        (
            WwdStatus::Waiting,
            PROCESS_AT,
            Some(Advance(vec![BecomeReady])),
        ),
        (WwdStatus::Ready, PROCESS_AT - 1, None),
        (WwdStatus::Ready, PROCESS_AT, Some(Noop)),
        (WwdStatus::Completed, PROCESS_AT - 1, None),
        (WwdStatus::Completed, PROCESS_AT, Some(Noop)),
        (WwdStatus::Failed, OFFERING_END - 1, None),
        (WwdStatus::Failed, OFFERING_END, Some(Noop)),
    ];

    for (status, block_time, expected) in cases {
        let actual = plan_wwd_advance(&projection(status), block_time);
        match expected {
            Some(expected) => assert_eq!(
                actual.unwrap(),
                expected,
                "{status:?} at block timestamp {block_time}"
            ),
            None => assert!(
                actual.is_err(),
                "{status:?} accepted backward block timestamp {block_time}"
            ),
        }
    }
}

#[test]
fn reducer_is_a_noop_for_processing_and_terminal_states_at_valid_time() {
    for (status, valid_time) in [
        (WwdStatus::Ready, PROCESS_AT),
        (WwdStatus::Completed, PROCESS_AT),
        (WwdStatus::Failed, OFFERING_END),
    ] {
        assert_eq!(
            plan_wwd_advance(&projection(status), valid_time).unwrap(),
            WwdTransitionPlan::Noop
        );
        assert_eq!(
            plan_wwd_advance(&projection(status), valid_time + 1).unwrap(),
            WwdTransitionPlan::Noop
        );
    }
}

#[test]
fn reducer_accepts_each_phase_at_its_exact_lower_time_bound() {
    for (status, lower_bound, expected) in [
        (
            WwdStatus::LookbackDelay,
            FORMING_END,
            WwdTransitionPlan::Noop,
        ),
        (WwdStatus::Offering, LOOKBACK_END, WwdTransitionPlan::Noop),
        (WwdStatus::Waiting, OFFERING_END, WwdTransitionPlan::Noop),
    ] {
        assert_eq!(
            plan_wwd_advance(&projection(status), lower_bound).unwrap(),
            expected
        );
        assert_eq!(
            plan_wwd_advance(&projection(status), lower_bound + 1).unwrap(),
            expected
        );
    }
}

#[test]
fn reducer_rejects_backward_time_for_terminal_states() {
    for (status, before) in [
        (WwdStatus::Completed, PROCESS_AT - 1),
        (WwdStatus::Failed, OFFERING_END - 1),
    ] {
        assert!(
            plan_wwd_advance(&projection(status), before).is_err(),
            "{status:?} accepted backward time {before}"
        );
    }
}

#[test]
fn reducer_is_a_noop_for_processing_states_after_the_process_boundary() {
    let status = WwdStatus::Ready;
    assert_eq!(
        plan_wwd_advance(&projection(status), PROCESS_AT + 1).unwrap(),
        WwdTransitionPlan::Noop
    );
}
