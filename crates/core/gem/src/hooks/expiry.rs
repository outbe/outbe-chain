use alloy_primitives::{B256, U256};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    expiry_queue::{self, Due, ExpiryHandler},
    sweep_budget::SweepBudget,
};

use crate::constants::MAX_EXPIRY_STEPS_PER_BLOCK;
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
    let mut budget = SweepBudget::new(MAX_EXPIRY_STEPS_PER_BLOCK, MAX_EXPIRY_STEPS_PER_BLOCK, 0);
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
impl ExpiryHandler<U256> for GemExpiry<'_> {
    type Member = U256;

    fn due(&mut self, entry: U256) -> Result<Due> {
        Ok(match self.gem.called_bucket(entry)? {
            Some(bucket) if self.gem.bucket_gem_count.read(&bucket)? != 0 => Due::Expire,
            _ => Due::Drop,
        })
    }

    fn member_count(&self, entry: U256) -> Result<u32> {
        self.gem.bucket_gem_count.read(&entry_bucket(entry))
    }

    fn member_at(&self, entry: U256, index: u32) -> Result<U256> {
        self.gem
            .bucket_gems
            .read(&GemContract::bucket_member_key(entry_bucket(entry), index))
    }

    fn expire_member(&mut self, _entry: U256, gem_id: U256) -> Result<()> {
        if !self.gem.forfeit(gem_id, self.now)? {
            return Err(GemError::InvalidState.into());
        }
        self.burned = self.burned.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, entry: U256, retry_at: u64) -> Result<()> {
        let bucket = entry_bucket(entry);
        tracing::warn!(target: "outbe::gem", %bucket, retry_at, "expiry sweep: bucket deferred");
        self.gem.emit(ExpiryDeferred {
            bucketKey: bucket,
            retryAt: retry_at,
        })
    }
}

fn entry_bucket(entry: U256) -> B256 {
    B256::from(entry.to_be_bytes::<32>())
}
