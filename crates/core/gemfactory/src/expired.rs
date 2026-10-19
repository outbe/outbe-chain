use alloy_primitives::B256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{Result, SweepFailure},
    expiry_queue::{self, Due, ExpiryHandler},
    sweep_budget::SweepBudget,
};

use outbe_gem::precompile::IGem::GemGroupForfeited;
use outbe_gem::{ExpiryHours, GemContract};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::IGemFactory::ExpiryDeferred;
use crate::runtime::emit_event;

/// Forfeits the called groups whose notice period closed, one credit per group.
/// Returns the groups forfeited.
pub(crate) fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = GemContract::new(ctx.storage.clone());
    let mut expiry = GemExpiry::new(ctx);
    let mut budget = SweepBudget::per_block();
    expiry_queue::sweep(
        &ExpiryHours(&queue),
        &ctx.storage,
        ctx.block.timestamp,
        &mut budget,
        &mut expiry,
    )?;
    Ok(expiry.forfeited)
}

pub(crate) struct GemExpiry<'a, 's> {
    storage: &'a StorageHandle<'s>,
    gem: GemContract<'s>,
    forfeited: u32,
}

impl<'a, 's> GemExpiry<'a, 's> {
    pub(crate) fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            gem: GemContract::new(ctx.storage.clone()),
            forfeited: 0,
        }
    }
}

/// A called group forfeits as one: it is its own and only member.
impl ExpiryHandler<B256> for GemExpiry<'_, '_> {
    type Member = B256;

    fn due(&mut self, group: B256) -> Result<Due> {
        let called = self.gem.group_called_at.read(&group)? != 0;
        Ok(
            match called && self.gem.group_gem_count.read(&group)? != 0 {
                true => Due::Expire,
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

    fn classify(&self, error: &PrecompileError) -> SweepFailure {
        error.record_sweep_failure()
    }

    /// Returns the load of the group's unpaid gems in one credit and hands the gems to
    /// the cleanup. Nobody realized the load, so it goes back to the Promis reserve.
    fn expire_member(&mut self, group: B256, _member: B256) -> Result<()> {
        let gems = self.gem.group_gem_count.read(&group)?;
        let load = self.gem.group_unpaid_load.read(&group)?;
        if !load.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .add_to_total_unallocated(load)?;
        }
        self.gem.hand_to_cleanup(group)?;
        self.gem.emit(GemGroupForfeited {
            groupKey: group,
            gems,
            promisLoadMinor: load,
        })?;
        self.forfeited = self.forfeited.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::gemfactory", %group, retry_at, "expiry sweep: group deferred");
        emit_event(
            &self.gem.storage,
            ExpiryDeferred {
                groupKey: group,
                retryAt: retry_at,
            },
        )
    }
}
