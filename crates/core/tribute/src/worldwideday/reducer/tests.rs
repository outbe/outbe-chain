use alloy_primitives::U256;

use super::*;
use crate::worldwideday::aggregate::{WwdDayType, WwdMembership, WwdProjection, WwdStatus};

pub(super) const FORMING_START: u64 = 100;
pub(super) const FORMING_END: u64 = 200;
pub(super) const LOOKBACK_END: u64 = 300;
pub(super) const OFFERING_END: u64 = 400;
pub(super) const PROCESS_AT: u64 = 500;

pub(super) fn projection(status: WwdStatus) -> WwdProjection {
    projection_for_day_type(status, WwdDayType::Green)
}

pub(super) fn projection_for_day_type(status: WwdStatus, day_type: WwdDayType) -> WwdProjection {
    WwdProjection {
        worldwide_day: outbe_primitives::time::WorldwideDay::new(2026_0730),
        status,
        day_type,
        membership: if status.is_terminal() {
            WwdMembership::Closed
        } else {
            WwdMembership::Active
        },
        forming_start: FORMING_START,
        forming_end: FORMING_END,
        lookback_end: LOOKBACK_END,
        offering_end: OFFERING_END,
        scheduled_process_time: PROCESS_AT,
        metadosis_limit_minor: U256::from(10),
        previous_vwap_minor: U256::ZERO,
        current_vwap_minor: U256::ZERO,
    }
}

pub(super) fn plan(status: WwdStatus, block_time: u64) -> WwdTransitionPlan {
    plan_wwd_advance(&projection(status), block_time).unwrap()
}

#[test]
fn outer_reducer_emergency_fail_is_broad_but_terminal_safe() {
    for status in [
        WwdStatus::Forming,
        WwdStatus::LookbackDelay,
        WwdStatus::Offering,
        WwdStatus::Waiting,
        WwdStatus::Ready,
    ] {
        let transition =
            reduce_outer_wwd(Some(&projection(status)), OuterWwdEvent::EmergencyFail).unwrap();
        assert_eq!(transition.source(), Some(status));
        assert_eq!(transition.target(), WwdStatus::Failed);
        assert_eq!(transition.membership_after(), WwdMembership::Closed);
        assert_eq!(transition.kind(), &OuterWwdTransitionKind::EmergencyFail);
    }

    let failed = reduce_outer_wwd(
        Some(&projection(WwdStatus::Failed)),
        OuterWwdEvent::EmergencyFail,
    )
    .unwrap();
    assert_eq!(failed.target(), WwdStatus::Failed);
    assert_eq!(failed.kind(), &OuterWwdTransitionKind::Noop);

    assert!(reduce_outer_wwd(
        Some(&projection(WwdStatus::Completed)),
        OuterWwdEvent::EmergencyFail,
    )
    .is_err());
}

