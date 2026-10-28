//! Forfeits the called groups whose settlement window lapsed, earliest deadline first,
//! and hands their Credis to the cleanup.

use alloy_primitives::B256;
use outbe_credis::precompile::ICredis;
use outbe_credis::{CredisContract, ForfeitQueue};
use outbe_primitives::{
    block::BlockRuntimeContext,
    deadline_queue::{self, DeadlineHandler, Due},
    error::Result,
    pass_budget::PassBudget,
};

/// Runs from CycleTick every block. Returns the groups forfeited.
#[cfg(test)]
pub fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = CredisContract::new(ctx.storage.clone());
    let mut expiry = CredisForfeit::new(ctx);
    deadline_queue::process_block(&ForfeitQueue(&queue), ctx, &mut expiry)?;
    Ok(expiry.forfeited)
}

pub(crate) fn forfeit_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut PassBudget,
) -> Result<deadline_queue::Progress> {
    let queue = CredisContract::new(ctx.storage.clone());
    deadline_queue::process_day(
        &ForfeitQueue(&queue),
        ctx,
        through_day,
        budget,
        &mut CredisForfeit::new(ctx),
    )
}

struct CredisForfeit<'storage> {
    credis: CredisContract<'storage>,
    forfeited: u32,
}

impl<'s> CredisForfeit<'s> {
    fn new(ctx: &BlockRuntimeContext<'s>) -> Self {
        Self {
            credis: CredisContract::new(ctx.storage.clone()),
            forfeited: 0,
        }
    }
}

/// A called group forfeits as one: it is its own and only member.
impl DeadlineHandler<B256> for CredisForfeit<'_> {
    type Member = B256;

    fn due(&mut self, group: B256) -> Result<Due> {
        let called = self.credis.group_called_at.read(&group)? != 0;
        Ok(
            match called && self.credis.group_member_count(group)? != 0 {
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

    /// Forfeits the group's Credis at once. Each one's Gratis burns and returns to the
    /// Promis reserve together, in the cleanup.
    fn process_member(&mut self, group: B256, _member: B256) -> Result<()> {
        let credis = self.credis.group_member_count(group)?;
        self.credis.hand_to_cleanup(group)?;
        self.credis.emit(ICredis::CredisGroupForfeited {
            groupKey: group,
            credis,
        })?;
        self.forfeited = self.forfeited.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::credisfactory", %group, retry_at, "forfeit pass: group deferred");
        Ok(())
    }
}
