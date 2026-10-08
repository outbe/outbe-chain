use alloy_primitives::B256;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, SweepFailure},
    expiry_queue::{self, Due, ExpiryHandler},
    storage::StorageHandle,
    sweep_budget::SweepBudget,
};

use crate::called::{materializing, sweep_failure};
use crate::{api, precompile::INod, schema::NodContract, state::ExpiryHours};

/// The storage, scope and parent bodies a forfeit loads and removes Nods through.
pub(crate) struct Bodies<'a, 's, P> {
    pub(crate) storage: &'a StorageHandle<'s>,
    pub(crate) scope: &'a ExecutionScope,
    pub(crate) parent: &'a P,
}

/// Forfeit-burns the unpaid Nods of the called buckets whose notice period lapsed.
/// Returns the Nods burned.
pub(crate) fn sweep_expired(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<u32> {
    let queue = NodContract::new(ctx.storage.clone());
    let mut expiry = NodExpiry {
        bodies: Bodies {
            storage: &ctx.storage,
            scope,
            parent,
        },
        nod: NodContract::new(ctx.storage.clone()),
        forfeited: 0,
    };
    let mut budget = SweepBudget::per_block();
    expiry_queue::sweep(
        &ExpiryHours(&queue),
        &ctx.storage,
        ctx.block.timestamp,
        &mut budget,
        &mut expiry,
    )?;
    Ok(expiry.forfeited)
}

/// One block's burns across the due hours.
struct NodExpiry<'a, 's, P> {
    bodies: Bodies<'a, 's, P>,
    nod: NodContract<'s>,
    forfeited: u32,
}

/// A bucket's members are its unpaid Nods. Paid ones keep its terms but never burn.
impl<P: ParentBodySource> ExpiryHandler<B256> for NodExpiry<'_, '_, P> {
    type Member = WwdEntityId;

    /// A bucket whose Nods are still landing waits for the next hour.
    fn due(&mut self, bucket_key: B256) -> Result<Due> {
        if materializing(&self.nod, bucket_key)? {
            return Ok(Due::Wait);
        }
        Ok(match self.nod.bucket_nod_count.read(&bucket_key)? {
            0 => Due::Drop,
            _ => Due::Expire,
        })
    }

    fn member_count(&self, bucket_key: B256) -> Result<u32> {
        self.nod.bucket_nod_count.read(&bucket_key)
    }

    fn member_at(&self, bucket_key: B256, index: u32) -> Result<WwdEntityId> {
        self.nod
            .bucket_nods
            .read(&crate::index_keys::bucket_nod_key(bucket_key, index))
    }

    fn charge(&self, budget: &mut SweepBudget) -> bool {
        budget.body_write()
    }

    fn expire_member(&mut self, bucket_key: B256, nod_id: WwdEntityId) -> Result<()> {
        forfeit_member(&self.bodies, &mut self.nod, bucket_key, nod_id)?;
        self.forfeited = self.forfeited.saturating_add(1);
        Ok(())
    }

    fn classify(&self, error: &PrecompileError) -> SweepFailure {
        sweep_failure(error)
    }

    fn deferred(&mut self, bucket_key: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::nod", %bucket_key, retry_at, "forfeit sweep: bucket deferred");
        self.nod.emit(INod::ExpiryDeferred {
            bucketKey: bucket_key,
            retryAt: retry_at,
        })
    }
}

/// Forfeit-burns one unpaid member of a lapsed bucket and returns its load to the
/// Promis Reserve. Lysis drew it out of the day limit, and only mining converts it
/// into Gratis, so a load destroyed unmined goes back.
///
/// The bucket body remains while settled members exist. Removal requires both
/// unpaid and settled member counts to reach zero.
pub(crate) fn forfeit_member(
    bodies: &Bodies<'_, '_, impl ParentBodySource>,
    nod: &mut NodContract<'_>,
    bucket_key: B256,
    nod_id: WwdEntityId,
) -> Result<()> {
    let Bodies {
        storage,
        scope,
        parent,
    } = *bodies;
    if nod_id.is_zero() {
        return Err(PrecompileError::Revert(format!(
            "Nod bucket {bucket_key} has an empty member slot during forfeit"
        )));
    }
    let worldwide_day = nod.bucket_worldwide_day.read(&bucket_key)?;
    let item = api::load_item(storage, scope, parent, nod_id)?.ok_or_else(|| {
        PrecompileError::Revert(format!(
            "Nod bucket {bucket_key} member {nod_id} has no body during forfeit"
        ))
    })?;
    if item.body().is_settled || item.body().bucket_key != bucket_key {
        return Err(PrecompileError::Revert(format!(
            "Nod bucket {bucket_key} indexes an ineligible member {nod_id}"
        )));
    }
    let owner = item.body().owner;
    let gratis_load_minor = api::calculation_amount(item.body())?;
    let encrypted_gratis_amount = item.body().encrypted.encrypted_gratis_amount.clone();
    let bucket_id = WwdEntityId::from_day_and_digest(worldwide_day, bucket_key.0);
    let bucket = api::load_bucket(storage, scope, parent, bucket_id)?.ok_or_else(|| {
        PrecompileError::Revert(format!(
            "Nod bucket {bucket_key} has no body during forfeit"
        ))
    })?;
    api::remove_nod(storage, scope, item, bucket)?;
    nod.emit(INod::NodForfeited {
        owner,
        nodId: nod_id.to_u256(),
        encryptedGratisAmount: encrypted_gratis_amount.into(),
    })?;
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .add_to_total_unallocated(gratis_load_minor)
}
