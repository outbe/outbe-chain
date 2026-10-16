use super::effects::{
    apply_capacity_forfeiture, apply_missed_offering, apply_wwd_advance_edges, WwdEffect,
};
use crate::{
    aggregate::{ValidatedWwdAggregate, WwdProjection},
    commit::commit_outer_transition_with_rate,
    reducer::{
        reduce_outer_wwd, OuterWwdEvent, OuterWwdTransition, OuterWwdTransitionKind, WwdAdvanceEdge,
    },
    schema::MetadosisContract,
};
use outbe_primitives::{block::BlockRuntimeContext, error::Result};
pub(crate) fn advance_active_worldwide_days(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let aggregate = ValidatedWwdAggregate::load_and_validate(ctx.storage.clone())?;
    let retained_count = aggregate.retained_count();
    let block_utc_day = outbe_primitives::time::timestamp_to_date_key(ctx.block.timestamp);
    let mut admission_consumed = false;
    for current in aggregate.active_records() {
        let transition = reduce_outer_wwd(
            Some(current),
            OuterWwdEvent::AdvanceDue {
                block_time: ctx.block.timestamp,
                retained_count,
                admission_available: !admission_consumed,
                // Only settling a WWD's UTC day forms its limit, earlier in this same tick.
                limit_final: block_utc_day > current.worldwide_day.value(),
            },
        )?;
        admission_consumed |= AdvanceTick {
            ctx,
            aggregate: &aggregate,
        }
        .apply(&mut metadosis, current, &transition)?;
    }
    Ok(())
}

struct AdvanceTick<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,

    aggregate: &'a ValidatedWwdAggregate,
}
impl<'storage> AdvanceTick<'_, 'storage> {
    fn effect<'a>(
        &'a self,
        current: &'a WwdProjection,
        transition: &'a OuterWwdTransition,
        rate_resolution: Option<crate::commit::WwdRateResolution>,
    ) -> WwdEffect<'a, 'storage> {
        WwdEffect {
            ctx: self.ctx,
            current,
            transition,
            rate_resolution,
        }
    }

    fn apply(
        &self,
        metadosis: &mut MetadosisContract<'_>,
        current: &WwdProjection,
        transition: &OuterWwdTransition,
    ) -> Result<bool> {
        match transition.kind() {
            OuterWwdTransitionKind::Noop => Ok(false),
            OuterWwdTransitionKind::MissedOffering { preceding_edges } => {
                self.close_missed(
                    metadosis,
                    AdvanceStep {
                        current,
                        transition,
                        edges: preceding_edges,
                    },
                )?;
                Ok(false)
            }
            OuterWwdTransitionKind::Advance(edges) => {
                self.advance(
                    metadosis,
                    AdvanceStep {
                        current,
                        transition,
                        edges,
                    },
                )?;
                Ok(edges.last() == Some(&WwdAdvanceEdge::BecomeReady))
            }
            OuterWwdTransitionKind::CapacityForfeiture { preceding_edges } => {
                self.forfeit_capacity(
                    metadosis,
                    AdvanceStep {
                        current,
                        transition,
                        edges: preceding_edges,
                    },
                )?;
                Ok(true)
            }
            unexpected => Err(
                crate::errors::MetadosisStorageCorruptionError::UnexpectedAdvanceDueTransition {
                    unexpected: unexpected.clone(),
                }
                .into(),
            ),
        }
    }

    fn close_missed(
        &self,
        metadosis: &mut MetadosisContract<'_>,
        step: AdvanceStep<'_>,
    ) -> Result<()> {
        let rate = apply_wwd_advance_edges(
            metadosis,
            step.current.worldwide_day,
            step.edges,
            crate::errors::MetadosisStorageCorruptionError::MissedOfferingResolvedRateMoreThanOnce,
        )?;
        apply_missed_offering(metadosis, self.effect(step.current, step.transition, rate))
    }

    fn advance(&self, metadosis: &mut MetadosisContract<'_>, step: AdvanceStep<'_>) -> Result<()> {
        let rate = apply_wwd_advance_edges(
            metadosis,
            step.current.worldwide_day,
            step.edges,
            crate::errors::MetadosisStorageCorruptionError::OuterAdvanceResolvedRateMoreThanOnce,
        )?;
        commit_outer_transition_with_rate(
            metadosis,
            step.current.worldwide_day,
            step.transition,
            self.ctx.block.block_number,
            rate,
        )
    }

    fn forfeit_capacity(
        &self,
        metadosis: &mut MetadosisContract<'_>,
        step: AdvanceStep<'_>,
    ) -> Result<()> {
        self.aggregate.validate_capacity_victim(step.current)?;
        let rate = apply_wwd_advance_edges(
            metadosis,
            step.current.worldwide_day,
            step.edges,
            crate::errors::MetadosisStorageCorruptionError::CapacityForfeitureResolvedRateMoreThanOnce,
        )?;
        apply_capacity_forfeiture(
            metadosis,
            self.effect(step.current, step.transition, rate),
            self.aggregate.retained_count(),
        )
    }
}

struct AdvanceStep<'step> {
    current: &'step WwdProjection,
    transition: &'step OuterWwdTransition,
    edges: &'step [WwdAdvanceEdge],
}
