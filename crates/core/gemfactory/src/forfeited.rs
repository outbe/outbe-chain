use alloy_primitives::B256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    deadline_queue::{self, DeadlineHandler, Due},
    error::{PassFailure, Result},
    pass_budget::PassBudget,
};

use outbe_gem::precompile::IGem::GemGroupForfeited;
use outbe_gem::{ForfeitQueue, GemContract};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::StorageHandle;

pub(crate) fn forfeit_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut PassBudget,
) -> Result<deadline_queue::Progress> {
    let queue = GemContract::new(ctx.storage.clone());
    deadline_queue::process_day(
        &ForfeitQueue(&queue),
        ctx,
        through_day,
        budget,
        &mut GemForfeit::new(ctx),
    )
}

pub(crate) struct GemForfeit<'a, 's> {
    storage: &'a StorageHandle<'s>,
    gem: GemContract<'s>,
}

impl<'a, 's> GemForfeit<'a, 's> {
    pub(crate) fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            gem: GemContract::new(ctx.storage.clone()),
        }
    }
}

/// A called group forfeits as one: it is its own and only member.
impl DeadlineHandler<B256> for GemForfeit<'_, '_> {
    type Member = B256;

    fn due(&mut self, group: B256) -> Result<Due> {
        let called = self.gem.group_called_at.read(&group)? != 0;
        Ok(
            match called && self.gem.group_gem_count.read(&group)? != 0 {
                true => Due::Process,
                false => Due::Drop,
            },
        )
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

    /// Returns the load of the group's unpaid gems in one credit and hands the gems to
    /// the cleanup. Nobody realized the load, so it goes back to the Promis reserve.
    fn process_member(&mut self, group: B256, _member: B256) -> Result<()> {
        let gems = self.gem.group_gem_count.read(&group)?;
        let load = self.gem.group_unpaid_load.read(&group)?;
        if !load.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .checked_credit(load)?;
        }
        self.gem.hand_to_cleanup(group)?;
        self.gem.emit(GemGroupForfeited {
            groupKey: group,
            gems,
            promisLoadMinor: load,
        })?;
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::gemfactory", %group, retry_at, "forfeit pass: group deferred");
        Ok(())
    }
}
