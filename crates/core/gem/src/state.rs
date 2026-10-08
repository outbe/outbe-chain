use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::call_bins;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::expiry_queue;

use crate::{
    constants::{TOKEN_NAME, TOKEN_SYMBOL},
    errors::GemError,
    precompile::IGem,
    schema::{BucketTerms, GemContract, GemData, GemState},
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

    pub fn balance_of(&self, owner: Address) -> Result<u32> {
        self.owner_gem_counts.read(&owner)
    }

    pub fn owner_of(&self, gem_id: U256) -> Result<Address> {
        let item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
        Ok(item.owner)
    }

    pub fn get_gem(&self, gem_id: U256) -> Result<Option<GemData>> {
        let Some(mut item) = self.gem_items.get(gem_id)? else {
            return Ok(None);
        };
        // A bucket member's record stays Issued. Its Called state is the bucket's.
        if is_callable(item.state) {
            let bucket = self.gem_bucket.read(&gem_id)?;
            if !bucket.is_zero() {
                let called_at = self.bucket_called_at.read(&bucket)?;
                if called_at != 0 {
                    item.state = GemState::Called as u8;
                    item.called_at = called_at;
                }
            }
        }
        Ok(Some(item))
    }

    pub fn token_by_index(&self, index: u32) -> Result<U256> {
        self.all_gem_ids
            .get(index)?
            .ok_or_else(|| GemError::IndexOutOfBounds.into())
    }

    pub fn token_of_owner_by_index(&self, owner: Address, index: u32) -> Result<U256> {
        let count = self.owner_gem_counts.read(&owner)?;
        if index >= count {
            return Err(GemError::IndexOutOfBounds.into());
        }
        self.owner_gem_ids
            .read(&Self::owner_index_key(owner, index))
    }

    pub fn token_uri(&self, gem_id: U256) -> Result<String> {
        let item = self.get_gem(gem_id)?.ok_or(GemError::GemNotFound)?;
        let qualified = is_callable(item.state) && crate::api::is_qualified(&self.storage, &item)?;
        let now = self.storage.timestamp()?.to::<u64>();
        Ok(crate::metadata::token_uri(&item, qualified, now))
    }

    pub(crate) fn owner_index_key(owner: Address, index: u32) -> B256 {
        let mut buf = [0u8; 24];
        buf[0..20].copy_from_slice(owner.as_slice());
        buf[20..24].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }

    pub(crate) fn add_gem(&mut self, item: &GemData) -> Result<()> {
        if self.gem_items.exists(item.gem_id)? {
            return Err(GemError::AlreadyExists.into());
        }
        self.gem_items.create(item)?;

        let owner_count = self.owner_gem_counts.read(&item.owner)?;
        self.owner_gem_ids
            .write(&Self::owner_index_key(item.owner, owner_count), item.gem_id)?;
        self.owner_gem_counts.write(&item.owner, owner_count + 1)?;
        self.owner_gem_position
            .write(&item.gem_id, owner_count + 1)?;

        let idx = self.all_gem_ids.len()?;
        self.all_gem_ids.push(item.gem_id)?;
        self.gem_index.write(&item.gem_id, idx)?;

        let supply = self.total_supply.read()?;
        self.total_supply.write(supply + 1)?;

        if is_callable(item.state) {
            self.join_bucket(item)?;
        }

        outbe_primitives::call_breach::widen_scan_terms(
            &self.max_call_window_seconds,
            &self.min_call_threshold_seconds,
            item.reference_currency,
            item.call_window_seconds,
            item.call_threshold_seconds,
        )?;

        self.emit(IGem::Transfer {
            from: Address::ZERO,
            to: item.owner,
            tokenId: item.gem_id,
        })
    }

    pub(crate) fn burn(&mut self, item: &GemData) -> Result<()> {
        self.gem_items.delete(item.gem_id)?;

        self.leave_bucket(item.gem_id)?;

        let idx = self.gem_index.read(&item.gem_id)?;
        let last = self
            .all_gem_ids
            .len()?
            .checked_sub(1)
            .ok_or(GemError::GemNotFound)?;
        if idx != last {
            let last_id = self.all_gem_ids.get(last)?.ok_or(GemError::GemNotFound)?;
            self.all_gem_ids.set(idx, last_id)?;
            self.gem_index.write(&last_id, idx)?;
        }
        self.all_gem_ids.pop()?;
        self.gem_index.clear(&item.gem_id)?;

        self.compact_owner_index(item.owner, item.gem_id)?;

        let supply = self.total_supply.read()?;
        if supply > 0 {
            self.total_supply.write(supply - 1)?;
        }
        self.emit(IGem::Transfer {
            from: item.owner,
            to: Address::ZERO,
            tokenId: item.gem_id,
        })
    }

    /// Only `Settled` is set here: a call goes through `mark_bucket_called`, and
    /// qualification is derived.
    pub(crate) fn set_state(&mut self, gem_id: U256, new_state: GemState) -> Result<()> {
        if new_state != GemState::Settled {
            return Err(GemError::InvalidState.into());
        }
        // Read through the bucket, so a called member keeps its `called_at` once settled.
        let mut item = self.get_gem(gem_id)?.ok_or(GemError::GemNotFound)?;
        self.leave_bucket(gem_id)?;
        item.settled_at = self.storage.timestamp()?.to::<u64>();
        item.state = new_state as u8;
        self.gem_items.update(&item)?;
        self.emit(IGem::MetadataUpdate { _tokenId: gem_id })
    }

    pub(crate) fn push_called(&mut self, bucket: B256, deadline: u64) -> Result<()> {
        expiry_queue::push(&ExpiryHours(self), bucket, deadline)
    }

    pub(crate) fn remove_called(&mut self, bucket: B256) -> Result<()> {
        expiry_queue::remove(&ExpiryHours(self), bucket)
    }

    /// Hour since the epoch a deadline falls in: plain UTC, not a WorldwideDay.
    #[cfg(test)]
    pub(crate) const fn deadline_hour(deadline: u64) -> u32 {
        expiry_queue::bucket_of(deadline)
    }

    #[cfg(test)]
    pub(crate) const fn hour_end(day: u32) -> u64 {
        expiry_queue::bucket_end(day)
    }

    #[cfg(test)]
    pub(crate) fn expiry_slot(&self, day: u32, slot: u32) -> Result<Option<B256>> {
        expiry_queue::entry_at(&ExpiryHours(self), day, slot)
    }

    #[cfg(test)]
    pub(crate) fn first_expiry_day(&self) -> Result<Option<u32>> {
        expiry_queue::first_bucket(&ExpiryHours(self))
    }

    fn compact_owner_index(&mut self, owner: Address, gem_id: U256) -> Result<()> {
        let count = self.owner_gem_counts.read(&owner)?;
        let last = count.checked_sub(1).ok_or(GemError::GemNotFound)?;
        let stored = self.owner_gem_position.read(&gem_id)?.checked_sub(1);
        let idx = match stored {
            Some(idx)
                if idx < count
                    && self
                        .owner_gem_ids
                        .read(&Self::owner_index_key(owner, idx))?
                        == gem_id =>
            {
                idx
            }
            _ => self.find_in_owner_index(owner, gem_id, count)?,
        };
        let last_key = Self::owner_index_key(owner, last);
        if idx != last {
            let last_id = self.owner_gem_ids.read(&last_key)?;
            self.owner_gem_ids
                .write(&Self::owner_index_key(owner, idx), last_id)?;
            self.owner_gem_position.write(&last_id, idx + 1)?;
        }
        self.owner_gem_ids.clear(&last_key)?;
        self.owner_gem_position.clear(&gem_id)?;
        self.owner_gem_counts.write(&owner, last)?;
        Ok(())
    }

    fn find_in_owner_index(&self, owner: Address, gem_id: U256, count: u32) -> Result<u32> {
        for i in 0..count {
            if self.owner_gem_ids.read(&Self::owner_index_key(owner, i))? == gem_id {
                return Ok(i);
            }
        }
        Err(GemError::GemNotFound.into())
    }

    // --- Call buckets: membership, sealed terms and the call itself -------

    /// Add a new gem to the bucket of its terms, opening the bucket if it is the first.
    pub(crate) fn join_bucket(&mut self, item: &GemData) -> Result<B256> {
        let terms = BucketTerms::of(item);
        let bucket = terms.key();
        // Unreachable outside test hooks: a bucket is called days after its start day.
        if self.bucket_called_at.read(&bucket)? != 0 {
            return Err(GemError::InvalidState.into());
        }
        let index = self.bucket_gem_count.read(&bucket)?;
        if index == 0 {
            self.seal_bucket(bucket, &terms)?;
        }
        let next = index
            .checked_add(1)
            .ok_or_else(|| corrupt(format!("gem bucket {bucket} member index overflow")))?;
        self.bucket_gems
            .write(&Self::bucket_member_key(bucket, index), item.gem_id)?;
        self.bucket_gem_index.write(&item.gem_id, index)?;
        self.bucket_gem_count.write(&bucket, next)?;
        self.gem_bucket.write(&item.gem_id, bucket)?;
        Ok(bucket)
    }

    /// Take a gem out of its bucket, closing the bucket once it is empty. No-op for a
    /// gem without one.
    pub(crate) fn leave_bucket(&mut self, gem_id: U256) -> Result<()> {
        let bucket = self.gem_bucket.read(&gem_id)?;
        if bucket.is_zero() {
            return Ok(());
        }
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
            self.bucket_gems
                .write(&Self::bucket_member_key(bucket, index), moved)?;
            self.bucket_gem_index.write(&moved, index)?;
        }
        self.bucket_gems.clear(&last_key)?;
        self.bucket_gem_index.clear(&gem_id)?;
        self.gem_bucket.clear(&gem_id)?;
        self.bucket_gem_count.write(&bucket, last)?;
        if last == 0 {
            self.close_bucket(bucket)?;
        }
        Ok(())
    }

    pub(crate) fn read_bucket_terms(&self, bucket: B256) -> Result<BucketTerms> {
        Ok(BucketTerms {
            start_day: self.bucket_start_day.read(&bucket)?,
            reference_currency: self.bucket_currency.read(&bucket)?,
            call_price_minor: self.bucket_call_price_minor.read(&bucket)?,
            call_window_seconds: self.bucket_call_window_seconds.read(&bucket)?,
            call_threshold_seconds: self.bucket_call_threshold_seconds.read(&bucket)?,
            call_notice_period_seconds: self.bucket_call_notice_period_seconds.read(&bucket)?,
        })
    }

    fn seal_bucket(&mut self, bucket: B256, terms: &BucketTerms) -> Result<()> {
        self.bucket_start_day.write(&bucket, terms.start_day)?;
        self.bucket_currency
            .write(&bucket, terms.reference_currency)?;
        self.bucket_call_price_minor
            .write(&bucket, terms.call_price_minor)?;
        self.bucket_call_window_seconds
            .write(&bucket, terms.call_window_seconds)?;
        self.bucket_call_threshold_seconds
            .write(&bucket, terms.call_threshold_seconds)?;
        self.bucket_call_notice_period_seconds
            .write(&bucket, terms.call_notice_period_seconds)?;
        self.insert_bucket_bin(bucket, terms)
    }

    /// `Issued -> Called` for every member at once: the bucket leaves the trie for the
    /// expiry queue, where it waits as one entry.
    pub(crate) fn mark_bucket_called(
        &mut self,
        bucket: B256,
        terms: &BucketTerms,
        now: u64,
    ) -> Result<()> {
        self.remove_bucket_bin(bucket, terms)?;
        self.bucket_called_at.write(&bucket, now)?;
        let deadline = now + u64::from(terms.call_notice_period_seconds);
        self.push_called(bucket, deadline)?;
        self.emit(IGem::GemBucketCalled {
            bucketKey: bucket,
            calledAt: now,
            settlementDeadline: deadline,
        })
    }

    fn close_bucket(&mut self, bucket: B256) -> Result<()> {
        let terms = self.read_bucket_terms(bucket)?;
        self.remove_bucket_bin(bucket, &terms)?;
        self.remove_called(bucket)?;
        self.bucket_start_day.clear(&bucket)?;
        self.bucket_currency.clear(&bucket)?;
        self.bucket_call_price_minor.clear(&bucket)?;
        self.bucket_call_window_seconds.clear(&bucket)?;
        self.bucket_call_threshold_seconds.clear(&bucket)?;
        self.bucket_call_notice_period_seconds.clear(&bucket)?;
        self.bucket_called_at.clear(&bucket)
    }

    pub(crate) fn bucket_member_key(bucket: B256, index: u32) -> B256 {
        let mut buf = [0u8; 36];
        buf[0..32].copy_from_slice(bucket.as_slice());
        buf[32..36].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }

    // --- Bucket bins: uncalled buckets by call price ---------------------

    fn insert_bucket_bin(&mut self, bucket: B256, terms: &BucketTerms) -> Result<()> {
        let bin = Self::price_to_bin(terms.call_price_minor)?;
        call_bins::insert(&CallBins(self, terms.reference_currency), bucket, bin)
    }

    /// No-op for a bucket the trie no longer holds.
    pub(crate) fn remove_bucket_bin(&mut self, bucket: B256, terms: &BucketTerms) -> Result<()> {
        call_bins::remove(&CallBins(self, terms.reference_currency), bucket)?;
        Ok(())
    }

    // --- Bin keys (PancakeSwap LB-style) ----------------------------------

    pub fn price_to_bin(price: U256) -> Result<u32> {
        call_bins::price_to_bin(price)
    }
}

/// Called buckets, queued by the hour their notice period closes in.
pub(crate) struct ExpiryHours<'a, 'storage>(pub(crate) &'a GemContract<'storage>);

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
pub(crate) struct CallBins<'a, 'storage>(pub(crate) &'a GemContract<'storage>, pub(crate) u16);

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

/// An Issued record waits for a call, unless its bucket was called.
fn is_callable(state: u8) -> bool {
    state == GemState::Issued as u8
}
