use outbe_primitives::error::{PrecompileError, Result};

use crate::{
    aggregate::{WwdMembership, WwdProjection, WwdStatus},
    constants::MAX_RETAINED_WWDS,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WwdAdvanceEdge {
    ResolveForming,
    OpenOffering,
    CloseOffering,
    BecomeReady,
}

impl WwdAdvanceEdge {
    #[must_use]
    pub(crate) const fn source(self) -> WwdStatus {
        match self {
            Self::ResolveForming => WwdStatus::Forming,
            Self::OpenOffering => WwdStatus::LookbackDelay,
            Self::CloseOffering => WwdStatus::Offering,
            Self::BecomeReady => WwdStatus::Waiting,
        }
    }

    #[must_use]
    pub(crate) const fn target(self) -> WwdStatus {
        match self {
            Self::ResolveForming => WwdStatus::LookbackDelay,
            Self::OpenOffering => WwdStatus::Offering,
            Self::CloseOffering => WwdStatus::Waiting,
            Self::BecomeReady => WwdStatus::Ready,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WwdTransitionPlan {
    Noop,
    Advance(Vec<WwdAdvanceEdge>),
    MissedOffering,
}

impl WwdTransitionPlan {
    fn opens_offering(&self) -> bool {
        matches!(self, Self::Advance(edges) if edges.contains(&WwdAdvanceEdge::OpenOffering))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum ReadyDisposition {
    EmptyTributeDay,
    ZeroGratisAllocation,
    PrepareOcomp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(any(test, feature = "test-utils")), allow(dead_code))]
pub(crate) enum OuterWwdEvent {
    CreateDay,
    AdvanceDue {
        block_time: u64,
        retained_count: usize,
        admission_available: bool,
        limit_final: bool,
    },
    ProcessReady(ReadyDisposition),
    OcompRequestCommitted,
    OcompExpired,
    OcompCompleted,
    EmergencyFail,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OuterWwdTransitionKind {
    Noop,
    Created,
    Advance(Vec<WwdAdvanceEdge>),
    MissedOffering {
        preceding_edges: Vec<WwdAdvanceEdge>,
    },
    CapacityForfeiture {
        preceding_edges: Vec<WwdAdvanceEdge>,
    },
    ProcessReady(ReadyDisposition),
    OcompRequestCommitted,
    OcompExpired,
    OcompCompleted,
    EmergencyFail,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OuterWwdTransition {
    source: Option<WwdStatus>,
    target: WwdStatus,
    membership_after: WwdMembership,
    kind: OuterWwdTransitionKind,
}

impl OuterWwdTransition {
    #[must_use]
    pub(crate) const fn source(&self) -> Option<WwdStatus> {
        self.source
    }

    #[must_use]
    pub(crate) const fn target(&self) -> WwdStatus {
        self.target
    }

    #[must_use]
    pub(crate) const fn membership_after(&self) -> WwdMembership {
        self.membership_after
    }

    #[must_use]
    pub(crate) const fn kind(&self) -> &OuterWwdTransitionKind {
        &self.kind
    }
}

pub(crate) fn reduce_outer_wwd(
    current: Option<&WwdProjection>,
    event: OuterWwdEvent,
) -> Result<OuterWwdTransition> {
    if matches!(event, OuterWwdEvent::CreateDay) {
        return Ok(match current {
            None => transition(None, WwdStatus::Forming, OuterWwdTransitionKind::Created),
            Some(current) => transition(
                Some(current.status),
                current.status,
                OuterWwdTransitionKind::Noop,
            ),
        });
    }
    let current = current.ok_or_else(|| {
        crate::errors::storage_corruption(
            "Metadosis outer WWD event requires a persisted day".into(),
        )
    })?;
    match event {
        OuterWwdEvent::AdvanceDue {
            block_time,
            retained_count,
            admission_available,
            limit_final,
        } => reduce_due(
            current,
            block_time,
            Admission {
                retained_count,
                available: admission_available,
                limit_final,
            },
        ),
        OuterWwdEvent::ProcessReady(disposition) => reduce_ready(current, disposition),
        event @ (OuterWwdEvent::OcompRequestCommitted
        | OuterWwdEvent::OcompExpired
        | OuterWwdEvent::OcompCompleted) => reduce_ocomp(current, event),
        OuterWwdEvent::EmergencyFail => reduce_failure(current),
        event => Err(invalid_event(current, event)),
    }
}

struct Admission {
    retained_count: usize,
    available: bool,
    limit_final: bool,
}

fn reduce_due(
    current: &WwdProjection,
    block_time: u64,
    admission: Admission,
) -> Result<OuterWwdTransition> {
    if admission.retained_count > MAX_RETAINED_WWDS {
        return Err(crate::errors::storage_corruption(format!(
            "Metadosis retained WWD count {} exceeds cap {MAX_RETAINED_WWDS}",
            admission.retained_count,
        )));
    }
    let mut plan = plan_wwd_advance(current, block_time)?;
    if admission.limit_final && current.metadosis_limit_amount.is_zero() && plan.opens_offering() {
        plan = WwdTransitionPlan::MissedOffering;
    }
    Ok(reduce_admission_plan(current, plan, admission))
}

fn reduce_admission_plan(
    current: &WwdProjection,
    plan: WwdTransitionPlan,
    admission: Admission,
) -> OuterWwdTransition {
    match plan {
        WwdTransitionPlan::Noop => transition(
            Some(current.status),
            current.status,
            OuterWwdTransitionKind::Noop,
        ),
        WwdTransitionPlan::MissedOffering => {
            let preceding_edges = if current.status == WwdStatus::Forming {
                vec![WwdAdvanceEdge::ResolveForming]
            } else {
                Vec::new()
            };
            transition(
                Some(current.status),
                WwdStatus::Failed,
                OuterWwdTransitionKind::MissedOffering { preceding_edges },
            )
        }
        WwdTransitionPlan::Advance(mut edges)
            if edges.last() == Some(&WwdAdvanceEdge::BecomeReady) && !admission.available =>
        {
            edges.pop();
            let target = edges.last().map_or(current.status, |edge| edge.target());
            let kind = if edges.is_empty() {
                OuterWwdTransitionKind::Noop
            } else {
                OuterWwdTransitionKind::Advance(edges)
            };
            transition(Some(current.status), target, kind)
        }
        WwdTransitionPlan::Advance(mut edges)
            if admission.retained_count == MAX_RETAINED_WWDS
                && edges.last() == Some(&WwdAdvanceEdge::BecomeReady) =>
        {
            edges.pop();
            transition(
                Some(current.status),
                WwdStatus::Failed,
                OuterWwdTransitionKind::CapacityForfeiture {
                    preceding_edges: edges,
                },
            )
        }
        WwdTransitionPlan::Advance(edges) => {
            let target = edges.last().map_or(current.status, |edge| edge.target());
            transition(
                Some(current.status),
                target,
                OuterWwdTransitionKind::Advance(edges),
            )
        }
    }
}

fn reduce_ready(
    current: &WwdProjection,
    disposition: ReadyDisposition,
) -> Result<OuterWwdTransition> {
    if current.status != WwdStatus::Ready {
        return Err(invalid_event(
            current,
            OuterWwdEvent::ProcessReady(disposition),
        ));
    }
    let target = match disposition {
        ReadyDisposition::EmptyTributeDay | ReadyDisposition::ZeroGratisAllocation => {
            WwdStatus::Completed
        }
        ReadyDisposition::PrepareOcomp => WwdStatus::Ready,
    };
    Ok(transition(
        Some(current.status),
        target,
        OuterWwdTransitionKind::ProcessReady(disposition),
    ))
}

fn reduce_ocomp(current: &WwdProjection, event: OuterWwdEvent) -> Result<OuterWwdTransition> {
    let (source, target, kind) = match event {
        OuterWwdEvent::OcompRequestCommitted => (
            WwdStatus::Ready,
            WwdStatus::OffchainPending,
            OuterWwdTransitionKind::OcompRequestCommitted,
        ),
        OuterWwdEvent::OcompExpired => (
            WwdStatus::OffchainPending,
            WwdStatus::Failed,
            OuterWwdTransitionKind::OcompExpired,
        ),
        OuterWwdEvent::OcompCompleted => (
            WwdStatus::OffchainPending,
            WwdStatus::Completed,
            OuterWwdTransitionKind::OcompCompleted,
        ),
        event => return Err(invalid_event(current, event)),
    };
    if current.status != source {
        return Err(invalid_event(current, event));
    }
    Ok(transition(Some(source), target, kind))
}

fn reduce_failure(current: &WwdProjection) -> Result<OuterWwdTransition> {
    if current.status == WwdStatus::Failed {
        return Ok(transition(
            Some(current.status),
            current.status,
            OuterWwdTransitionKind::Noop,
        ));
    }
    if current.status.is_terminal() {
        return Err(invalid_event(current, OuterWwdEvent::EmergencyFail));
    }
    Ok(transition(
        Some(current.status),
        WwdStatus::Failed,
        OuterWwdTransitionKind::EmergencyFail,
    ))
}

fn invalid_event(current: &WwdProjection, event: OuterWwdEvent) -> PrecompileError {
    crate::errors::storage_corruption(format!(
        "Metadosis outer WWD event {event:?} is invalid from {:?}",
        current.status
    ))
}

fn transition(
    source: Option<WwdStatus>,
    target: WwdStatus,
    kind: OuterWwdTransitionKind,
) -> OuterWwdTransition {
    OuterWwdTransition {
        source,
        target,
        membership_after: if target.is_terminal() {
            WwdMembership::Closed
        } else {
            WwdMembership::Active
        },
        kind,
    }
}

/// Pure and exhaustive outer-WWD transition decision.
///
/// The persisted status supplies the lowest time region that is still
/// admissible. A timestamp behind that region is a consensus-state
/// contradiction, not a request to rewind the state machine.
pub(crate) fn plan_wwd_advance(
    current: &WwdProjection,
    block_time: u64,
) -> Result<WwdTransitionPlan> {
    if block_time < current.forming_start || block_time < earliest_time(current) {
        return Err(backward_time(current, block_time));
    }
    Ok(match current.status {
        WwdStatus::Forming => plan_forming(current, block_time),
        WwdStatus::LookbackDelay => plan_lookback(current, block_time),
        WwdStatus::Offering => plan_offering(current, block_time),
        WwdStatus::Waiting if block_time >= current.scheduled_process_time => {
            WwdTransitionPlan::Advance(vec![WwdAdvanceEdge::BecomeReady])
        }
        _ => WwdTransitionPlan::Noop,
    })
}

fn earliest_time(current: &WwdProjection) -> u64 {
    match current.status {
        WwdStatus::Forming => current.forming_start,
        WwdStatus::LookbackDelay => current.forming_end,
        WwdStatus::Offering => current.lookback_end,
        WwdStatus::Waiting | WwdStatus::Failed => current.offering_end,
        WwdStatus::Ready | WwdStatus::OffchainPending | WwdStatus::Completed => {
            current.scheduled_process_time
        }
    }
}

fn plan_forming(current: &WwdProjection, time: u64) -> WwdTransitionPlan {
    if time < current.forming_end {
        return WwdTransitionPlan::Noop;
    }
    if time < current.lookback_end {
        return WwdTransitionPlan::Advance(vec![WwdAdvanceEdge::ResolveForming]);
    }
    if time < current.offering_end {
        return WwdTransitionPlan::Advance(vec![
            WwdAdvanceEdge::ResolveForming,
            WwdAdvanceEdge::OpenOffering,
        ]);
    }
    WwdTransitionPlan::MissedOffering
}

fn plan_lookback(current: &WwdProjection, time: u64) -> WwdTransitionPlan {
    if time < current.lookback_end {
        return WwdTransitionPlan::Noop;
    }
    if time < current.offering_end {
        return WwdTransitionPlan::Advance(vec![WwdAdvanceEdge::OpenOffering]);
    }
    WwdTransitionPlan::MissedOffering
}

fn plan_offering(current: &WwdProjection, time: u64) -> WwdTransitionPlan {
    if time < current.offering_end {
        return WwdTransitionPlan::Noop;
    }
    if time < current.scheduled_process_time {
        return WwdTransitionPlan::Advance(vec![WwdAdvanceEdge::CloseOffering]);
    }
    WwdTransitionPlan::Advance(vec![
        WwdAdvanceEdge::CloseOffering,
        WwdAdvanceEdge::BecomeReady,
    ])
}

fn backward_time(current: &WwdProjection, block_time: u64) -> PrecompileError {
    crate::errors::storage_corruption(format!(
        "Metadosis WWD {} status {:?} is ahead of block timestamp {}",
        current.worldwide_day, current.status, block_time
    ))
}
