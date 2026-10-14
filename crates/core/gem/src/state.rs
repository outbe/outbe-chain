use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_compressed_entities::{
    delete, list, mint, read, update, BodyInput, EntityRef, ExecutionScope, IdPageRequest,
    ParentBodySource, QueryRef, VerifiedBody, WwdEntityId, MAX_ID_PAGE_LIMIT,
};
use outbe_primitives::call_bins;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::expiry_queue;

use crate::{
    api::{LoadedGem, LoadedGemBucket},
    constants::{TOKEN_NAME, TOKEN_SYMBOL},
    errors::GemError,
    precompile::IGem,
    schema::{CallTerms, GemBucketState, GemContract, GemItemState, GemState},
};

impl GemContract<'_> {
    pub fn name() -> &'static str {
        TOKEN_NAME
    }

    pub fn symbol() -> &'static str {
        TOKEN_SYMBOL
    }

    pub fn parse_gem_id(gem_id: &str) -> Result<U256> {
        let trimmed = gem_id.strip_prefix("0x").unwrap_or(gem_id);
        if trimmed.len() != 64 {
            return Err(GemError::GemNotFound.into());
        }
        let mut buf = [0u8; 32];
        hex::decode_to_slice(trimmed, &mut buf).map_err(|_| GemError::GemNotFound)?;
        Ok(U256::from_be_bytes(buf))
    }

    pub fn total_supply(&self) -> Result<u64> {
        self.total_supply.read()
    }

    // --- Compressed body reads -------------------------------------------------

    pub(crate) fn get_item_verified(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        gem_id: WwdEntityId,
    ) -> Result<Option<VerifiedBody>> {
        read(self.storage.clone(), scope, parent, EntityRef::Gem(gem_id))
    }

    pub(crate) fn get_bucket_verified(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        bucket_id: WwdEntityId,
    ) -> Result<Option<VerifiedBody>> {
        read(
            self.storage.clone(),
            scope,
            parent,
            EntityRef::GemBucket(bucket_id),
        )
    }

    /// Every gem of `owner`, with the block's own mutations folded in.
    pub(crate) fn read_all_by_owner(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        owner: Address,
    ) -> Result<Vec<GemItemState>> {
        let mut records = Vec::new();
        let mut after = None;
        loop {
            let page = list(
                self.storage.clone(),
                scope,
                parent,
                QueryRef::GemByOwner(owner),
                IdPageRequest {
                    after,
                    limit: MAX_ID_PAGE_LIMIT,
                },
            )?;
            let next_after = page.next_after();
            for body in page.into_bodies() {
                records.push(gem_item_from_verified(&body)?);
            }
            let Some(next) = next_after else {
                return Ok(records);
            };
            after = Some(next);
        }
    }

    // --- Lifecycle --------------------------------------------------------------

    /// Records compact issuance state and delegates both bodies to the generic lifecycle.
    pub(crate) fn record_gem_issued(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        item: &GemItemState,
        entry_price_minor: U256,
        call_terms: Option<CallTerms>,
    ) -> Result<()> {
        if self
            .get_item_verified(scope, parent, item.gem_id)?
            .is_some()
        {
            return Err(GemError::AlreadyExists.into());
        }
        let bucket_key = item.bucket_key;
        let bucket_id = item.bucket_id();
        let current_bucket = self.get_bucket_verified(scope, parent, bucket_id)?;
        let member_count = self.bucket_gem_count.read(&bucket_key)?;
        let settled_count = current_bucket
            .as_ref()
            .map(gem_bucket_from_verified)
            .transpose()?
            .map_or(0, |bucket| bucket.settled_gems);
        if current_bucket.is_some() != (member_count > 0 || settled_count > 0) {
            return Err(PrecompileError::Revert(format!(
                "gem bucket {bucket_id} existence disagrees with member count {member_count}"
            )));
        }
        let new_bucket = match current_bucket {
            Some(_) => None,
            None => {
                self.bucket_issued_at.write(&bucket_key, item.issued_at)?;
                if let Some(terms) = call_terms {
                    self.seal_bucket_call_terms(bucket_key, terms)?;
                    self.insert_call_bin(bucket_key)?;
                }
                Some(GemBucketState {
                    bucket_key,
                    worldwide_day: item.worldwide_day(),
                    entry_price_minor,
                    reference_currency: item.reference_currency,
                    settled_gems: 0,
                })
            }
        };

        let supply = self
            .total_supply
            .read()?
            .checked_add(1)
            .ok_or_else(|| PrecompileError::Revert("gem total supply overflow".into()))?;
        self.total_supply.write(supply)?;
        self.insert_bucket_member(bucket_key, item.gem_id.to_u256())?;
        let canonical_item = crate::repository::canonical_item(item);
        mint(self.storage.clone(), scope, BodyInput::Gem(&canonical_item))?;
        if let Some(bucket) = new_bucket {
            let canonical_bucket = crate::repository::canonical_bucket(&bucket);
            mint(
                self.storage.clone(),
                scope,
                BodyInput::GemBucket(&canonical_bucket),
            )?;
        }
        self.emit(IGem::Transfer {
            from: Address::ZERO,
            to: item.owner,
            tokenId: item.gem_id.to_u256(),
        })
    }

    /// Moves one unpaid member into the paid count. The gem keeps its owner and the
    /// bucket's `called_at`, so its deadline survives the bucket.
    pub(crate) fn record_gem_settled(
        &mut self,
        scope: &ExecutionScope,
        item: LoadedGem,
        bucket: LoadedGemBucket,
    ) -> Result<()> {
        let (mut item, current_item) = item.into_parts();
        let (mut bucket, current_bucket) = bucket.into_parts();
        check_loaded_bucket(&item, &current_bucket)?;
        if item.is_settled() {
            return Err(GemError::InvalidState.into());
        }
        bucket.settled_gems = bucket
            .settled_gems
            .checked_add(1)
            .ok_or_else(|| PrecompileError::Revert("gem settled count overflow".into()))?;
        self.remove_bucket_member(item.bucket_key, item.gem_id.to_u256())?;
        item.called_at = self.bucket_called_at.read(&item.bucket_key)?;
        item.settled_at = self.storage.timestamp()?.to::<u64>();
        item.state = GemState::Settled as u8;
        update(
            self.storage.clone(),
            scope,
            current_item,
            BodyInput::Gem(&crate::repository::canonical_item(&item)),
        )?;
        update(
            self.storage.clone(),
            scope,
            current_bucket,
            BodyInput::GemBucket(&crate::repository::canonical_bucket(&bucket)),
        )?;
        self.emit(IGem::MetadataUpdate {
            _tokenId: item.gem_id.to_u256(),
        })
    }

    /// Deletes the gem body. An unpaid gem leaves the membership; a paid one lowers the
    /// paid count. The bucket goes only once both reach zero.
    pub(crate) fn record_gem_removed(
        &mut self,
        scope: &ExecutionScope,
        item: LoadedGem,
        bucket: LoadedGemBucket,
    ) -> Result<()> {
        let (item, current_item) = item.into_parts();
        let (mut bucket, current_bucket) = bucket.into_parts();
        check_loaded_bucket(&item, &current_bucket)?;
        let bucket_id = current_bucket.entity_id();
        let supply = self
            .total_supply
            .read()?
            .checked_sub(1)
            .ok_or_else(|| PrecompileError::Revert("gem total supply underflow".into()))?;
        self.total_supply.write(supply)?;
        let remaining = if item.is_settled() {
            bucket.settled_gems = bucket.settled_gems.checked_sub(1).ok_or_else(|| {
                PrecompileError::Revert(format!("gem bucket {bucket_id} settled count underflow"))
            })?;
            self.bucket_gem_count.read(&item.bucket_key)?
        } else {
            self.remove_bucket_member(item.bucket_key, item.gem_id.to_u256())?
        };
        delete(self.storage.clone(), scope, current_item)?;
        if remaining == 0 && bucket.settled_gems == 0 {
            self.close_bucket(item.bucket_key)?;
            delete(self.storage.clone(), scope, current_bucket)?;
        } else if item.is_settled() {
            update(
                self.storage.clone(),
                scope,
                current_bucket,
                BodyInput::GemBucket(&crate::repository::canonical_bucket(&bucket)),
            )?;
        }
        self.emit(IGem::Transfer {
            from: item.owner,
            to: Address::ZERO,
            tokenId: item.gem_id.to_u256(),
        })
    }

    // --- Bucket member index ----------------------------------------------------

    pub fn bucket_member_key(bucket: B256, index: u32) -> B256 {
        let mut buf = [0u8; 36];
        buf[0..32].copy_from_slice(bucket.as_slice());
        buf[32..36].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }

    /// Appends `gem_id` and increments the bucket's unpaid count.
    pub(crate) fn insert_bucket_member(&mut self, bucket: B256, gem_id: U256) -> Result<()> {
        let index = self.bucket_gem_count.read(&bucket)?;
        let next = index.checked_add(1).ok_or_else(|| {
            PrecompileError::Fatal(format!("gem bucket {bucket} member index overflow"))
        })?;
        self.bucket_gems
            .write(&Self::bucket_member_key(bucket, index), gem_id)?;
        self.bucket_gem_index.write(&gem_id, index)?;
        self.bucket_gem_count.write(&bucket, next)?;
        Ok(())
    }

    /// Swap-removes `gem_id` and returns the remaining unpaid count. The bucket's
    /// terms stay: paid members may still need them.
    pub(crate) fn remove_bucket_member(&mut self, bucket: B256, gem_id: U256) -> Result<u32> {
        let index = self.bucket_gem_index.read(&gem_id)?;
        let last = self
            .bucket_gem_count
            .read(&bucket)?
            .checked_sub(1)
            .ok_or_else(|| corrupt(format!("gem bucket {bucket} is empty")))?;
        if index > last
            || self
                .bucket_gems
                .read(&Self::bucket_member_key(bucket, index))?
                != gem_id
        {
            return Err(corrupt(format!("gem {gem_id} is not in bucket {bucket}")));
        }
        let last_key = Self::bucket_member_key(bucket, last);
        if index != last {
            let moved = self.bucket_gems.read(&last_key)?;
            if moved.is_zero() {
                return Err(corrupt(format!(
                    "gem bucket {bucket} member slot {last} is empty during removal"
                )));
            }
            self.bucket_gems
                .write(&Self::bucket_member_key(bucket, index), moved)?;
            self.bucket_gem_index.write(&moved, index)?;
        }
        self.bucket_gems.clear(&last_key)?;
        self.bucket_gem_index.clear(&gem_id)?;
        self.bucket_gem_count.write(&bucket, last)?;
        Ok(last)
    }

    // --- Sealed terms ------------------------------------------------------------

    /// Writes the call terms a new bucket sealed at issuance. Later gems that join the
    /// same bucket inherit this copy. Nothing reads the profile again.
    pub(crate) fn seal_bucket_call_terms(&mut self, bucket: B256, terms: CallTerms) -> Result<()> {
        self.seal_bucket_prices(bucket, &terms)?;
        self.seal_bucket_calendar(bucket, &terms)?;
        outbe_primitives::call_breach::widen_scan_terms(
            &self.max_call_window_seconds,
            &self.min_call_threshold_seconds,
            terms.reference_currency,
            terms.call_window_seconds,
            terms.call_threshold_seconds,
        )
    }
    fn seal_bucket_prices(&self, bucket: B256, terms: &CallTerms) -> Result<()> {
        self.bucket_call_price_minor
            .write(&bucket, terms.call_price_minor)?;
        self.bucket_currency
            .write(&bucket, terms.reference_currency)?;
        self.bucket_call_rate.write(&bucket, terms.call_rate)?;
        Ok(())
    }

    fn seal_bucket_calendar(&self, bucket: B256, terms: &CallTerms) -> Result<()> {
        self.bucket_call_window_seconds
            .write(&bucket, terms.call_window_seconds)?;
        self.bucket_call_threshold_seconds
            .write(&bucket, terms.call_threshold_seconds)?;
        self.bucket_call_notice_period_seconds
            .write(&bucket, terms.call_notice_period_seconds)?;
        Ok(())
    }

    /// Reads back the terms [`Self::seal_bucket_call_terms`] sealed at issuance.
    pub fn read_call_terms(&self, bucket: B256) -> Result<CallTerms> {
        Ok(CallTerms {
            call_price_minor: self.bucket_call_price_minor.read(&bucket)?,
            reference_currency: self.bucket_currency.read(&bucket)?,
            call_rate: self.bucket_call_rate.read(&bucket)?,
            call_window_seconds: self.bucket_call_window_seconds.read(&bucket)?,
            call_threshold_seconds: self.bucket_call_threshold_seconds.read(&bucket)?,
            call_notice_period_seconds: self.bucket_call_notice_period_seconds.read(&bucket)?,
        })
    }

    /// Records every unpaid member called at once: the bucket leaves the trie for the
    /// expiry queue, where it waits on `deadline` as one entry.
    pub fn record_bucket_called(&mut self, bucket: B256, now: u64, deadline: u64) -> Result<()> {
        self.remove_call_bin(bucket)?;
        self.bucket_called_at.write(&bucket, now)?;
        self.push_called(bucket, deadline)
    }

    /// Forgets a bucket nothing refers to any more: its terms, its place in the call
    /// trie and the expiry queue, its day and its call.
    fn close_bucket(&mut self, bucket: B256) -> Result<()> {
        self.remove_call_bin(bucket)?;
        self.remove_called(bucket)?;
        self.bucket_call_price_minor.clear(&bucket)?;
        self.bucket_currency.clear(&bucket)?;
        self.bucket_call_rate.clear(&bucket)?;
        self.bucket_call_window_seconds.clear(&bucket)?;
        self.bucket_call_threshold_seconds.clear(&bucket)?;
        self.bucket_call_notice_period_seconds.clear(&bucket)?;
        self.bucket_issued_at.clear(&bucket)?;
        self.bucket_called_at.clear(&bucket)?;
        self.bucket_gem_count.clear(&bucket)
    }

    // --- Expiry queue -------------------------------------------------------------

    pub fn push_called(&mut self, bucket: B256, deadline: u64) -> Result<()> {
        expiry_queue::push(&ExpiryHours(self), bucket, deadline)
    }

    pub fn remove_called(&mut self, bucket: B256) -> Result<()> {
        expiry_queue::remove(&ExpiryHours(self), bucket)
    }

    /// Hour since the epoch a deadline falls in: plain UTC, not a WorldwideDay.
    #[cfg(any(test, feature = "test-utils"))]
    pub const fn deadline_hour(deadline: u64) -> u32 {
        expiry_queue::bucket_of(deadline)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub const fn hour_end(day: u32) -> u64 {
        expiry_queue::bucket_end(day)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn expiry_slot(&self, day: u32, slot: u32) -> Result<Option<B256>> {
        expiry_queue::entry_at(&ExpiryHours(self), day, slot)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn first_expiry_day(&self) -> Result<Option<u32>> {
        expiry_queue::first_bucket(&ExpiryHours(self))
    }

    // --- Bucket bins: uncalled buckets by call price --------------------------------

    /// Parks a bucket in the bin of its sealed call price. No-op for a bucket without
    /// a call price.
    fn insert_call_bin(&mut self, bucket: B256) -> Result<()> {
        let price = self.bucket_call_price_minor.read(&bucket)?;
        if price.is_zero() {
            return Ok(());
        }
        let iso = self.bucket_currency.read(&bucket)?;
        let bin = Self::price_to_bin(price)?;
        call_bins::insert(&CallBins(self, iso), bucket, bin)
    }

    /// No-op for a bucket the trie no longer holds.
    pub(crate) fn remove_call_bin(&mut self, bucket: B256) -> Result<()> {
        let iso = self.bucket_currency.read(&bucket)?;
        call_bins::remove(&CallBins(self, iso), bucket)?;
        Ok(())
    }

    // --- Bin keys (PancakeSwap LB-style) ----------------------------------------

    pub fn price_to_bin(price: U256) -> Result<u32> {
        call_bins::price_to_bin(price)
    }
}

fn check_loaded_bucket(item: &GemItemState, current: &VerifiedBody) -> Result<()> {
    let expected = item.bucket_id();
    if current.entity_id() != expected {
        return Err(PrecompileError::Revert(format!(
            "loaded gem bucket {} does not match item bucket {expected}",
            current.entity_id()
        )));
    }
    Ok(())
}

pub(crate) fn gem_item_from_verified(body: &VerifiedBody) -> Result<GemItemState> {
    let payload = body.payload().as_gem().ok_or_else(|| {
        PrecompileError::Fatal("compressed-entity read returned a non-gem payload".into())
    })?;
    crate::repository::from_canonical_item(payload.clone())
        .map_err(|error| PrecompileError::Fatal(format!("corrupt gem body: {error}")))
}

pub(crate) fn gem_bucket_from_verified(body: &VerifiedBody) -> Result<GemBucketState> {
    let payload = body.payload().as_gem_bucket().ok_or_else(|| {
        PrecompileError::Fatal("compressed-entity read returned a non-gem-bucket payload".into())
    })?;
    crate::repository::from_canonical_bucket(payload.clone())
        .map_err(|error| PrecompileError::Fatal(format!("corrupt gem bucket body: {error}")))
}

/// Called buckets, queued by the hour their notice period closes in.
pub struct ExpiryHours<'a, 'storage>(pub &'a GemContract<'storage>);

outbe_primitives::impl_expiry_queue!(ExpiryHours<B256> {
    root: expiry_tree_root,
    mid: expiry_tree_mid,
    leaf: expiry_tree_leaf,
    len: expiry_bucket_len,
    live: expiry_bucket_live,
    at: expiry_bucket_at,
    slot: called_bucket_slot,
    deadline: called_deadline,
    sweep_bucket: expiry_sweep_hour,
    cursor: expiry_cursor,
});

/// The uncalled buckets of one reference currency, by call price.
pub struct CallBins<'a, 'storage>(pub &'a GemContract<'storage>, pub u16);

outbe_primitives::impl_call_bins!(CallBins<B256> {
    root: call_bin_tree_root,
    mid: call_bin_tree_mid,
    leaf: call_bin_tree_leaf,
    count: call_bin_count,
    at: call_bin_buckets,
    slot: call_bucket_slot,
    cursor: call_bin_cursor,
    failed: call_scan_failed_day,
});

/// A broken on-chain index is the same on every node, so it reverts: the sweep then
/// defers the entry instead of failing every block.
fn corrupt(message: String) -> PrecompileError {
    PrecompileError::Revert(message)
}
