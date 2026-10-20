use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;

use crate::precompile::ITribute;
use crate::schema::TributeContract;

use super::{
    aggregate::{ValidatedWwdAggregate, WwdDayType, WwdMembership, WwdStatus},
    errors::WwdStorageCorruptionError,
    reducer::{
        reduce_outer_wwd, OuterWwdEvent, OuterWwdTransition, OuterWwdTransitionKind, WwdAdvanceEdge,
    },
    schema::WorldwideDayEntryExt,
};

/// Non-forgeable, non-cloneable proof owned by exactly one outer-WWD commit.
/// The constructor is private to this module and the value never crosses into
/// reducer, runtime, commands, or the semantic test facade.
pub(super) struct CommitPermit<'transition> {
    _transition: &'transition mut (),
}

/// The sole mint for an outer-WWD commit permit. The higher-ranked closure
/// prevents the permit from escaping the one transition that owns it.
fn with_commit_permit<R>(
    effect: impl for<'transition> FnOnce(&CommitPermit<'transition>) -> Result<R>,
) -> Result<R> {
    let mut transition_scope = ();
    let permit = CommitPermit {
        _transition: &mut transition_scope,
    };
    effect(&permit)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NewWwdSchedule {
    pub forming_start: u64,
    pub forming_period_seconds: u64,
    pub lookback_delay_seconds: u64,
    pub offering_period_seconds: u64,
    pub waiting_period_seconds: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WwdRateResolution {
    pub previous_vwap_minor: U256,
    pub current_vwap_minor: U256,
    pub day_type: WwdDayType,
}

/// Applies the partition seal/unseal side effect associated with a WWD advance edge.
fn apply_edge_partition_effect(
    tribute: &mut TributeContract<'_>,
    wwd: WorldwideDay,
    edge: WwdAdvanceEdge,
) -> Result<()> {
    match edge {
        WwdAdvanceEdge::OpenOffering => tribute.unseal_day(wwd),
        WwdAdvanceEdge::CloseOffering => tribute.seal_day(wwd),
        WwdAdvanceEdge::ResolveForming | WwdAdvanceEdge::BecomeReady => Ok(()),
    }
}

/// Commits the reducer-authorized creation of one new WWD record, membership,
/// Tribute seal, and canonical creation event.
pub fn commit_new_wwd(
    tribute: &mut TributeContract<'_>,
    worldwide_day: WorldwideDay,
    schedule: NewWwdSchedule,
    transition: &OuterWwdTransition,
) -> Result<()> {
    let _ = commit_outer_transition_owned(
        tribute,
        worldwide_day,
        transition,
        OuterCommitMode::Create(schedule),
    )?;
    Ok(())
}

fn commit_new_wwd_inner(
    scope: &CommitScope<'_, '_>,
    tribute: &mut TributeContract<'_>,
    schedule: NewWwdSchedule,
    transition: &OuterWwdTransition,
) -> Result<Vec<WorldwideDay>> {
    let worldwide_day = scope.worldwide_day;
    let permit = scope.permit;
    let valid_origin =
        transition.source().is_none() && transition.kind() == &OuterWwdTransitionKind::Created;
    let valid_destination = transition.target() == WwdStatus::Forming
        && transition.membership_after() == WwdMembership::Active;
    if !valid_origin || !valid_destination {
        return Err(WwdStorageCorruptionError::WwdCreationRequiresExactCreatedTransition.into());
    }
    if tribute.worldwide_days.get(worldwide_day)?.is_some() {
        return Err(
            WwdStorageCorruptionError::WwdAlreadyExistsAtCreationCommit { worldwide_day }.into(),
        );
    }

    tribute.commit_create_worldwide_day(permit, worldwide_day, schedule)?;
    tribute.commit_add_active_wwd(permit, worldwide_day)?;
    tribute.seal_day(worldwide_day)?;

    let record = tribute
        .worldwide_days
        .get(worldwide_day)?
        .ok_or(WwdStorageCorruptionError::CreatedWwdRecordDisappeared)?;
    tribute.emit(ITribute::WorldwideDayStarted {
        worldwideDay: worldwide_day.into(),
        formingStart: record.forming_start,
        formingEnd: record.forming_end,
        offeringStart: record.lookback_end,
        offeringEnd: record.offering_end,
        scheduledTime: record.scheduled_process_time,
    })?;
    Ok(Vec::new())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OuterCommitMode {
    Create(NewWwdSchedule),
    Existing {
        block_number: u64,
        rate_resolution: Option<WwdRateResolution>,
    },
}

/// The only production owner of outer WWD status, active/closed membership,
/// cleanup, and canonical status-change events. Returns any evicted terminal
/// WWDs so callers can clean up auxiliary receipts.
pub fn commit_outer_transition(
    tribute: &mut TributeContract<'_>,
    worldwide_day: WorldwideDay,
    transition: &OuterWwdTransition,
    block_number: u64,
) -> Result<Vec<WorldwideDay>> {
    commit_outer_transition_owned(
        tribute,
        worldwide_day,
        transition,
        OuterCommitMode::Existing {
            block_number,
            rate_resolution: None,
        },
    )
}

pub fn commit_outer_transition_with_rate(
    tribute: &mut TributeContract<'_>,
    worldwide_day: WorldwideDay,
    transition: &OuterWwdTransition,
    block_number: u64,
    rate_resolution: Option<WwdRateResolution>,
) -> Result<Vec<WorldwideDay>> {
    commit_outer_transition_owned(
        tribute,
        worldwide_day,
        transition,
        OuterCommitMode::Existing {
            block_number,
            rate_resolution,
        },
    )
}

/// Validates that `worldwide_day` is a reducer-valid persisted outer aggregate
/// via a no-op outer transition, and sets its `metadosis_limit_minor` under a
/// scoped `CommitPermit`.
pub fn commit_set_day_limit(
    tribute: &mut TributeContract<'_>,
    worldwide_day: WorldwideDay,
    day_limit: U256,
    missing_error: impl FnOnce() -> outbe_primitives::error::PrecompileError,
    noop_error: impl FnOnce() -> outbe_primitives::error::PrecompileError,
) -> Result<()> {
    let aggregate = ValidatedWwdAggregate::load_and_validate(tribute.storage_handle())?;
    let current = aggregate.record(worldwide_day).ok_or_else(missing_error)?;
    let transition = reduce_outer_wwd(Some(current), OuterWwdEvent::CreateDay)?;
    let unchanged_status =
        transition.source() == Some(current.status) && transition.target() == current.status;
    let unchanged_membership = transition.membership_after() == current.membership;
    if transition.kind() != &OuterWwdTransitionKind::Noop
        || !unchanged_status
        || !unchanged_membership
    {
        return Err(noop_error());
    }
    with_commit_permit(|permit| {
        tribute.commit_set_metadosis_limit(permit, worldwide_day, day_limit)
    })
}

struct CommitScope<'permit, 'transition> {
    permit: &'permit CommitPermit<'transition>,
    worldwide_day: WorldwideDay,
    block_number: u64,
}

#[derive(Clone, Copy)]
enum StatusEvent {
    Emit,
    OwnedByExecution,
}

struct StatusChange {
    source: WwdStatus,
    target: WwdStatus,
    event: StatusEvent,
}

struct TerminalClose<'edges> {
    preceding_edges: &'edges [WwdAdvanceEdge],
    expected_source: WwdStatus,
    mismatch: WwdStorageCorruptionError,
}

fn commit_outer_transition_owned(
    tribute: &mut TributeContract<'_>,
    worldwide_day: WorldwideDay,
    transition: &OuterWwdTransition,
    mode: OuterCommitMode,
) -> Result<Vec<WorldwideDay>> {
    with_commit_permit(|permit| match mode {
        OuterCommitMode::Create(schedule) => commit_new_wwd_inner(
            &CommitScope {
                permit,
                worldwide_day,
                block_number: 0,
            },
            tribute,
            schedule,
            transition,
        ),
        OuterCommitMode::Existing {
            block_number,
            rate_resolution,
        } => {
            let scope = CommitScope {
                permit,
                worldwide_day,
                block_number,
            };
            commit_existing_wwd(&scope, tribute, transition, rate_resolution)
        }
    })
}

fn commit_existing_wwd(
    scope: &CommitScope<'_, '_>,
    tribute: &mut TributeContract<'_>,
    transition: &OuterWwdTransition,
    rate_resolution: Option<WwdRateResolution>,
) -> Result<Vec<WorldwideDay>> {
    commit_rate_resolution(scope, tribute, transition, rate_resolution)?;
    let source = validate_persisted_source(tribute, scope.worldwide_day, transition)?;
    match transition.kind() {
        OuterWwdTransitionKind::Noop => {
            validate_noop(source, transition)?;
            Ok(Vec::new())
        }
        OuterWwdTransitionKind::Created => {
            Err(WwdStorageCorruptionError::CreatedTransitionMustUseCommitNewWwd.into())
        }
        OuterWwdTransitionKind::Advance(edges) => {
            let final_status = commit_advance_edges(scope, tribute, source, edges)?;
            if final_status != transition.target() {
                return Err(
                    WwdStorageCorruptionError::OuterAdvanceEdgesDoNotReachReducerTarget.into(),
                );
            }
            Ok(Vec::new())
        }
        OuterWwdTransitionKind::MissedOffering { preceding_edges } => commit_terminal_close(
            scope,
            tribute,
            source,
            TerminalClose {
                preceding_edges,
                expected_source: WwdStatus::LookbackDelay,
                mismatch:
                    WwdStorageCorruptionError::MissedOfferingCommitMustTerminateFromLookbackDelay,
            },
        ),
        OuterWwdTransitionKind::CapacityForfeiture { preceding_edges } => commit_terminal_close(
            scope,
            tribute,
            source,
            TerminalClose {
                preceding_edges,
                expected_source: WwdStatus::Waiting,
                mismatch:
                    WwdStorageCorruptionError::CapacityForfeitureCommitMustTerminateFromWaiting,
            },
        ),
        OuterWwdTransitionKind::EmergencyFail => commit_failed_day(scope, tribute, source),
        OuterWwdTransitionKind::ProcessReady(_) => {
            commit_execution_status(scope, tribute, source, transition.target())
        }
    }
}

fn commit_rate_resolution(
    scope: &CommitScope<'_, '_>,
    tribute: &mut TributeContract<'_>,
    transition: &OuterWwdTransition,
    rate_resolution: Option<WwdRateResolution>,
) -> Result<()> {
    let requires_resolution = matches!(
        transition.kind(),
        OuterWwdTransitionKind::Advance(edges)
            | OuterWwdTransitionKind::MissedOffering { preceding_edges: edges }
            if edges.contains(&WwdAdvanceEdge::ResolveForming)
    );
    if requires_resolution != rate_resolution.is_some() {
        return Err(
            WwdStorageCorruptionError::ResolveFormingCommitRequiresSingleTypedRateResolution.into(),
        );
    }
    if let Some(resolution) = rate_resolution {
        tribute.commit_set_wwd_rate_resolution(scope.permit, scope.worldwide_day, resolution)?;
    }
    Ok(())
}

fn validate_persisted_source(
    tribute: &TributeContract<'_>,
    worldwide_day: WorldwideDay,
    transition: &OuterWwdTransition,
) -> Result<WwdStatus> {
    let source = transition
        .source()
        .ok_or(WwdStorageCorruptionError::PersistedWwdTransitionMissingSourceStatus)?;
    let persisted = tribute.get_wwd_status(worldwide_day)?;
    if persisted != source {
        return Err(WwdStorageCorruptionError::WwdReducerStatusMismatch {
            worldwide_day,
            expected: source,
            persisted,
        }
        .into());
    }
    let expected_membership = if transition.target().is_terminal() {
        WwdMembership::Closed
    } else {
        WwdMembership::Active
    };
    if transition.membership_after() != expected_membership {
        return Err(
            WwdStorageCorruptionError::OuterTransitionTargetMembershipDecisionInconsistent.into(),
        );
    }
    Ok(source)
}

fn validate_noop(source: WwdStatus, transition: &OuterWwdTransition) -> Result<()> {
    if transition.target() != source {
        return Err(WwdStorageCorruptionError::OuterNoopTransitionChangesStatus.into());
    }
    Ok(())
}

fn commit_terminal_close(
    scope: &CommitScope<'_, '_>,
    tribute: &mut TributeContract<'_>,
    source: WwdStatus,
    close: TerminalClose<'_>,
) -> Result<Vec<WorldwideDay>> {
    let status = commit_advance_edges(scope, tribute, source, close.preceding_edges)?;
    if status != close.expected_source {
        return Err(close.mismatch.into());
    }
    commit_failed_day(scope, tribute, status)
}

fn commit_failed_day(
    scope: &CommitScope<'_, '_>,
    tribute: &mut TributeContract<'_>,
    source: WwdStatus,
) -> Result<Vec<WorldwideDay>> {
    commit_status(
        scope,
        tribute,
        StatusChange {
            source,
            target: WwdStatus::Failed,
            event: StatusEvent::Emit,
        },
    )?;
    tribute.commit_retire_terminal_wwd(scope.permit, scope.worldwide_day)
}

fn commit_execution_status(
    scope: &CommitScope<'_, '_>,
    tribute: &mut TributeContract<'_>,
    source: WwdStatus,
    target: WwdStatus,
) -> Result<Vec<WorldwideDay>> {
    if target != source {
        commit_status(
            scope,
            tribute,
            StatusChange {
                source,
                target,
                event: StatusEvent::OwnedByExecution,
            },
        )?;
    }
    if target.is_terminal() {
        return tribute.commit_retire_terminal_wwd(scope.permit, scope.worldwide_day);
    }
    Ok(Vec::new())
}

fn commit_advance_edges(
    scope: &CommitScope<'_, '_>,
    tribute: &mut TributeContract<'_>,
    source: WwdStatus,
    edges: &[WwdAdvanceEdge],
) -> Result<WwdStatus> {
    let mut status = source;
    for edge in edges {
        if edge.source() != status {
            return Err(WwdStorageCorruptionError::WwdNonContiguousReducerEdge {
                worldwide_day: scope.worldwide_day,
                edge: *edge,
            }
            .into());
        }
        apply_edge_partition_effect(tribute, scope.worldwide_day, *edge)?;
        commit_status(
            scope,
            tribute,
            StatusChange {
                source: status,
                target: edge.target(),
                event: StatusEvent::Emit,
            },
        )?;
        status = edge.target();
    }
    Ok(status)
}

fn commit_status(
    scope: &CommitScope<'_, '_>,
    tribute: &mut TributeContract<'_>,
    change: StatusChange,
) -> Result<()> {
    let StatusChange {
        source,
        target,
        event,
    } = change;
    let worldwide_day = scope.worldwide_day;
    let persisted = tribute.get_wwd_status(worldwide_day)?;
    if persisted != source {
        return Err(WwdStorageCorruptionError::WwdCommitStatusMismatch {
            worldwide_day,
            expected: source,
            persisted,
        }
        .into());
    }
    tribute
        .worldwide_days
        .entry(worldwide_day)
        .status()
        .write(target.as_u8())?;
    if matches!(event, StatusEvent::Emit) {
        tribute.emit(ITribute::WorldwideDayStatusChange {
            worldwideDay: worldwide_day.into(),
            oldStatus: source.as_u8(),
            newStatus: target.as_u8(),
            blockNumber: scope.block_number,
        })?;
    }
    Ok(())
}
