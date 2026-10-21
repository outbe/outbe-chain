//! Forfeits the called groups whose settlement window lapsed, earliest deadline first,
//! and hands their Credis to the cleanup.

use alloy_primitives::B256;
use alloy_sol_types::SolEvent;
use outbe_credis::precompile::ICredis;
use outbe_credis::{CredisContract, ExpiryDays};
use outbe_primitives::{
    addresses::CREDIS_FACTORY_ADDRESS,
    block::BlockRuntimeContext,
    error::Result,
    expiry_queue::{self, Due, ExpiryHandler},
    sweep_budget::SweepBudget,
};

use crate::precompile::ICredisFactory::ExpiryDeferred;

/// Runs from CycleTick every block. Returns the groups forfeited.
#[cfg(test)]
pub fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = CredisContract::new(ctx.storage.clone());
    let mut expiry = CredisExpiry {
        ctx,
        credis: CredisContract::new(ctx.storage.clone()),
        forfeited: 0,
    };
    expiry_queue::sweep_block(&ExpiryDays(&queue), ctx, &mut expiry)?;
    Ok(expiry.forfeited)
}

pub(crate) fn sweep_through(
    ctx: &BlockRuntimeContext,
    through_day: u32,
    budget: &mut SweepBudget,
) -> Result<expiry_queue::Progress> {
    let queue = CredisContract::new(ctx.storage.clone());
    let mut expiry = CredisExpiry {
        ctx,
        credis: CredisContract::new(ctx.storage.clone()),
        forfeited: 0,
    };
    expiry_queue::sweep_through(
        &ExpiryDays(&queue),
        &ctx.storage,
        ctx.block.timestamp,
        through_day,
        budget,
        &mut expiry,
    )
}

struct CredisExpiry<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    credis: CredisContract<'storage>,
    forfeited: u32,
}

/// A called group forfeits as one: it is its own and only member.
impl ExpiryHandler<B256> for CredisExpiry<'_, '_> {
    type Member = B256;

    fn due(&mut self, group: B256) -> Result<Due> {
        let called = self.credis.group_called_at.read(&group)? != 0;
        Ok(
            match called && self.credis.group_member_count(group)? != 0 {
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

    /// Forfeits the group's Credis at once. Each one's Gratis burns and returns to the
    /// Promis reserve together, in the cleanup.
    fn expire_member(&mut self, group: B256, _member: B256) -> Result<()> {
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
        tracing::warn!(target: "outbe::credisfactory", %group, retry_at, "expiry sweep: forfeit deferred");
        self.ctx.storage.emit_event(
            CREDIS_FACTORY_ADDRESS,
            SolEvent::encode_log_data(&ExpiryDeferred {
                groupKey: group,
                retryAt: retry_at,
            }),
        )
    }
}
