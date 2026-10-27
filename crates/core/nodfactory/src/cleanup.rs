//! Cleanup sweep: deletes the unpaid Nods of the forfeited groups. It moves no value:
//! the forfeit already returned their load.

use alloy_primitives::B256;
use outbe_nod::state::CleanupQueue;
use outbe_nod::{api, schema::NodContract};
use outbe_primitives::wwd_entity_id::WwdEntityId;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, PassFailure},
    deadline_queue::{self, Due, DeadlineHandler},
    storage::StorageHandle,
};

use crate::errors::NodFactoryError;

/// Deletes the unpaid Nods of the forfeited groups. Returns the Nods deleted.
pub fn cleanup(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = NodContract::new(ctx.storage.clone());
    let mut cleanup = NodCleanup::new(ctx);
    deadline_queue::process_cleanup(
        &CleanupQueue(&queue),
        ctx,
        &queue.cleanup_budget,
        &mut cleanup,
    )?;
    Ok(cleanup.deleted)
}

struct NodCleanup<'a, 's> {
    storage: &'a StorageHandle<'s>,
    nod: NodContract<'s>,
    deleted: u32,
}

impl<'a, 's> NodCleanup<'a, 's> {
    fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            nod: NodContract::new(ctx.storage.clone()),
            deleted: 0,
        }
    }
}

/// A forfeited group's members are its unpaid Nods.
impl DeadlineHandler<B256> for NodCleanup<'_, '_> {
    type Member = WwdEntityId;

    fn due(&mut self, group: B256) -> Result<Due> {
        Ok(match self.nod.group_nod_count.read(&group)? {
            0 => Due::Drop,
            _ => Due::Process,
        })
    }

    fn member_count(&self, group: B256) -> Result<u32> {
        self.nod.group_nod_count.read(&group)
    }

    fn member_at(&self, group: B256, index: u32) -> Result<WwdEntityId> {
        self.nod
            .group_nods
            .read(&outbe_nod::index_keys::group_nod_key(group, index))
    }

    fn classify(&self, error: &PrecompileError) -> PassFailure {
        error.record_pass_failure()
    }

    fn process_member(&mut self, group_key: B256, nod_id: WwdEntityId) -> Result<()> {
        if nod_id.is_zero() {
            return Err(NodFactoryError::EmptyGroupMemberSlotDuringCleanup { group_key }.into());
        }
        let item = api::load_item(self.storage, nod_id)?
            .ok_or(NodFactoryError::MissingGroupMemberRecordDuringCleanup { group_key, nod_id })?;
        if item.body().is_settled || item.body().group_key != group_key {
            return Err(NodFactoryError::IneligibleGroupMember { group_key, nod_id }.into());
        }
        let group = api::load_group(self.storage, group_key)?
            .ok_or(NodFactoryError::MissingGroupRecordDuringCleanup { group_key })?;
        api::remove_nod(self.storage, item, group)?;
        self.deleted = self.deleted.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::nodfactory", %group, retry_at, "cleanup sweep: group deferred");
        Ok(())
    }
}
