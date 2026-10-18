//! Forfeit sweep: burns the unpaid Nods of the called buckets whose notice period lapsed
//! and returns their load to the Promis Reserve.

use alloy_primitives::B256;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, SweepFailure},
    expiry_queue::{self, Due, ExpiryHandler},
    storage::StorageHandle,
    sweep_budget::SweepBudget,
};

use crate::precompile::INodFactory;
use crate::runtime::emit_event;
use outbe_nod::state::ExpiryHours;
use outbe_nod::{api, precompile::INod, schema::NodContract};

/// Forfeit-burns the unpaid Nods of the called buckets whose notice period lapsed.
/// Returns the Nods burned.
pub(crate) fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let queue = NodContract::new(ctx.storage.clone());
    let mut expiry = NodExpiry {
        storage: &ctx.storage,
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
struct NodExpiry<'a, 's> {
    /// The EVM state used to load and remove Nods during forfeiture.
    storage: &'a StorageHandle<'s>,
    nod: NodContract<'s>,
    forfeited: u32,
}

/// A bucket's members are its unpaid Nods. Paid ones keep its terms but never burn.
impl ExpiryHandler<B256> for NodExpiry<'_, '_> {
    type Member = WwdEntityId;

    /// Checks whether a native bucket has unpaid members.
    fn due(&mut self, bucket_key: B256) -> Result<Due> {
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
            .read(&outbe_nod::index_keys::bucket_nod_key(bucket_key, index))
    }

    fn charge(&self, budget: &mut SweepBudget) -> bool {
        budget.body_write()
    }

    fn expire_member(&mut self, bucket_key: B256, nod_id: WwdEntityId) -> Result<()> {
        forfeit_member(self.storage, &mut self.nod, bucket_key, nod_id)?;
        self.forfeited = self.forfeited.saturating_add(1);
        Ok(())
    }

    fn classify(&self, error: &PrecompileError) -> SweepFailure {
        error.record_sweep_failure()
    }

    fn deferred(&mut self, bucket_key: B256, retry_at: u64) -> Result<()> {
        tracing::warn!(target: "outbe::nodfactory", %bucket_key, retry_at, "forfeit sweep: bucket deferred");
        emit_event(
            self.storage,
            INodFactory::ExpiryDeferred {
                bucketKey: bucket_key,
                retryAt: retry_at,
            },
        )
    }
}

/// Forfeit-burns one unpaid member of a lapsed bucket and returns its load to the
/// Promis Reserve. Lysis drew it out of the day limit, and only mining converts it
/// into Gratis, so a load destroyed unmined goes back.
///
/// The bucket body remains while settled members exist. Removal requires both
/// unpaid and settled member counts to reach zero.
pub(crate) fn forfeit_member(
    storage: &StorageHandle<'_>,
    nod: &mut NodContract<'_>,
    bucket_key: B256,
    nod_id: WwdEntityId,
) -> Result<()> {
    if nod_id.is_zero() {
        return Err(
            crate::errors::NodFactoryError::EmptyBucketMemberSlotDuringForfeit { bucket_key }
                .into(),
        );
    }
    let item = api::load_item(storage, nod_id)?.ok_or(
        crate::errors::NodFactoryError::MissingBucketMemberRecordDuringForfeit {
            bucket_key,
            nod_id,
        },
    )?;
    if item.body().is_settled || item.body().bucket_key != bucket_key {
        return Err(
            crate::errors::NodFactoryError::IneligibleBucketMember { bucket_key, nod_id }.into(),
        );
    }
    let owner = item.body().owner;
    let gratis_load_minor = api::calculation_amount(item.body())?;
    let encrypted_gratis_amount = item.body().encrypted.encrypted_gratis_amount.clone();
    let bucket = api::load_bucket(storage, bucket_key)?
        .ok_or(crate::errors::NodFactoryError::MissingBucketRecordDuringForfeit { bucket_key })?;
    api::remove_nod(storage, item, bucket)?;
    nod.emit(INod::NodForfeited {
        owner,
        nodId: nod_id.to_u256(),
        encryptedGratisAmount: encrypted_gratis_amount.into(),
    })?;
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .add_to_total_unallocated(gratis_load_minor)
}
