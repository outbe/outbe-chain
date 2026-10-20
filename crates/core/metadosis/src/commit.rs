use alloy_primitives::B256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    error::Result,
    storage::{MetadosisMutationPurpose, StorageHandle},
};
use outbe_tribute::TributeContract;

pub(crate) use outbe_tribute::worldwideday::commit::{NewWwdSchedule, WwdRateResolution};

use crate::{
    aggregate::ValidatedWwdAggregate,
    precompile::IMetadosis,
    reducer::{reduce_outer_wwd, OuterWwdEvent, OuterWwdTransition, OuterWwdTransitionKind},
    schema::MetadosisContract,
    state::{DayLimitFormation, DayLimitFormationReceipt},
};

/// Non-forgeable, non-cloneable proof owned by exactly one Metadosis commit.
/// The constructor is private to this module and the value never crosses into
/// reducer, runtime, commands, or the semantic test facade.
pub(crate) struct CommitPermit<'transition> {
    _transition: &'transition mut (),
}

/// The sole mint for a Metadosis commit permit. The higher-ranked closure
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
/// validates the complete indexed WWD aggregate and terminal receipts both
/// before effects and before commit.
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

/// Commits the reducer-authorized creation of one new WWD record, membership,
/// Tribute seal, and canonical creation event.
pub(crate) fn commit_new_wwd(
    metadosis: &mut MetadosisContract<'_>,
    worldwide_day: WorldwideDay,
    schedule: NewWwdSchedule,
    transition: &OuterWwdTransition,
) -> Result<()> {
    let mut tribute = TributeContract::new(metadosis.storage.clone());
    outbe_tribute::worldwideday::commit_new_wwd(&mut tribute, worldwide_day, schedule, transition)
}

/// Orchestrates outer WWD transition commit on Tribute and cleans up any
/// Metadosis receipts for evicted terminal days.
pub(crate) fn commit_outer_transition(
    metadosis: &mut MetadosisContract<'_>,
    worldwide_day: WorldwideDay,
    transition: &OuterWwdTransition,
    block_number: u64,
) -> Result<()> {
    commit_outer_transition_with_rate(metadosis, worldwide_day, transition, block_number, None)
}

pub(crate) fn commit_outer_transition_with_rate(
    metadosis: &mut MetadosisContract<'_>,
    worldwide_day: WorldwideDay,
    transition: &OuterWwdTransition,
    block_number: u64,
    rate_resolution: Option<WwdRateResolution>,
) -> Result<()> {
    let evicted_days = {
        let mut tribute = TributeContract::new(metadosis.storage.clone());
        outbe_tribute::worldwideday::commit_outer_transition_with_rate(
            &mut tribute,
            worldwide_day,
            transition,
            block_number,
            rate_resolution,
        )?
    };
    if !evicted_days.is_empty() {
        with_commit_permit(|permit| metadosis.cleanup_evicted_wwd_receipts(permit, &evicted_days))?;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use alloy_primitives::B256;
    use outbe_primitives::storage::{
        hashmap::HashMapStorageProvider, MetadosisCertifiedFinality, MetadosisCycleLifecycle,
        MetadosisMutationPurposeTag,
    };
    use outbe_primitives::time::WorldwideDay;

    use super::*;
    use crate::fixture_kernel::MetadosisFixtureKernelExt;

    fn wwd() -> WorldwideDay {
        WorldwideDay::new(20260730)
    }

    fn seed_corrupt_active(provider: &mut HashMapStorageProvider) {
        provider
            .enter(|storage| {
                let mut metadosis = MetadosisContract::new(storage);
                metadosis.create_worldwide_day(wwd(), 1, 1, 1)?;
                metadosis.corrupt_wwd_status_tag(wwd(), 255)?;
                metadosis.add_active_wwd(wwd())
            })
            .unwrap();
    }

    fn assert_mutation_rejected_before_effect(
        purpose: MetadosisMutationPurposeTag,
        expected: &str,
    ) {
        let mut provider = HashMapStorageProvider::new(1);
        seed_corrupt_active(&mut provider);
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
                        let mut metadosis = MetadosisContract::new(storage);
                        metadosis.create_worldwide_day(wwd(), 1, 1, 1)?;
                        metadosis.corrupt_wwd_status_tag(wwd(), 255)?;
                        metadosis.add_active_wwd(wwd())
                    },
                )
            })
            .unwrap_err();
        assert!(error.to_string().contains("unknown status tag"));

        provider
            .enter(|storage| {
                let contract = TributeContract::new(storage);
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
        seed_corrupt_active(&mut provider);
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
