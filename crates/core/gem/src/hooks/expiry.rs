use alloy_primitives::{B256, U256};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    expiry_queue::{self, Due, ExpiryHandler},
    sweep_budget::SweepBudget,
};

use crate::errors::GemError;
use crate::precompile::IGem::ExpiryDeferred;
use crate::schema::GemContract;
use crate::state::ExpiryHours;

/// Forfeit-burn the gems of the called buckets whose notice period closed.
pub(super) fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = GemContract::new(ctx.storage.clone());
    let mut expiry = GemExpiry {
        gem: GemContract::new(ctx.storage.clone()),
        now: ctx.block.timestamp,
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
struct GemExpiry<'storage> {
    gem: GemContract<'storage>,
    now: u64,
    burned: u32,
}

/// A called bucket's members are its unpaid gems: a settled one has left it.
impl ExpiryHandler<B256> for GemExpiry<'_> {
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

    fn expire_member(&mut self, _bucket: B256, gem_id: U256) -> Result<()> {
        if !self.gem.forfeit(gem_id, self.now)? {
            return Err(GemError::InvalidState.into());
        }
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
