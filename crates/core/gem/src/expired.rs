use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    expiry_queue::{self, Due, ExpiryHandler},
    sweep_budget::SweepBudget,
};

use crate::precompile::IGem::ExpiryDeferred;
use crate::runtime::Bodies;
use crate::schema::GemContract;
use crate::state::ExpiryHours;

/// Forfeit-burn the unpaid gems of the called buckets whose notice period closed.
/// Returns the gems burned.
pub(crate) fn sweep_expired(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<u32> {
    let queue = GemContract::new(ctx.storage.clone());
    let mut expiry = GemExpiry {
        bodies: Bodies {
            storage: &ctx.storage,
            scope,
            parent,
        },
        gem: GemContract::new(ctx.storage.clone()),
        burned: 0,
    };
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
struct GemExpiry<'a, 's, P> {
    bodies: Bodies<'a, 's, P>,
    gem: GemContract<'s>,
    burned: u32,
}

/// A called bucket's members are its unpaid gems: a settled one has left it.
impl<P: ParentBodySource> ExpiryHandler<B256> for GemExpiry<'_, '_, P> {
    type Member = U256;

    fn due(&mut self, bucket: B256) -> Result<Due> {
        let called = self.gem.bucket_called_at.read(&bucket)? != 0;
        Ok(
            match called && self.gem.bucket_gem_count.read(&bucket)? != 0 {
                true => Due::Expire,
                false => Due::Drop,
            },
        )
    }

    fn member_count(&self, bucket: B256) -> Result<u32> {
        self.gem.bucket_gem_count.read(&bucket)
    }

    fn member_at(&self, bucket: B256, index: u32) -> Result<U256> {
        self.gem
            .bucket_gems
            .read(&GemContract::bucket_member_key(bucket, index))
    }

    /// A forfeit loads a body and deletes it, which costs more than a slot write.
    fn charge(&self, budget: &mut SweepBudget) -> bool {
        budget.body_write()
    }

    fn expire_member(&mut self, bucket: B256, gem_id: U256) -> Result<()> {
        crate::runtime::forfeit_member(&self.bodies, &mut self.gem, bucket, gem_id)?;
        self.burned = self.burned.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, bucket: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::gem", %bucket, retry_at, "expiry sweep: bucket deferred");
        self.gem.emit(ExpiryDeferred {
            bucketKey: bucket,
            retryAt: retry_at,
        })
    }
}
