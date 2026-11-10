//! Cleanup sweep: deletes the Credis of the forfeited groups. Each one takes its CCA's
//! default, and its Gratis burns from the source's pledge into the Promis reserve.

use alloy_primitives::U256;
use outbe_common::deadline_queue::{self, DeadlineHandler, Due};
use outbe_credis::{CleanupQueue, CredisContract};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PassFailure, PrecompileError, Result},
    pass_budget::PassBudget,
    series_id::SeriesId,
};

pub(crate) fn cleanup_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut PassBudget,
) -> Result<deadline_queue::Progress> {
    let queue = CredisContract::new(ctx.storage.clone());
    deadline_queue::process_day(
        &CleanupQueue(&queue),
        ctx,
        through_day,
        budget,
        &mut CredisCleanup::new(ctx),
    )
}

struct CredisCleanup<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    credis: CredisContract<'storage>,
}

impl<'a, 's> CredisCleanup<'a, 's> {
    fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            ctx,
            credis: CredisContract::new(ctx.storage.clone()),
        }
    }
}

/// A forfeited series's members are its unsettled Credis.
impl DeadlineHandler<SeriesId> for CredisCleanup<'_, '_> {
    type Member = U256;

    fn due(&mut self, series: SeriesId) -> Result<Due> {
        Ok(match self.credis.series_member_count(series)? {
            0 => Due::Drop,
            _ => Due::Process,
        })
    }

    fn member_count(&self, series: SeriesId) -> Result<u32> {
        self.credis.series_member_count(series)
    }

    fn member_at(&self, series: SeriesId, index: u32) -> Result<U256> {
        self.credis.series_member(series, index)
    }

    fn classify(&self, error: &PrecompileError) -> PassFailure {
        error.record_pass_failure()
    }

    fn process_member(&mut self, _series: SeriesId, credis_id: U256) -> Result<()> {
        crate::runtime::delete_forfeited_credis(self.ctx.storage.clone(), credis_id)?;
        Ok(())
    }

    fn deferred(&mut self, series: SeriesId, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::credisfactory", %series, retry_at, "cleanup sweep: series deferred");
        Ok(())
    }
}
