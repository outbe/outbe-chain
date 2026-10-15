//! Cross-module API for the Gem entity store.

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    ExecutionReaders, ExecutionScope, ParentBodySource, VerifiedBody, WwdEntityId,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{first_full_day, timestamp_to_date_key, WorldwideDay};

use crate::schema::{
    CallTerms, GemAddParams, GemBucketState, GemContract, GemItemState, GemState, GemView,
};

/// A decoded gem paired with the exact generic capability that verified it.
pub struct LoadedGem {
    body: GemItemState,
    current: VerifiedBody,
}

impl LoadedGem {
    #[must_use]
    pub const fn body(&self) -> &GemItemState {
        &self.body
    }

    #[must_use]
    pub(crate) fn into_parts(self) -> (GemItemState, VerifiedBody) {
        (self.body, self.current)
    }
}

/// A decoded gem bucket paired with the exact generic capability that verified it.
pub struct LoadedGemBucket {
    body: GemBucketState,
    current: VerifiedBody,
}

impl LoadedGemBucket {
    #[must_use]
    pub const fn body(&self) -> &GemBucketState {
        &self.body
    }

    #[must_use]
    pub(crate) fn into_parts(self) -> (GemBucketState, VerifiedBody) {
        (self.body, self.current)
    }
}

/// Records a gem in the bucket of its day, entry price and reference currency,
/// opening that bucket if it is the first. Returns the gem id. GemFactory decides the issue.
pub fn add_gem(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    params: GemAddParams,
) -> Result<U256> {
    let gem_id = GemContract::generate_gem_id(
        params.owner,
        params.promis_load_minor,
        storage.block_number()?,
    );
    let worldwide_day = WorldwideDay::new(timestamp_to_date_key(params.issued_at));
    let item = GemItemState {
        gem_id: WwdEntityId::from(gem_id),
        owner: params.owner,
        gem_type: params.gem_type,
        promis_load_minor: params.promis_load_minor,
        floor_price_minor: params.floor_price_minor,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
        issued_at: params.issued_at,
        bucket_key: crate::identity::bucket_key(
            worldwide_day,
            params.entry_price_minor,
            params.reference_currency,
        ),
        state: GemState::Issued as u8,
        called_at: 0,
        settled_at: 0,
    };
    record_issued(
        storage,
        ExecutionReaders { scope, parent },
        &item,
        params.entry_price_minor,
        params.call_terms,
    )?;
    Ok(gem_id)
}

/// Records an already built gem body, opening its bucket with `call_terms` if it is the first.
pub fn record_issued(
    storage: &StorageHandle<'_>,
    readers: ExecutionReaders<'_, '_, impl ParentBodySource>,
    item: &GemItemState,
    entry_price_minor: U256,
    call_terms: Option<CallTerms>,
) -> Result<()> {
    let mut gem = GemContract::new(storage.clone());
    storage
        .clone()
        .with_checkpoint(|| gem.record_gem_issued(readers, item, entry_price_minor, call_terms))
}

/// Loads a gem while retaining its verified generic mutation capability.
pub fn load_item(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    gem_id: U256,
) -> Result<Option<LoadedGem>> {
    let gem = GemContract::new(storage.clone());
    gem.get_item_verified(scope, parent, WwdEntityId::from(gem_id))?
        .map(|current| {
            crate::state::gem_item_from_verified(&current).map(|body| LoadedGem { body, current })
        })
        .transpose()
}

/// Loads a gem bucket while retaining its verified generic mutation capability.
pub fn load_bucket(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    bucket_id: WwdEntityId,
) -> Result<Option<LoadedGemBucket>> {
    let gem = GemContract::new(storage.clone());
    gem.get_bucket_verified(scope, parent, bucket_id)?
        .map(|current| {
            crate::state::gem_bucket_from_verified(&current)
                .map(|body| LoadedGemBucket { body, current })
        })
        .transpose()
}

