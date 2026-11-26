//! Forfeit pass: deletes the unsettled Nods of the closed groups. It moves no value:
//! the forfeit already returned their load.

use alloy_primitives::{B256, U256};
use outbe_core_common::deadline_queue::{self, DeadlineHandler, Due};
use outbe_nod::state::ForfeitQueue;
use outbe_nod::{api, schema::NodContract};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PassFailure, PrecompileError, Result},
    storage::StorageHandle,
};

use crate::errors::NodFactoryError;

/// Deletes the unsettled Nods of the closed groups. Returns the Nods deleted.
pub fn forfeit_through(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = NodContract::new(ctx.storage.clone());
    let mut cleanup = NodForfeit::new(ctx);
    deadline_queue::run_with_saved_budget(
        &ForfeitQueue(&queue),
        ctx,
        &queue.forfeit_budget,
        &mut cleanup,
    )?;
    Ok(cleanup.deleted)
}

struct NodForfeit<'a, 's> {
    storage: &'a StorageHandle<'s>,
    nod: NodContract<'s>,
    deleted: u32,
}

impl<'a, 's> NodForfeit<'a, 's> {
    fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            nod: NodContract::new(ctx.storage.clone()),
            deleted: 0,
        }
    }
}

/// A closed group's members are its unsettled Nods.
impl DeadlineHandler<B256> for NodForfeit<'_, '_> {
    type Member = U256;

    fn due(&mut self, group: B256) -> Result<Due> {
        Ok(match self.nod.group_nod_count.read(&group)? {
            0 => Due::Drop,
            _ => Due::Handle,
        })
    }

    fn member_count(&self, group: B256) -> Result<u32> {
        self.nod.group_nod_count.read(&group)
    }

    fn member_at(&self, group: B256, index: u32) -> Result<U256> {
        self.nod
            .group_nods
            .read(&outbe_nod::index_keys::group_nod_key(group, index))
    }

    fn classify(&self, error: &PrecompileError) -> PassFailure {
        error.record_pass_failure()
    }

    fn handle_member(&mut self, group_key: B256, nod_id: U256) -> Result<()> {
        if nod_id.is_zero() {
            return Err(NodFactoryError::EmptyGroupMemberSlotDuringForfeit { group_key }.into());
        }
        let item = api::load_item(self.storage, nod_id)?
            .ok_or(NodFactoryError::MissingGroupMemberRecordDuringForfeit { group_key, nod_id })?;
        if item.body().is_settled || item.body().group_key != group_key {
            return Err(NodFactoryError::IneligibleGroupMember { group_key, nod_id }.into());
        }
        let group = api::load_group(self.storage, group_key)?
            .ok_or(NodFactoryError::MissingGroupRecordDuringForfeit { group_key })?;
        api::remove_nod(self.storage, item, group)?;
        self.deleted = self.deleted.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::nodfactory", %group, retry_at, "forfeit pass: group deferred");
        Ok(())
    }
}
