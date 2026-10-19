//! Forfeits the called groups whose settlement window lapsed, earliest deadline first,
//! and hands their Credis to the cleanup.

use alloy_primitives::B256;
use alloy_sol_types::SolEvent;
use outbe_credis::precompile::ICredis;
use outbe_credis::{CredisContract, ExpiryHours};
use outbe_primitives::{
    addresses::CREDIS_FACTORY_ADDRESS,
    block::BlockRuntimeContext,
    error::Result,
    expiry_queue::{self, Due, ExpiryHandler},
};

use crate::precompile::ICredisFactory::ExpiryDeferred;

/// Runs from CycleTick every block. Returns the groups forfeited.
pub fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = CredisContract::new(ctx.storage.clone());
    let mut expiry = CredisExpiry {
        ctx,
        credis: CredisContract::new(ctx.storage.clone()),
        forfeited: 0,
    };
    expiry_queue::sweep_block(&ExpiryHours(&queue), ctx, &mut expiry)?;
    Ok(expiry.forfeited)
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

    /// Returns the Gratis still pledged on the group's Credis in one credit. The cleanup
    /// burns it from each source's pledge.
    fn expire_member(&mut self, group: B256, _member: B256) -> Result<()> {
        let credis = self.credis.group_member_count(group)?;
        let gratis = self.credis.group_outstanding_gratis.read(&group)?;
        if !gratis.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.ctx.storage.clone())
                .add_to_total_unallocated(gratis)?;
        }
        self.credis.hand_to_cleanup(group)?;
        self.credis.emit(ICredis::CredisGroupForfeited {
            groupKey: group,
            credis,
            gratisBurnedMinor: gratis,
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
