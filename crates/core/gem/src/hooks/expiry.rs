use alloy_primitives::{B256, U256};
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::constants::MAX_EXPIRY_STEPS_PER_BLOCK;
use crate::schema::GemContract;

/// Forfeit-burn the gems whose notice period closed. A head that is not due ends the pass.
pub(super) fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let mut sweep = ExpirySweep {
        ctx,
        budget: MAX_EXPIRY_STEPS_PER_BLOCK,
        burned: 0,
    };
    while sweep.budget > 0 {
        let mut gem = GemContract::new(ctx.storage.clone());
        let Some(day) = gem.first_expiry_day()? else {
            break;
        };
        // A deadline lies inside its own hour, so an open one holds nobody due.
        if ctx.block.timestamp < GemContract::hour_end(day) {
            break;
        }
        if !sweep.sweep_hour(&mut gem, day)? {
            break;
        }
    }
    Ok(sweep.burned)
}

/// One block's shared expiry budget and burn count, across hours and buckets.
struct ExpirySweep<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    budget: u32,
    burned: u32,
}

enum ExpiryStep {
    Skipped,
    Advanced,
    Paused,
}

struct ExpiryHour {
    day: u32,
    len: u32,
    slot: u32,
}

impl ExpirySweep<'_, '_> {
    /// Returns false when this hour still needs another block's budget.
    fn sweep_hour(&mut self, gem: &mut GemContract<'_>, day: u32) -> Result<bool> {
        let len = gem.expiry_bucket_len.read(&day)?;
        let slot = match gem.expiry_sweep_day.read()? == day {
            true => gem.expiry_cursor.read()?.min(len),
            false => 0,
        };
        let mut hour = ExpiryHour { day, len, slot };
        while hour.slot < hour.len && self.budget > 0 {
            match self.sweep_slot(gem, day, hour.slot)? {
                ExpiryStep::Skipped => {
                    hour.slot += 1;
                    continue;
                }
                ExpiryStep::Advanced => hour.slot += 1,
                ExpiryStep::Paused => break,
            }
            if gem.expiry_bucket_live.read(&day)? == 0 {
                break;
            }
        }
        self.finish_hour(gem, hour)
    }

    fn finish_hour(&self, gem: &mut GemContract<'_>, hour: ExpiryHour) -> Result<bool> {
        let ExpiryHour { day, len, slot } = hour;
        // The last live entry left and retired the hour, cursor included.
        if gem.expiry_bucket_live.read(&day)? == 0 {
            return Ok(true);
        }
        if slot < len {
            gem.expiry_sweep_day.write(day)?;
            gem.expiry_cursor.write(slot)?;
            return Ok(false);
        }
        gem.expiry_sweep_day.write(0)?;
        gem.expiry_cursor.write(0)?;
        // Anything left broke the invariant above. Retiring it keeps the tree moving.
        if gem.expiry_bucket_live.read(&day)? != 0 {
            let (deferred, dropped) = gem.force_retire_hour(day, self.ctx.block.timestamp)?;
            tracing::warn!(target: "outbe::gem", day, deferred, dropped, "expiry sweep: hour outlived itself, retiring it");
        }
        Ok(true)
    }

    fn sweep_slot(&mut self, gem: &mut GemContract<'_>, day: u32, slot: u32) -> Result<ExpiryStep> {
        let Some(entry) = gem.expiry_slot(day, slot)? else {
            self.budget -= 1;
            return Ok(ExpiryStep::Skipped);
        };
        if self.ctx.block.timestamp <= gem.called_deadline.read(&entry)? {
            self.budget -= 1;
            return Ok(ExpiryStep::Skipped);
        }
        if let Some(bucket) = gem.called_bucket(entry)? {
            return Ok(if self.forfeit_bucket(gem, bucket)? {
                ExpiryStep::Advanced
            } else {
                ExpiryStep::Paused
            });
        }
        self.budget -= 1;
        self.forfeit_entry(gem, entry)?;
        Ok(ExpiryStep::Advanced)
    }

    fn forfeit_entry(&mut self, gem: &mut GemContract<'_>, entry: U256) -> Result<()> {
        let now = self.ctx.block.timestamp;
        // The entry leaves this bucket either way, so an entry that does not burn cannot
        // block the day. A Called gem among them is retried later rather than lost.
        match self.ctx.storage.with_checkpoint(|| gem.forfeit(entry, now)) {
            Ok(true) => self.burned = self.burned.saturating_add(1),
            Ok(false) => {
                if !gem.requeue_or_drop(entry, now)? {
                    tracing::warn!(target: "outbe::gem", %entry, "expiry sweep: queued gem is not Called");
                }
            }
            Err(error) if error.is_node_local() => return Err(error),
            Err(error) => {
                let deferred = gem.requeue_or_drop(entry, now)?;
                tracing::warn!(target: "outbe::gem", %entry, deferred, error = ?error, "expiry sweep: forfeit failed");
            }
        }
        Ok(())
    }

    /// Burn from the last member down, one budget step each. Returns whether the
    /// bucket left its slot: emptied, or moved on after a gem failed to burn.
    fn forfeit_bucket(&mut self, gem: &mut GemContract<'_>, bucket: B256) -> Result<bool> {
        loop {
            let count = gem.bucket_gem_count.read(&bucket)?;
            if count == 0 {
                return Ok(true);
            }
            if self.budget == 0 {
                return Ok(false);
            }
            self.budget -= 1;
            let gem_id = gem
                .bucket_gems
                .read(&GemContract::bucket_member_key(bucket, count - 1))?;
            if !self.forfeit_bucket_member(gem, bucket, gem_id)? {
                return Ok(true);
            }
        }
    }

    /// A failed member detaches for retry. If detaching fails, defer the whole bucket.
    fn forfeit_bucket_member(
        &mut self,
        gem: &mut GemContract<'_>,
        bucket: B256,
        gem_id: U256,
    ) -> Result<bool> {
        let now = self.ctx.block.timestamp;
        let error = match self
            .ctx
            .storage
            .with_checkpoint(|| gem.forfeit(gem_id, now))
        {
            Ok(true) => {
                self.burned = self.burned.saturating_add(1);
                return Ok(true);
            }
            Ok(false) => None,
            Err(error) if error.is_node_local() => return Err(error),
            Err(error) => Some(error),
        };
        match self
            .ctx
            .storage
            .with_checkpoint(|| gem.detach_called_member(gem_id, now))
        {
            Ok(()) => {
                tracing::warn!(target: "outbe::gem", %bucket, %gem_id, error = ?error, "expiry sweep: bucket member not forfeited, queued on its own");
                Ok(true)
            }
            Err(detach) if detach.is_node_local() => Err(detach),
            Err(detach) => {
                gem.requeue_or_drop(crate::state::bucket_entry(bucket), now)?;
                tracing::warn!(target: "outbe::gem", %bucket, %gem_id, error = ?error, detach = ?detach, "expiry sweep: bucket member not forfeited, bucket deferred");
                Ok(false)
            }
        }
    }
}
