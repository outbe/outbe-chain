use alloy_primitives::{B256, U256};
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

use crate::errors::GemFactoryError;
use crate::precompile::IGemFactory::ExpiryDeferred;
use crate::runtime::emit_event;

/// Forfeit-burn the unpaid gems of the called buckets whose notice period closed.
/// Returns the gems burned.
pub(crate) fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = GemContract::new(ctx.storage.clone());
    let mut expiry = GemExpiry::new(ctx);
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
pub(crate) struct GemExpiry<'a, 's> {
    bodies: Bodies<'a, 's>,
    gem: GemContract<'s>,
    burned: u32,
}

/// A called bucket's members are its unpaid gems: a settled one has left it.
impl ExpiryHandler<B256> for GemExpiry<'_, '_> {
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

impl<'a, 's> GemExpiry<'a, 's> {
    pub(crate) fn new(ctx: &'a BlockRuntimeContext<'s>) -> Self {
        Self {
            bodies: Bodies {
                storage: &ctx.storage,
            },
            gem: GemContract::new(ctx.storage.clone()),
            burned: 0,
        }
    }
}

/// The EVM state used to load and remove Gems during forfeiture.
struct Bodies<'a, 's> {
    storage: &'a StorageHandle<'s>,
}

/// Forfeit-burns one unpaid member of a lapsed bucket and returns its load to the
/// Promis reserve. The load came from a daily emission sink and nobody realized it, so
/// it goes back. The return uses the same checkpoint as the burn, or there is nothing
/// to recover.
fn forfeit_member(
    bodies: &Bodies<'_, '_>,
    gem: &mut GemContract<'_>,
    bucket_key: B256,
    gem_id: U256,
) -> Result<()> {
    let Bodies { storage } = *bodies;
    if gem_id.is_zero() {
        return Err(GemFactoryError::EmptyMemberSlotDuringForfeit { bucket_key }.into());
    }
    let (item, bucket) = gem_api::load_pair(storage, gem_id)?
        .ok_or(GemFactoryError::MissingMemberBodyDuringForfeit { bucket_key, gem_id })?;
    if item.body().is_settled() || item.body().bucket_key != bucket_key {
        return Err(GemFactoryError::IneligibleBucketMember { bucket_key, gem_id }.into());
    }
    let owner = item.body().owner;
    let promis_load_minor = item.body().promis_load_minor;
    gem_api::remove_gem(storage, item, bucket)?;
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .add_to_total_unallocated(promis_load_minor)?;
    gem.emit(GemForfeited {
        gemId: gem_id,
        owner,
        promisLoadMinor: promis_load_minor,
    })
}
