use alloy_primitives::{B256, U256};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    expiry_queue::{self, ExpiryHandler, Step},
};

use crate::constants::MAX_EXPIRY_STEPS_PER_BLOCK;
use crate::schema::GemContract;
use crate::state::ExpiryHours;

/// Forfeit-burn the gems whose notice period closed. A head that is not due ends the pass.
pub(super) fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = GemContract::new(ctx.storage.clone());
    let mut expiry = GemExpiry {
        ctx,
        gem: GemContract::new(ctx.storage.clone()),
        burned: 0,
    };
    let mut budget = MAX_EXPIRY_STEPS_PER_BLOCK;
    expiry_queue::sweep(
        &ExpiryHours(&queue),
        ctx.block.timestamp,
        &mut budget,
        &mut expiry,
    )?;
    Ok(expiry.burned)
}

/// One block's burns across hours and buckets.
struct GemExpiry<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    gem: GemContract<'storage>,
    burned: u32,
}

impl ExpiryHandler<U256> for GemExpiry<'_, '_> {
    fn expire(&mut self, entry: U256, budget: &mut u32) -> Result<Step> {
        if let Some(bucket) = self.gem.called_bucket(entry)? {
            return Ok(if self.forfeit_bucket(bucket, budget)? {
                Step::Done
            } else {
                Step::Hold
            });
        }
        *budget -= 1;
        self.forfeit_entry(entry)?;
        Ok(Step::Done)
    }

    fn retire_leftover(&mut self, entry: U256) -> Result<()> {
        let deferred = self.gem.requeue_or_drop(entry, self.ctx.block.timestamp)?;
        tracing::warn!(target: "outbe::gem", %entry, deferred, "expiry sweep: hour outlived itself, retiring its entry");
        Ok(())
    }
}

impl GemExpiry<'_, '_> {
    fn forfeit_entry(&mut self, entry: U256) -> Result<()> {
        let now = self.ctx.block.timestamp;
        let storage = &self.ctx.storage;
        // The entry leaves this bucket either way, so an entry that does not burn cannot
        // block the day. A Called gem among them is retried later rather than lost.
        match storage.with_checkpoint(|| self.gem.forfeit(entry, now)) {
            Ok(true) => self.burned = self.burned.saturating_add(1),
            Ok(false) => {
                if !self.gem.requeue_or_drop(entry, now)? {
                    tracing::warn!(target: "outbe::gem", %entry, "expiry sweep: queued gem is not Called");
                }
            }
            Err(error) if error.is_node_local() => return Err(error),
            Err(error) => {
                let deferred = self.gem.requeue_or_drop(entry, now)?;
                tracing::warn!(target: "outbe::gem", %entry, deferred, error = ?error, "expiry sweep: forfeit failed");
            }
        }
        Ok(())
    }

    /// Burn from the last member down, one budget step each. Returns whether the
    /// bucket left its slot: emptied, or moved on after a gem failed to burn.
    fn forfeit_bucket(&mut self, bucket: B256, budget: &mut u32) -> Result<bool> {
        loop {
            let count = self.gem.bucket_gem_count.read(&bucket)?;
            if count == 0 {
                return Ok(true);
            }
            if *budget == 0 {
                return Ok(false);
            }
            *budget -= 1;
            let gem_id = self
                .gem
                .bucket_gems
                .read(&GemContract::bucket_member_key(bucket, count - 1))?;
            if !self.forfeit_bucket_member(bucket, gem_id)? {
                return Ok(true);
            }
        }
    }

    /// A failed member detaches for retry. If detaching fails, defer the whole bucket.
    fn forfeit_bucket_member(&mut self, bucket: B256, gem_id: U256) -> Result<bool> {
        let now = self.ctx.block.timestamp;
        let storage = &self.ctx.storage;
        let error = match storage.with_checkpoint(|| self.gem.forfeit(gem_id, now)) {
            Ok(true) => {
                self.burned = self.burned.saturating_add(1);
                return Ok(true);
            }
            Ok(false) => None,
            Err(error) if error.is_node_local() => return Err(error),
            Err(error) => Some(error),
        };
        match storage.with_checkpoint(|| self.gem.detach_called_member(gem_id, now)) {
            Ok(()) => {
                tracing::warn!(target: "outbe::gem", %bucket, %gem_id, error = ?error, "expiry sweep: bucket member not forfeited, queued on its own");
                Ok(true)
            }
            Err(detach) if detach.is_node_local() => Err(detach),
            Err(detach) => {
                self.gem
                    .requeue_or_drop(crate::state::bucket_entry(bucket), now)?;
                tracing::warn!(target: "outbe::gem", %bucket, %gem_id, error = ?error, detach = ?detach, "expiry sweep: bucket member not forfeited, bucket deferred");
                Ok(false)
            }
        }
    }
}
