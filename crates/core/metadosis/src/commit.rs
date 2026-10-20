use alloy_primitives::B256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    error::Result,
    storage::{MetadosisMutationPurpose, StorageHandle},
};
use outbe_tribute::TributeContract;

use crate::{
    aggregate::{ValidatedWwdAggregate, WwdMembership, WwdStatus},
    precompile::IMetadosis,
    reducer::{
        reduce_outer_wwd, OuterWwdEvent, OuterWwdTransition, OuterWwdTransitionKind, WwdAdvanceEdge,
    },
    schema::{MetadosisContract, WorldwideDayEntryExt},
    state::{DayLimitFormation, DayLimitFormationReceipt},
};

/// Non-forgeable, non-cloneable proof owned by exactly one outer-WWD commit.
/// The constructor is private to this module and the value never crosses into
/// reducer, runtime, commands, or the semantic test facade.
pub(crate) struct CommitPermit<'transition> {
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

/// The single production mutation seam. The provider grants an exact
/// purpose-bound lease. Its checkpoint covers state and EVM events. This seam
/// validates the complete indexed WWD aggregate both before effects and
/// before commit.
pub(crate) fn commit_transition<P, R>(
    storage: StorageHandle<'_>,
    binding: B256,
    effect: impl FnOnce(StorageHandle<'_>) -> Result<R>,
) -> Result<R>
where
    P: MetadosisMutationPurpose,
{
    let command_storage = storage.clone();
    storage.with_metadosis_mutation::<P, R>(binding, || {
        ValidatedWwdAggregate::load_and_validate(command_storage.clone())?;
        let result = effect(command_storage.clone())?;
        ValidatedWwdAggregate::load_and_validate(command_storage)?;
        Ok(result)
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NewWwdSchedule {
    pub(crate) forming_start: u64,
    pub(crate) forming_period_seconds: u64,
    pub(crate) lookback_delay_seconds: u64,
    pub(crate) offering_period_seconds: u64,
    pub(crate) waiting_period_seconds: u64,
}

/// Commits the reducer-authorized creation of one new WWD record, membership,
/// Tribute seal, and canonical creation event.
pub(crate) fn commit_new_wwd(
    metadosis: &mut MetadosisContract<'_>,
    worldwide_day: WorldwideDay,
    schedule: NewWwdSchedule,
    transition: &OuterWwdTransition,
) -> Result<()> {
    commit_outer_transition_owned(
        metadosis,
        worldwide_day,
        transition,
        OuterCommitMode::Create(schedule),
    )
}

fn commit_new_wwd_inner(
    scope: &CommitScope<'_, '_>,
    metadosis: &mut MetadosisContract<'_>,
    schedule: NewWwdSchedule,
    transition: &OuterWwdTransition,
) -> Result<()> {
    let worldwide_day = scope.worldwide_day;
    let permit = scope.permit;
    let valid_origin =
        transition.source().is_none() && transition.kind() == &OuterWwdTransitionKind::Created;
    let valid_destination = transition.target() == WwdStatus::Forming
        && transition.membership_after() == WwdMembership::Active;
    if !valid_origin || !valid_destination {
        return Err(
            crate::errors::MetadosisStorageCorruptionError::WwdCreationRequiresExactCreatedTransition
                .into(),
        );
    }
    if metadosis.worldwide_days.get(worldwide_day)?.is_some() {
        return Err(
            crate::errors::MetadosisStorageCorruptionError::WwdAlreadyExistsAtCreationCommit {
                worldwide_day,
            }
            .into(),
        );
    }

    metadosis.commit_create_worldwide_day(permit, worldwide_day, schedule)?;
    metadosis.commit_add_active_wwd(permit, worldwide_day)?;
    TributeContract::new(metadosis.storage.clone()).seal_day(worldwide_day)?;

    let record = metadosis
        .worldwide_days
        .get(worldwide_day)?
        .ok_or(crate::errors::MetadosisStorageCorruptionError::CreatedWwdRecordDisappeared)?;
    metadosis.emit(IMetadosis::WorldwideDayStarted {
        worldwideDay: worldwide_day.into(),
        formingStart: record.forming_start,
        formingEnd: record.forming_end,
        offeringStart: record.lookback_end,
        offeringEnd: record.offering_end,
        scheduledTime: record.scheduled_process_time,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OuterCommitMode {
    Create(NewWwdSchedule),
    Existing { block_number: u64 },
}

/// The only production owner of outer WWD status, active/closed membership,
/// cleanup, and canonical status-change events.
pub(crate) fn commit_outer_transition(
    metadosis: &mut MetadosisContract<'_>,
    worldwide_day: WorldwideDay,
    transition: &OuterWwdTransition,
    block_number: u64,
) -> Result<()> {
    commit_outer_transition_owned(
        metadosis,
        worldwide_day,
        transition,
        OuterCommitMode::Existing { block_number },
    )
}

/// Commits one immutable Cycle day-limit formation while proving that the
/// target WWD is a reducer-valid persisted outer aggregate. The permit is born
/// and consumed inside this single no-op outer transition. Callers can supply
/// values, but cannot mutate WWD state directly.
pub(crate) fn commit_day_limit_formation(
    metadosis: &mut MetadosisContract<'_>,
    formation: DayLimitFormation,
) -> Result<DayLimitFormationReceipt> {
    let aggregate = ValidatedWwdAggregate::load_and_validate(metadosis.storage.clone())?;
    let current = aggregate.record(formation.worldwide_day).ok_or(
        crate::errors::MetadosisStorageCorruptionError::DayLimitFormationMissingPersistedWwd {
            worldwide_day: formation.worldwide_day,
        },
    )?;
    let transition = reduce_outer_wwd(Some(current), OuterWwdEvent::CreateDay)?;
    let unchanged_status =
        transition.source() == Some(current.status) && transition.target() == current.status;
    let unchanged_membership = transition.membership_after() == current.membership;
    if transition.kind() != &OuterWwdTransitionKind::Noop
        || !unchanged_status
        || !unchanged_membership
    {
        return Err(
            crate::errors::MetadosisStorageCorruptionError::DayLimitFormationRequiresNoopOuterTransition
                .into(),
        );
    }

    with_commit_permit(|permit| {
        metadosis.commit_write_day_limit_formation(permit, formation)?;
        metadosis.emit(IMetadosis::MetadosisAccumulation {
            date: formation.worldwide_day.value(),
            metadosisLimitMinor: formation.day_limit,
            totalAccumulatedMinor: formation.day_limit,
            blockNumber: formation.block_number,
        })?;
        metadosis.emit(IMetadosis::DayLimitFormed {
            worldwideDay: formation.worldwide_day.value(),
            baseLimitMinor: formation.base_limit,
            promisLimitBeforeMinor: formation.carry_over_before,
            promisLimitTakenMinor: formation.carry_over_taken,
            promisLimitAfterMinor: formation.carry_over_after,
            metadosisLimitMinor: formation.day_limit,
            blockNumber: formation.block_number,
        })?;
        // Selector-last: neither an interrupted write nor an event failure may
        // make a partial formation observable as committed.
        metadosis.commit_seal_day_limit_formation(permit, formation.worldwide_day)?;

        metadosis
            .day_limit_formation_receipt(formation.worldwide_day)?
            .ok_or_else(|| {
                crate::errors::MetadosisStorageCorruptionError::FormedDayLimitReceiptDisappearedAfterCommit
                    .into()
            })
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
    mismatch: crate::errors::MetadosisStorageCorruptionError,
}

fn commit_outer_transition_owned(
    metadosis: &mut MetadosisContract<'_>,
    worldwide_day: WorldwideDay,
    transition: &OuterWwdTransition,
    mode: OuterCommitMode,
) -> Result<()> {
    with_commit_permit(|permit| match mode {
        OuterCommitMode::Create(schedule) => commit_new_wwd_inner(
            &CommitScope {
                permit,
                worldwide_day,
                block_number: 0,
            },
            metadosis,
            schedule,
            transition,
        ),
        OuterCommitMode::Existing { block_number } => {
            let scope = CommitScope {
                permit,
                worldwide_day,
                block_number,
            };
            commit_existing_wwd(&scope, metadosis, transition)
        }
    })
}

fn commit_existing_wwd(
    scope: &CommitScope<'_, '_>,
    metadosis: &mut MetadosisContract<'_>,
    transition: &OuterWwdTransition,
) -> Result<()> {
    let source = validate_persisted_source(metadosis, scope.worldwide_day, transition)?;
    match transition.kind() {
        OuterWwdTransitionKind::Noop => validate_noop(source, transition),
        OuterWwdTransitionKind::Created => Err(
            crate::errors::MetadosisStorageCorruptionError::CreatedTransitionMustUseCommitNewWwd
                .into(),
        ),
        OuterWwdTransitionKind::Advance(edges) => {
            let final_status = commit_advance_edges(scope, metadosis, source, edges)?;
            if final_status != transition.target() {
                return Err(
                    crate::errors::MetadosisStorageCorruptionError::OuterAdvanceEdgesDoNotReachReducerTarget
                        .into(),
                );
            }
            Ok(())
        }
        OuterWwdTransitionKind::MissedOffering { preceding_edges } => commit_terminal_close(
            scope,
            metadosis,
            source,
            TerminalClose {
                preceding_edges,
                expected_source: WwdStatus::LookbackDelay,
                mismatch: crate::errors::MetadosisStorageCorruptionError::MissedOfferingCommitMustTerminateFromLookbackDelay,
            },
        ),
        OuterWwdTransitionKind::CapacityForfeiture { preceding_edges } => commit_terminal_close(
            scope,
            metadosis,
            source,
            TerminalClose {
                preceding_edges,
                expected_source: WwdStatus::Waiting,
                mismatch: crate::errors::MetadosisStorageCorruptionError::CapacityForfeitureCommitMustTerminateFromWaiting,
            },
        ),
        OuterWwdTransitionKind::EmergencyFail => commit_failed_day(scope, metadosis, source),
        OuterWwdTransitionKind::ProcessReady(_) => {
            commit_execution_status(scope, metadosis, source, transition.target())
        }
    }
}

fn validate_persisted_source(
    metadosis: &MetadosisContract<'_>,
    worldwide_day: WorldwideDay,
    transition: &OuterWwdTransition,
) -> Result<WwdStatus> {
    let source = transition.source().ok_or(
        crate::errors::MetadosisStorageCorruptionError::PersistedWwdTransitionMissingSourceStatus,
    )?;
    let persisted = metadosis.get_wwd_status(worldwide_day)?;
    if persisted != source {
        return Err(
            crate::errors::MetadosisStorageCorruptionError::WwdReducerStatusMismatch {
                worldwide_day,
                expected: source,
                persisted,
            }
            .into(),
        );
    }
    let expected_membership = if transition.target().is_terminal() {
        WwdMembership::Closed
    } else {
        WwdMembership::Active
    };
    if transition.membership_after() != expected_membership {
        return Err(
            crate::errors::MetadosisStorageCorruptionError::OuterTransitionTargetMembershipDecisionInconsistent
                .into(),
        );
    }
    Ok(source)
}

fn validate_noop(source: WwdStatus, transition: &OuterWwdTransition) -> Result<()> {
    if transition.target() != source {
        return Err(
            crate::errors::MetadosisStorageCorruptionError::OuterNoopTransitionChangesStatus.into(),
        );
    }
    Ok(())
}

fn commit_terminal_close(
    scope: &CommitScope<'_, '_>,
    metadosis: &mut MetadosisContract<'_>,
    source: WwdStatus,
    close: TerminalClose<'_>,
) -> Result<()> {
    let status = commit_advance_edges(scope, metadosis, source, close.preceding_edges)?;
    if status != close.expected_source {
        return Err(close.mismatch.into());
    }
    commit_failed_day(scope, metadosis, status)
}

fn commit_failed_day(
    scope: &CommitScope<'_, '_>,
    metadosis: &mut MetadosisContract<'_>,
    source: WwdStatus,
) -> Result<()> {
    commit_status(
        scope,
        metadosis,
        StatusChange {
            source,
            target: WwdStatus::Failed,
            event: StatusEvent::Emit,
        },
    )?;
    metadosis.commit_retire_terminal_wwd(scope.permit, scope.worldwide_day)
}

fn commit_execution_status(
    scope: &CommitScope<'_, '_>,
    metadosis: &mut MetadosisContract<'_>,
    source: WwdStatus,
    target: WwdStatus,
) -> Result<()> {
    if target != source {
        commit_status(
            scope,
            metadosis,
            StatusChange {
                source,
                target,
                event: StatusEvent::OwnedByExecution,
            },
        )?;
    }
    if target.is_terminal() {
        metadosis.commit_retire_terminal_wwd(scope.permit, scope.worldwide_day)?;
    }
    Ok(())
}

fn commit_advance_edges(
    scope: &CommitScope<'_, '_>,
    metadosis: &mut MetadosisContract<'_>,
    source: WwdStatus,
    edges: &[WwdAdvanceEdge],
) -> Result<WwdStatus> {
    let mut status = source;
    for edge in edges {
        if edge.source() != status {
            return Err(
                crate::errors::MetadosisStorageCorruptionError::WwdNonContiguousReducerEdge {
                    worldwide_day: scope.worldwide_day,
                    edge: *edge,
                }
                .into(),
            );
        }
        commit_status(
            scope,
            metadosis,
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
    metadosis: &mut MetadosisContract<'_>,
    change: StatusChange,
) -> Result<()> {
    let StatusChange {
        source,
        target,
        event,
    } = change;
    let worldwide_day = scope.worldwide_day;
    let persisted = metadosis.get_wwd_status(worldwide_day)?;
    if persisted != source {
        return Err(
            crate::errors::MetadosisStorageCorruptionError::WwdCommitStatusMismatch {
                worldwide_day,
                expected: source,
                persisted,
            }
            .into(),
        );
    }
    metadosis
        .worldwide_days
        .entry(worldwide_day)
        .status()
        .write(target.as_u8())?;
    if matches!(event, StatusEvent::Emit) {
        metadosis.emit(IMetadosis::WorldwideDayStatusChange {
            worldwideDay: worldwide_day.into(),
            oldStatus: source.as_u8(),
            newStatus: target.as_u8(),
            blockNumber: scope.block_number,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use alloy_primitives::{B256, U256};
    use outbe_primitives::storage::{
        hashmap::HashMapStorageProvider, MetadosisCertifiedFinality, MetadosisCycleLifecycle,
        MetadosisMutationPurposeTag,
    };
    use outbe_primitives::time::WorldwideDay;

    use super::*;
    use crate::{
        aggregate::{ValidatedWwdAggregate, WwdStatus},
        schema::{status, MetadosisContract, WorldwideDay as WorldwideDayRecord},
    };

    fn wwd() -> WorldwideDay {
        WorldwideDay::new(20260730)
    }

    fn record(status: u8) -> WorldwideDayRecord {
        WorldwideDayRecord {
            wwd: wwd(),
            status,
            forming_start: 1,
            forming_end: 2,
            lookback_end: 3,
            offering_end: 4,
            scheduled_process_time: 5,
            metadosis_limit_minor: U256::ZERO,
        }
    }

    fn seed_active(provider: &mut HashMapStorageProvider, record: &WorldwideDayRecord) {
        provider
            .enter(|storage| {
                let contract = MetadosisContract::new(storage);
                contract.worldwide_days.create(record)?;
                contract.active_wwd.insert(record.wwd)?;
                Ok::<_, outbe_primitives::error::PrecompileError>(())
            })
            .unwrap();
    }

    #[test]
    fn unknown_persisted_status_is_rejected_before_effects() {
        let mut provider = HashMapStorageProvider::new(1);
        seed_active(&mut provider, &record(5));
        provider
            .enter(|storage| {
                assert!(matches!(
                    ValidatedWwdAggregate::load_and_validate(storage),
                    Err(outbe_primitives::error::PrecompileError::Revert(_))
                ));
                Ok::<_, outbe_primitives::error::PrecompileError>(())
            })
            .unwrap();
    }

    #[test]
    fn typed_tags_reject_unrecognized_values() {
        assert!(WwdStatus::try_from(255).is_err());
    }

    #[test]
    fn aggregate_rejects_active_closed_overlap() {
        let mut provider = HashMapStorageProvider::new(1);
        let terminal = record(status::COMPLETED);
        provider
            .enter(|storage| {
                let contract = MetadosisContract::new(storage.clone());
                contract.worldwide_days.create(&terminal)?;
                contract.active_wwd.insert(terminal.wwd)?;
                contract.closed_wwd.push_back(terminal.wwd)?;
                let error = ValidatedWwdAggregate::load_and_validate(storage).unwrap_err();
                assert!(error.to_string().contains("both active and closed"));
                Ok::<_, outbe_primitives::error::PrecompileError>(())
            })
            .unwrap();
    }

    #[test]
    fn aggregate_rejects_missing_record_status_membership_and_phase_drift() {
        let mut missing = HashMapStorageProvider::new(1);
        missing
            .enter(|storage| {
                let contract = MetadosisContract::new(storage.clone());
                contract.active_wwd.insert(wwd())?;
                let error = ValidatedWwdAggregate::load_and_validate(storage).unwrap_err();
                assert!(error.to_string().contains("missing WWD"));
                Ok::<_, outbe_primitives::error::PrecompileError>(())
            })
            .unwrap();

        let mut membership = HashMapStorageProvider::new(1);
        seed_active(&mut membership, &record(status::COMPLETED));
        membership.enter(|storage| {
            let error = ValidatedWwdAggregate::load_and_validate(storage).unwrap_err();
            assert!(error.to_string().contains("status/membership mismatch"));
        });

        let mut boundaries = HashMapStorageProvider::new(1);
        let mut invalid = record(status::FORMING);
        invalid.forming_end = 0;
        seed_active(&mut boundaries, &invalid);
        boundaries.enter(|storage| {
            let error = ValidatedWwdAggregate::load_and_validate(storage).unwrap_err();
            assert!(error.to_string().contains("non-monotonic"));
        });
    }

    fn assert_mutation_rejected_before_effect(
        purpose: MetadosisMutationPurposeTag,
        expected: &str,
    ) {
        let mut provider = HashMapStorageProvider::new(1);
        seed_active(&mut provider, &record(255));
        provider.enable_metadosis_mutation_frame(purpose);
        let effect_called = Cell::new(false);

        let error = provider
            .enter(|storage| {
                commit_transition::<MetadosisCycleLifecycle, _>(
                    storage,
                    B256::repeat_byte(0x51),
                    |_| {
                        effect_called.set(true);
                        Ok(())
                    },
                )
            })
            .unwrap_err();

        assert!(error.to_string().contains(expected));
        assert!(!effect_called.get());
    }

    #[test]
    fn invalid_pre_state_rejects_before_effect() {
        assert_mutation_rejected_before_effect(
            MetadosisMutationPurposeTag::CycleLifecycle,
            "unknown status tag",
        );
    }

    #[test]
    fn invalid_post_state_reverts_effect() {
        let mut provider = HashMapStorageProvider::new(1);
        provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);

        let error = provider
            .enter(|storage| {
                commit_transition::<MetadosisCycleLifecycle, _>(
                    storage,
                    B256::repeat_byte(0x52),
                    |storage| {
                        let contract = MetadosisContract::new(storage);
                        let invalid = record(255);
                        contract.worldwide_days.create(&invalid)?;
                        contract.active_wwd.insert(invalid.wwd)?;
                        Ok(())
                    },
                )
            })
            .unwrap_err();
        assert!(error.to_string().contains("unknown status tag"));

        provider
            .enter(|storage| {
                let contract = MetadosisContract::new(storage);
                assert!(contract.worldwide_days.get(wwd())?.is_none());
                assert!(contract.active_wwd.read_all()?.is_empty());
                Ok::<_, outbe_primitives::error::PrecompileError>(())
            })
            .unwrap();
    }

    #[test]
    fn wrong_purpose_rejects_before_aggregate_or_effect() {
        assert_mutation_rejected_before_effect(
            MetadosisMutationPurposeTag::CertifiedFinality,
            "no matching Metadosis mutation lease",
        );
    }

    #[test]
    fn nested_mutation_rejects_before_nested_aggregate_or_effect() {
        let mut provider = HashMapStorageProvider::new(1);
        seed_active(&mut provider, &record(255));
        provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CertifiedFinality);
        let nested_effect_called = Cell::new(false);

        provider
            .enter(|storage| {
                let nested_storage = storage.clone();
                storage.with_metadosis_mutation::<MetadosisCertifiedFinality, _>(
                    B256::repeat_byte(0x54),
                    || {
                        let error = commit_transition::<MetadosisCertifiedFinality, _>(
                            nested_storage,
                            B256::repeat_byte(0x55),
                            |_| {
                                nested_effect_called.set(true);
                                Ok(())
                            },
                        )
                        .unwrap_err();
                        assert!(error
                            .to_string()
                            .contains("no matching Metadosis mutation lease"));
                        Ok(())
                    },
                )
            })
            .unwrap();

        assert!(!nested_effect_called.get());
    }
}
