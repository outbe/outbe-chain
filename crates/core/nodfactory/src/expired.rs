//! Forfeit sweep: forfeits the called groups whose notice period lapsed, one credit per
//! group, and hands their unpaid Nods to the cleanup.

use alloy_primitives::B256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, SweepFailure},
    deadline_queue::{self, Due, DeadlineHandler},
    storage::StorageHandle,
    sweep_budget::SweepBudget,
};

use crate::precompile::INodFactory;
use crate::runtime::emit_event;
use outbe_nod::state::ExpiryDays;
use outbe_nod::{precompile::INod, schema::NodContract};

/// Forfeits the called groups whose notice period lapsed. Returns the groups forfeited.
#[cfg(any(test, feature = "test-utils"))]
pub fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = NodContract::new(ctx.storage.clone());
    let mut expiry = NodExpiry::new(ctx);
    deadline_queue::process_block(&ExpiryDays(&queue), ctx, &mut expiry)?;
    Ok(expiry.forfeited)
}

pub(crate) fn sweep_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut SweepBudget,
) -> Result<deadline_queue::Progress> {
    let queue = NodContract::new(ctx.storage.clone());
    deadline_queue::process_day(
        &ExpiryDays(&queue),
        ctx,
        through_day,
        budget,
        &mut NodExpiry::new(ctx),
    )
}

struct NodExpiry<'a, 's> {
    storage: &'a StorageHandle<'s>,
    nod: NodContract<'s>,
    forfeited: u32,
}

impl<'a, 's> NodExpiry<'a, 's> {
    fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            storage: &ctx.storage,
            nod: NodContract::new(ctx.storage.clone()),
            forfeited: 0,
        }
    }
}

/// A called group forfeits as one: it is its own and only member.
impl DeadlineHandler<B256> for NodExpiry<'_, '_> {
    type Member = B256;

    fn due(&mut self, group: B256) -> Result<Due> {
        let called = self.nod.group_called_at.read(&group)? != 0;
        Ok(
            match called && self.nod.group_nod_count.read(&group)? != 0 {
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

    fn classify(&self, error: &PrecompileError) -> SweepFailure {
        error.record_sweep_failure()
    }

    /// Returns the Gratis load of the group's unpaid Nods in one credit. Lysis drew it
    /// out of the day limit and only mining converts it, so unmined it goes back.
    fn process_member(&mut self, group: B256, _member: B256) -> Result<()> {
        let nods = self.nod.group_nod_count.read(&group)?;
        let load = self.nod.group_unpaid_load.read(&group)?;
        if !load.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .checked_credit(load)?;
        }
        self.nod.group_unpaid_load.clear(&group)?;
        self.nod.hand_to_cleanup(group)?;
        self.nod.emit(INod::NodGroupForfeited {
            groupKey: group,
            nods,
            gratisLoadMinor: load,
        })?;
        self.forfeited = self.forfeited.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::nodfactory", %group, retry_at, "forfeit sweep: group deferred");
        emit_event(
            self.storage,
            INodFactory::ExpiryDeferred {
                groupKey: group,
                retryAt: retry_at,
            },
        )
    }
}
