//! Cleanup sweep: deletes the spent series of the expired groups. It moves no value:
//! the expiry already returned what the groups forfeited.

use alloy_primitives::B256;
use outbe_intex::{CleanupDays, IntexContract, SeriesId};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    expiry_queue::{self, Due, ExpiryHandler},
    storage::StorageHandle,
};

/// Walks the expired groups and deletes each series nothing is left in. Returns the
/// series deleted.
pub fn sweep_cleanup(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = IntexContract::new(ctx.storage.clone());
    let mut cleanup = IntexCleanup::new(ctx);
    expiry_queue::sweep_cleanup(
        &CleanupDays(&queue),
        ctx,
        &queue.cleanup_budget,
        &mut cleanup,
    )?;
    Ok(cleanup.deleted)
}

struct IntexCleanup<'a, 'storage> {
    storage: &'a StorageHandle<'storage>,
    intex: IntexContract<'storage>,
    deleted: u32,
}

impl<'a, 's> IntexCleanup<'a, 's> {
    fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            intex: IntexContract::new(ctx.storage.clone()),
            deleted: 0,
        }
    }
}

/// A group's members are its live series. One with settled units left to exercise
/// stays, and its last exercise deletes it.
impl ExpiryHandler<B256> for IntexCleanup<'_, '_> {
    type Member = SeriesId;

    fn due(&mut self, group: B256) -> Result<Due> {
        Ok(match self.intex.group_member_count(group)? {
            0 => Due::Drop,
            _ => Due::Expire,
        })
    }

    fn member_count(&self, group: B256) -> Result<u32> {
        self.intex.group_member_count(group)
    }

    fn member_at(&self, group: B256, index: u32) -> Result<SeriesId> {
        self.intex.group_member(group, index)
    }

    fn expire_member(&mut self, _group: B256, series_id: SeriesId) -> Result<()> {
        if outbe_intex::api::release_if_spent(self.storage, series_id)? {
            self.deleted = self.deleted.saturating_add(1);
        }
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::intexfactory", %group, retry_at, "cleanup sweep: group deferred");
        Ok(())
    }
}
