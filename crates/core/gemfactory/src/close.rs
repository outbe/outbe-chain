use alloy_primitives::B256;
use outbe_core_common::deadline_queue::{self, DeadlineHandler, Due};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PassFailure, Result},
};
use outbe_system_common::pass_budget::PassBudget;

use crate::precompile::IGemFactory::GemSeriesClosed;
use outbe_gem::{CloseQueue, GemContract};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::StorageHandle;

pub(crate) fn close_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut PassBudget,
) -> Result<deadline_queue::Progress> {
    let queue = GemContract::new(ctx.storage.clone());
    deadline_queue::run_through_day(
        &CloseQueue(&queue),
        ctx,
        through_day,
        budget,
        &mut GemClose::new(ctx),
    )
}

pub(crate) struct GemClose<'a, 's> {
    storage: &'a StorageHandle<'s>,
    gem: GemContract<'s>,
}

impl<'a, 's> GemClose<'a, 's> {
    pub(crate) fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            gem: GemContract::new(ctx.storage.clone()),
        }
    }
}

/// A called group forfeits as one: it is its own and only member.
impl DeadlineHandler<B256> for GemClose<'_, '_> {
    type Member = B256;

    fn due(&mut self, group: B256) -> Result<Due> {
        Ok(match self.gem.group_called_at.read(&group)? != 0 {
            true => Due::Handle,
            false => Due::Drop,
        })
    }

    fn member_count(&self, _group: B256) -> Result<u32> {
        Ok(1)
    }

    fn member_at(&self, group: B256, _index: u32) -> Result<B256> {
        Ok(group)
    }

    fn classify(&self, error: &PrecompileError) -> PassFailure {
        error.record_pass_failure()
    }

    /// Returns the load of the group's unsettled gems in one credit and hands the gems to
    /// the cleanup. Nobody realized the load, so it goes back to the Promis reserve.
    fn handle_member(&mut self, group: B256, _member: B256) -> Result<()> {
        let load = self.gem.group_unsettled_load_minor.read(&group)?;
        let sub_series = self.gem.group_sub_series(group)?;
        if !load.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .add(outbe_promislimit::LimitSource::Gem, load)?;
        }
        if self.gem.group_gem_count.read(&group)? == 0 {
            self.gem.remove_called(group)?;
            self.gem.close_group_if_empty(group)?;
        } else {
            self.gem.hand_to_cleanup(group)?;
        }
        for sub in sub_series {
            crate::runtime::emit_event(
                self.storage,
                GemSeriesClosed {
                    seriesId: sub.series_id.into(),
                    subSeriesId: sub.sub_series_id,
                    issuanceCurrency: sub.issuance_currency,
                    referenceCurrency: sub.reference_currency,
                    forfeitedCount: sub.unsettled_count,
                    forfeitedAmountMinor: sub.unsettled_load_minor,
                },
            )?;
        }
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::gemfactory", %group, retry_at, "close pass: group deferred");
        Ok(())
    }
}
