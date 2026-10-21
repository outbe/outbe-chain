//! Cleanup sweep: deletes the unpaid gems of the forfeited groups. It moves no value:
//! the expiry already returned their load.

use alloy_primitives::{B256, U256};
use outbe_gem::{api as gem_api, CleanupDays, GemContract};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, SweepFailure},
    expiry_queue::{self, Due, ExpiryHandler},
    storage::StorageHandle,
};

use crate::errors::GemFactoryError;

/// Deletes the unpaid gems of the forfeited groups. Returns the gems deleted.
pub fn sweep_cleanup(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = GemContract::new(ctx.storage.clone());
    let mut cleanup = GemCleanup {
        storage: &ctx.storage,
        gem: GemContract::new(ctx.storage.clone()),
        deleted: 0,
    };
    ctx.storage.with_checkpoint(|| {
        let mut allowance = outbe_primitives::sweep_budget::BlockAllowance::for_block(
            queue.cleanup_budget.read()?,
            ctx.block.block_number,
            0,
        )?;
        expiry_queue::sweep(
            &CleanupDays(&queue),
            &ctx.storage,
            ctx.block.timestamp,
            &mut allowance.budget,
            &mut cleanup,
        )?;
        queue.cleanup_budget.write(allowance.pack())
    })?;
    Ok(cleanup.deleted)
}

struct GemCleanup<'a, 's> {
    storage: &'a StorageHandle<'s>,
    gem: GemContract<'s>,
    deleted: u32,
}

/// A forfeited group's members are its unpaid gems.
impl ExpiryHandler<B256> for GemCleanup<'_, '_> {
    type Member = U256;

    fn due(&mut self, group: B256) -> Result<Due> {
        Ok(match self.gem.group_gem_count.read(&group)? {
            0 => Due::Drop,
            _ => Due::Expire,
        })
    }

    fn member_count(&self, group: B256) -> Result<u32> {
        self.gem.group_gem_count.read(&group)
    }

    fn member_at(&self, group: B256, index: u32) -> Result<U256> {
        self.gem
            .group_gems
            .read(&GemContract::group_member_key(group, index))
    }

    fn classify(&self, error: &PrecompileError) -> SweepFailure {
        error.record_sweep_failure()
    }

    fn expire_member(&mut self, group_key: B256, gem_id: U256) -> Result<()> {
        if gem_id.is_zero() {
            return Err(GemFactoryError::EmptyMemberSlotDuringCleanup { group_key }.into());
        }
        let (item, group) = gem_api::load_pair(self.storage, gem_id)?
            .ok_or(GemFactoryError::MissingMemberRecordDuringCleanup { group_key, gem_id })?;
        if item.body().is_settled() || item.body().group_key != group_key {
            return Err(GemFactoryError::IneligibleGroupMember { group_key, gem_id }.into());
        }
        gem_api::remove_gem(self.storage, item, group)?;
        self.deleted = self.deleted.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::gemfactory", %group, retry_at, "cleanup sweep: group deferred");
        Ok(())
    }
}