/// Loads a gem together with its bucket. A gem whose bucket body is missing is
/// corruption: the bucket outlives every member.
pub fn load_pair(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    gem_id: U256,
) -> Result<Option<(LoadedGem, LoadedGemBucket)>> {
    let Some(item) = load_item(storage, scope, parent, gem_id)? else {
        return Ok(None);
    };
    let bucket =
        load_bucket(storage, scope, parent, item.body().bucket_id())?.ok_or_else(|| {
            crate::errors::GemFatalError::MissingBucketBody {
                gem_id,
                bucket_id: item.body().bucket_id(),
            }
        })?;
    Ok(Some((item, bucket)))
}

/// Fetches a gem body by id.
pub fn get_item(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    gem_id: U256,
) -> Result<Option<GemItemState>> {
    load_item(storage, scope, parent, gem_id).map(|loaded| loaded.map(|loaded| loaded.body))
}

/// Fetches a bucket body by id.
pub fn get_bucket(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    bucket_id: WwdEntityId,
) -> Result<Option<GemBucketState>> {
    load_bucket(storage, scope, parent, bucket_id).map(|loaded| loaded.map(|loaded| loaded.body))
}

/// Fetches the full status view of a gem: body, bucket body and sealed terms.
pub fn get_view(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    gem_id: U256,
) -> Result<Option<GemView>> {
    let Some((item, bucket)) = load_pair(storage, scope, parent, gem_id)? else {
        return Ok(None);
    };
    Ok(Some(view_of(storage, item.body, bucket.body)?))
}

/// Assembles the view from already loaded bodies.
pub fn view_of(
    storage: &StorageHandle<'_>,
    item: GemItemState,
    bucket: GemBucketState,
) -> Result<GemView> {
    let gem = GemContract::new(storage.clone());
    let terms = gem.read_call_terms(item.bucket_key)?;
    let bucket_called_at = gem.bucket_called_at.read(&item.bucket_key)?;
    Ok(GemView {
        item,
        bucket,
        terms,
        bucket_called_at,
    })
}

/// One owner's gems, with the block's own mutations folded in.
pub fn list_by_owner(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    owner: Address,
) -> Result<Vec<GemItemState>> {
    GemContract::new(storage.clone()).read_all_by_owner(scope, parent, owner)
}

/// A gem is qualified once a finalized daily VWAP closed above its own floor. A zero
/// floor clears on the first eligible full day, because every positive price exceeds it.
pub fn is_qualified(storage: &StorageHandle<'_>, item: &GemItemState) -> Result<bool> {
    outbe_oracle::api::closed_above_floor(
        storage.clone(),
        item.reference_currency,
        item.floor_price_minor,
        first_full_day(item.issued_at),
    )
}

/// Timestamp by which a called bucket must be settled, or `0` while it is not called.
pub fn settlement_deadline(storage: &StorageHandle<'_>, bucket_key: B256) -> Result<u64> {
    let gem = GemContract::new(storage.clone());
    let called_at = gem.bucket_called_at.read(&bucket_key)?;
    if called_at == 0 {
        return Ok(0);
    }
    let notice = gem.bucket_call_notice_period_seconds.read(&bucket_key)?;
    Ok(settlement_deadline_of(called_at, notice))
}

/// The deadline a call at `called_at` sets under `notice_period`.
pub fn settlement_deadline_of(called_at: u64, notice_period: u32) -> u64 {
    called_at.saturating_add(u64::from(notice_period))
}

/// Records a loaded gem paid for. GemFactory decides the settlement and takes the payment.
pub fn record_settled(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    item: LoadedGem,
    bucket: LoadedGemBucket,
) -> Result<()> {
    let mut gem = GemContract::new(storage.clone());
    storage
        .clone()
        .with_checkpoint(|| gem.record_gem_settled(scope, item, bucket))
}

/// Deletes a loaded gem, lowering membership or the paid count, and the bucket once
/// nothing refers to it.
pub fn remove_gem(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    item: LoadedGem,
    bucket: LoadedGemBucket,
) -> Result<()> {
    let mut gem = GemContract::new(storage.clone());
    storage
        .clone()
        .with_checkpoint(|| gem.record_gem_removed(scope, item, bucket))
}
