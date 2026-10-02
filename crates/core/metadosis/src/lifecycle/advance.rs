use super::effects::{
    apply_capacity_forfeiture, apply_missed_offering, apply_wwd_advance_edges, WwdEffect,
};
use crate::{
    aggregate::{ValidatedWwdAggregate, WwdProjection},
    commit::commit_outer_transition_with_rate,
    ocomp::schema::require_active_ocomp_profile,
    reducer::{
        reduce_outer_wwd, OuterWwdEvent, OuterWwdTransition, OuterWwdTransitionKind, WwdAdvanceEdge,
    },
    schema::MetadosisContract,
};
use outbe_compressed_entities::ExecutionScope;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};
pub(crate) fn advance_active_worldwide_days(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    require_active_ocomp_profile(&metadosis)?;
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
            scope,
            aggregate: &aggregate,
        }
        .apply(&mut metadosis, current, &transition)?;
    }
    Ok(())
}

struct AdvanceTick<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    scope: &'a ExecutionScope,
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
            scope: self.scope,
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
        let Self {
            ctx,
            scope: _,
            aggregate,
        } = self;
        let retained_count = aggregate.retained_count();
        match transition.kind() {
            OuterWwdTransitionKind::Noop => {}
            OuterWwdTransitionKind::MissedOffering { preceding_edges } => {
                let rate_resolution = apply_wwd_advance_edges(
                    metadosis,
                    current.worldwide_day,
                    preceding_edges,
                    "missed offering resolved one WWD rate more than once",
                )?;
                apply_missed_offering(
                    metadosis,
                    self.effect(current, transition, rate_resolution),
                )?;
            }
            OuterWwdTransitionKind::Advance(edges) => {
                let rate_resolution = apply_wwd_advance_edges(
                    metadosis,
                    current.worldwide_day,
                    edges,
                    "outer advance resolved one WWD rate more than once",
                )?;
                commit_outer_transition_with_rate(
                    metadosis,
                    current.worldwide_day,
                    transition,
                    ctx.block.block_number,
                    rate_resolution,
                )?;
                if edges.last() == Some(&WwdAdvanceEdge::BecomeReady) {
                    return Ok(true);
                }
            }
            OuterWwdTransitionKind::CapacityForfeiture { preceding_edges } => {
                aggregate.validate_capacity_victim(current)?;
                let rate_resolution = apply_wwd_advance_edges(
                    metadosis,
                    current.worldwide_day,
                    preceding_edges,
                    "capacity forfeiture resolved one WWD rate more than once",
                )?;
                apply_capacity_forfeiture(
                    metadosis,
                    self.effect(current, transition, rate_resolution),
                    retained_count,
                )?;
                return Ok(true);
            }
            unexpected => {
                return Err(crate::errors::storage_corruption(format!(
                    "AdvanceDue produced non-Cycle transition {unexpected:?}"
                )));
            }
        }
        Ok(false)
    }
}