#[test]
fn outer_reducer_covers_creation_advance_capacity_and_ready_outcomes() {
    let created = reduce_outer_wwd(None, OuterWwdEvent::CreateDay).unwrap();
    assert_eq!(created.source(), None);
    assert_eq!(created.target(), WwdStatus::Forming);
    assert_eq!(created.membership_after(), WwdMembership::Active);
    assert_eq!(created.kind(), &OuterWwdTransitionKind::Created);

    let existing = projection(WwdStatus::Offering);
    let duplicate = reduce_outer_wwd(Some(&existing), OuterWwdEvent::CreateDay).unwrap();
    assert_eq!(duplicate.target(), WwdStatus::Offering);
    assert_eq!(duplicate.kind(), &OuterWwdTransitionKind::Noop);

    let forming = projection(WwdStatus::Forming);
    let missed = reduce_outer_wwd(
        Some(&forming),
        OuterWwdEvent::AdvanceDue {
            block_time: OFFERING_END,
            retained_count: 0,
            admission_available: true,
            limit_final: true,
        },
    )
    .unwrap();
    assert_eq!(missed.target(), WwdStatus::Failed);
    assert_eq!(missed.membership_after(), WwdMembership::Closed);
    assert_eq!(
        missed.kind(),
        &OuterWwdTransitionKind::MissedOffering {
            preceding_edges: vec![WwdAdvanceEdge::ResolveForming],
        }
    );

    let waiting = projection(WwdStatus::Waiting);
    let forfeited = reduce_outer_wwd(
        Some(&waiting),
        OuterWwdEvent::AdvanceDue {
            block_time: PROCESS_AT,
            retained_count: MAX_RETAINED_WWDS,
            admission_available: true,
            limit_final: true,
        },
    )
    .unwrap();
    assert_eq!(forfeited.target(), WwdStatus::Failed);
    assert_eq!(forfeited.membership_after(), WwdMembership::Closed);
    assert_eq!(
        forfeited.kind(),
        &OuterWwdTransitionKind::CapacityForfeiture {
            preceding_edges: Vec::new(),
        }
    );

    let offering = projection(WwdStatus::Offering);
    let forfeited_after_close = reduce_outer_wwd(
        Some(&offering),
        OuterWwdEvent::AdvanceDue {
            block_time: PROCESS_AT,
            retained_count: MAX_RETAINED_WWDS,
            admission_available: true,
            limit_final: true,
        },
    )
    .unwrap();
    assert_eq!(forfeited_after_close.target(), WwdStatus::Failed);
    assert_eq!(
        forfeited_after_close.kind(),
        &OuterWwdTransitionKind::CapacityForfeiture {
            preceding_edges: vec![WwdAdvanceEdge::CloseOffering],
        }
    );

    assert!(matches!(
        reduce_outer_wwd(
            Some(&waiting),
            OuterWwdEvent::AdvanceDue {
                block_time: PROCESS_AT,
                retained_count: MAX_RETAINED_WWDS + 1,
                admission_available: true,
                limit_final: true,
            },
        ),
        Err(outbe_primitives::error::PrecompileError::Revert(_))
    ));

    let waiting_without_admission = reduce_outer_wwd(
        Some(&waiting),
        OuterWwdEvent::AdvanceDue {
            block_time: PROCESS_AT,
            retained_count: 0,
            admission_available: false,
            limit_final: true,
        },
    )
    .unwrap();
    assert_eq!(waiting_without_admission.target(), WwdStatus::Waiting);
    assert_eq!(
        waiting_without_admission.kind(),
        &OuterWwdTransitionKind::Noop
    );

    let offering_without_admission = reduce_outer_wwd(
        Some(&offering),
        OuterWwdEvent::AdvanceDue {
            block_time: PROCESS_AT,
            retained_count: 0,
            admission_available: false,
            limit_final: true,
        },
    )
    .unwrap();
    assert_eq!(offering_without_admission.target(), WwdStatus::Waiting);
    assert_eq!(
        offering_without_admission.kind(),
        &OuterWwdTransitionKind::Advance(vec![WwdAdvanceEdge::CloseOffering])
    );

    let ready = projection(WwdStatus::Ready);
    for (disposition, target) in [
        (ReadyDisposition::EmptyTributeDay, WwdStatus::Completed),
        (ReadyDisposition::ZeroGratisAllocation, WwdStatus::Completed),
        (ReadyDisposition::LysisCompleted, WwdStatus::Completed),
        (ReadyDisposition::LysisFailed, WwdStatus::Failed),
    ] {
        let transition =
            reduce_outer_wwd(Some(&ready), OuterWwdEvent::ProcessReady(disposition)).unwrap();
        assert_eq!(transition.target(), target);
        assert_eq!(
            transition.membership_after(),
            if target.is_terminal() {
                WwdMembership::Closed
            } else {
                WwdMembership::Active
            }
        );
        assert_eq!(
            transition.kind(),
            &OuterWwdTransitionKind::ProcessReady(disposition)
        );
    }

    for disposition in [
        ReadyDisposition::LysisCompleted,
        ReadyDisposition::LysisFailed,
    ] {
        assert!(
            reduce_outer_wwd(Some(&waiting), OuterWwdEvent::ProcessReady(disposition)).is_err()
        );
    }
}

#[test]
fn final_zero_limit_turns_every_opening_advance_into_missed_offering() {
    use WwdAdvanceEdge::{BecomeReady, CloseOffering, OpenOffering, ResolveForming};
    let advance = |status: WwdStatus, limit: U256, block_time: u64, limit_final: bool| {
        let mut current = projection(status);
        current.metadosis_limit_minor = limit;
        reduce_outer_wwd(
            Some(&current),
            OuterWwdEvent::AdvanceDue {
                block_time,
                retained_count: 0,
                admission_available: true,
                limit_final,
            },
        )
        .unwrap()
    };
    let opening = [
        (
            WwdStatus::Forming,
            LOOKBACK_END,
            vec![ResolveForming, OpenOffering],
        ),
        (
            WwdStatus::Forming,
            OFFERING_END - 1,
            vec![ResolveForming, OpenOffering],
        ),
        (WwdStatus::LookbackDelay, LOOKBACK_END, vec![OpenOffering]),
        (
            WwdStatus::LookbackDelay,
            OFFERING_END - 1,
            vec![OpenOffering],
        ),
    ];
    for (status, block_time, edges) in opening {
        let missed = advance(status, U256::ZERO, block_time, true);
        assert_eq!(missed.target(), WwdStatus::Failed);
        assert_eq!(missed.membership_after(), WwdMembership::Closed);
        assert_eq!(
            missed.kind(),
            &OuterWwdTransitionKind::MissedOffering {
                preceding_edges: edges[..edges.len() - 1].to_vec(),
            }
        );
        for (limit, limit_final) in [(U256::ZERO, false), (U256::ONE, true)] {
            let opened = advance(status, limit, block_time, limit_final);
            assert_eq!(opened.target(), WwdStatus::Offering);
            assert_eq!(
                opened.kind(),
                &OuterWwdTransitionKind::Advance(edges.clone())
            );
        }
    }

    for (status, block_time, edges) in [
        (WwdStatus::Forming, FORMING_END, vec![ResolveForming]),
        (WwdStatus::Offering, OFFERING_END, vec![CloseOffering]),
        (WwdStatus::Waiting, PROCESS_AT, vec![BecomeReady]),
    ] {
        assert_eq!(
            advance(status, U256::ZERO, block_time, true).kind(),
            &OuterWwdTransitionKind::Advance(edges)
        );
    }
}
