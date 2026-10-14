use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{Result, SweepFailure},
    expiry_queue::{self, Due, ExpiryHandler},
    sweep_budget::SweepBudget,
};

use outbe_gem::precompile::IGem::GemForfeited;
use outbe_gem::{api as gem_api, ExpiryHours, GemContract};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::IGemFactory::ExpiryDeferred;
use crate::runtime::emit_event;

/// Forfeit-burn the unpaid gems of the called buckets whose notice period closed.
/// Returns the gems burned.
pub fn sweep_expired(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<u32> {
    let queue = GemContract::new(ctx.storage.clone());
    let mut expiry = GemExpiry::new(ctx, scope, parent);
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

    fn classify(&self, error: &PrecompileError) -> SweepFailure {
        error.body_sweep_failure()
    }

    fn expire_member(&mut self, bucket: B256, gem_id: U256) -> Result<()> {
        forfeit_member(&self.bodies, &mut self.gem, bucket, gem_id)?;
        self.burned = self.burned.saturating_add(1);
        Ok(())
    }

    fn deferred(&mut self, bucket: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::gemfactory", %bucket, retry_at, "expiry sweep: bucket deferred");
        emit_event(
            &self.gem.storage,
            ExpiryDeferred {
                bucketKey: bucket,
                retryAt: retry_at,
            },
        )
    }
}

impl<'a, 's, P: ParentBodySource> GemExpiry<'a, 's, P> {
    fn new(ctx: &'a BlockRuntimeContext<'s>, scope: &'a ExecutionScope, parent: &'a P) -> Self {
        Self {
            bodies: Bodies {
                storage: &ctx.storage,
                scope,
                parent,
            },
            gem: GemContract::new(ctx.storage.clone()),
            burned: 0,
        }
    }
}

/// The storage, scope and parent bodies a forfeit loads and removes gems through.
struct Bodies<'a, 's, P> {
    storage: &'a StorageHandle<'s>,
    scope: &'a ExecutionScope,
    parent: &'a P,
}

/// Forfeit-burns one unpaid member of a lapsed bucket and returns its load to the
/// Promis reserve. The load came from a daily emission sink and nobody realized it, so
/// it goes back. The return uses the same checkpoint as the burn, or there is nothing
/// to recover.
fn forfeit_member(
    bodies: &Bodies<'_, '_, impl ParentBodySource>,
    gem: &mut GemContract<'_>,
    bucket_key: B256,
    gem_id: U256,
) -> Result<()> {
    let Bodies {
        storage,
        scope,
        parent,
    } = *bodies;
    if gem_id.is_zero() {
        return Err(PrecompileError::Revert(format!(
            "gem bucket {bucket_key} has an empty member slot during forfeit"
        )));
    }
    let (item, bucket) = gem_api::load_pair(storage, scope, parent, gem_id)?.ok_or_else(|| {
        PrecompileError::Revert(format!(
            "gem bucket {bucket_key} member {gem_id} has no body during forfeit"
        ))
    })?;
    if item.body().is_settled() || item.body().bucket_key != bucket_key {
        return Err(PrecompileError::Revert(format!(
            "gem bucket {bucket_key} indexes an ineligible member {gem_id}"
        )));
    }
    let owner = item.body().owner;
    let promis_load_minor = item.body().promis_load_minor;
    gem_api::remove_gem(storage, scope, item, bucket)?;
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .add_to_total_unallocated(promis_load_minor)?;
    gem.emit(GemForfeited {
        gemId: gem_id,
        owner,
        promisLoadMinor: promis_load_minor,
    })
}
