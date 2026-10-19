use alloy_primitives::{B256, U256};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{Result, SweepFailure},
    expiry_queue::{self, Due, ExpiryHandler},
    sweep_budget::SweepBudget,
};

use outbe_gem::precompile::IGem::GemForfeited;
use outbe_gem::{api as gem_api, ExpiryHours, GemContract};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::StorageHandle;

use crate::errors::GemFactoryError;
use crate::precompile::IGemFactory::ExpiryDeferred;
use crate::runtime::emit_event;

/// Forfeit-burn the unpaid gems of the called groups whose notice period closed.
/// Returns the gems burned.
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
    Ok(expiry.burned)
}

/// One block's burns across the due hours.
pub(crate) struct GemExpiry<'a, 's> {
    /// The EVM state used to load and remove Gems during forfeiture.
    storage: &'a StorageHandle<'s>,
    gem: GemContract<'s>,
    burned: u32,
}

/// A called group's members are its unpaid gems: a settled one has left it.
impl ExpiryHandler<B256> for GemExpiry<'_, '_> {
    type Member = U256;

    fn due(&mut self, group: B256) -> Result<Due> {
        let called = self.gem.group_called_at.read(&group)? != 0;
        Ok(
            match called && self.gem.group_gem_count.read(&group)? != 0 {
                true => Due::Expire,
                false => Due::Drop,
            },
        )
    }

    fn member_count(&self, group: B256) -> Result<u32> {
        self.gem.group_gem_count.read(&group)
    }

    fn member_at(&self, group: B256, index: u32) -> Result<U256> {
        self.gem
            .group_gems
            .read(&GemContract::group_member_key(group, index))
    }

    /// Each forfeit draws from the per-block forfeit limit, separate from slot writes.
    fn charge(&self, budget: &mut SweepBudget) -> bool {
        budget.body_write()
    }

    fn classify(&self, error: &PrecompileError) -> SweepFailure {
        error.record_sweep_failure()
    }

    fn expire_member(&mut self, group: B256, gem_id: U256) -> Result<()> {
        forfeit_member(self.storage, &mut self.gem, group, gem_id)?;
        self.burned = self.burned.saturating_add(1);
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

impl<'a, 's> GemExpiry<'a, 's> {
    pub(crate) fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            gem: GemContract::new(ctx.storage.clone()),
            burned: 0,
        }
    }
}

/// Forfeit-burns one unpaid member of a lapsed group and returns its load to the
/// Promis reserve. The load came from a daily emission sink and nobody realized it, so
/// it goes back. The return uses the same checkpoint as the burn, or there is nothing
/// to recover.
fn forfeit_member(
    storage: &StorageHandle<'_>,
    gem: &mut GemContract<'_>,
    group_key: B256,
    gem_id: U256,
) -> Result<()> {
    if gem_id.is_zero() {
        return Err(GemFactoryError::EmptyMemberSlotDuringForfeit { group_key }.into());
    }
    let (item, group) = gem_api::load_pair(storage, gem_id)?
        .ok_or(GemFactoryError::MissingMemberRecordDuringForfeit { group_key, gem_id })?;
    if item.body().is_settled() || item.body().group_key != group_key {
        return Err(GemFactoryError::IneligibleGroupMember { group_key, gem_id }.into());
    }
    let owner = item.body().owner;
    let promis_load_minor = item.body().promis_load_minor;
    gem_api::remove_gem(storage, item, group)?;
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .add_to_total_unallocated(promis_load_minor)?;
    gem.emit(GemForfeited {
        gemId: gem_id,
        owner,
        promisLoadMinor: promis_load_minor,
    })
}
