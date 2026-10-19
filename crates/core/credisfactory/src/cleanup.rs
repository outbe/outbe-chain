//! Cleanup sweep: deletes the Credis of the forfeited groups. Each one takes its CCA's
//! default and burns its Gratis from the source's pledge. The forfeit already returned
//! that Gratis to the Promis reserve.

use alloy_primitives::{B256, U256};
use outbe_credis::{CleanupHours, CredisContract};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, SweepFailure},
    expiry_queue::{self, Due, ExpiryHandler},
};

/// Deletes the Credis of the forfeited groups. Returns the Credis deleted.
pub fn sweep_cleanup(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = CredisContract::new(ctx.storage.clone());
    let mut cleanup = CredisCleanup {
        ctx,
        credis: CredisContract::new(ctx.storage.clone()),
        deleted: 0,
    };
    expiry_queue::sweep_block(&CleanupHours(&queue), ctx, &mut cleanup)?;
    Ok(cleanup.deleted)
}

struct CredisCleanup<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    credis: CredisContract<'storage>,
    deleted: u32,
}

/// A forfeited group's members are its unsettled Credis.
impl ExpiryHandler<B256> for CredisCleanup<'_, '_> {
    type Member = U256;

    fn due(&mut self, group: B256) -> Result<Due> {
        Ok(match self.credis.group_member_count(group)? {
            0 => Due::Drop,
            _ => Due::Expire,
        })
    }

    fn member_count(&self, group: B256) -> Result<u32> {
        self.credis.group_member_count(group)
    }

    fn member_at(&self, group: B256, index: u32) -> Result<U256> {
        self.credis.group_member(group, index)
    }

    fn classify(&self, error: &PrecompileError) -> SweepFailure {
        error.record_sweep_failure()
    }

    fn expire_member(&mut self, _group: B256, credis_id: U256) -> Result<()> {
        crate::runtime::delete_forfeited_credis(self.ctx.storage.clone(), credis_id)?;
        self.deleted = self.deleted.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, group: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::credisfactory", %group, retry_at, "cleanup sweep: group deferred");
        Ok(())
    }
}
