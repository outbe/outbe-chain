use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::math::{
    reference_price,
    tree_math::{self, BinTreeStorage},
};

use crate::{
    constants::{BIN_STEP_BP, TOKEN_NAME, TOKEN_SYMBOL},
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
        // A bucket member's record stays Issued; its Called state is the bucket's.
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
        Ok(crate::metadata::token_uri(&item, qualified))
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

        if item.call_window_seconds
            > self
                .max_call_window_seconds
                .read(&item.reference_currency)?
        {
            self.max_call_window_seconds
                .write(&item.reference_currency, item.call_window_seconds)?;
        }

        self.emit(IGem::Transfer {
            from: Address::ZERO,
            to: item.owner,
            tokenId: item.gem_id,
        })
    }

    pub(crate) fn burn(&mut self, item: &GemData) -> Result<()> {
        self.gem_items.delete(item.gem_id)?;

        self.remove_called(item.gem_id)?;
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
        self.remove_called(gem_id)?;
        self.leave_bucket(gem_id)?;
        item.settled_at = self.storage.timestamp()?.to::<u64>();
        item.state = new_state as u8;
        self.gem_items.update(&item)?;
        self.emit(IGem::MetadataUpdate { _tokenId: gem_id })
    }

    pub(crate) fn push_called(&mut self, gem_id: U256, deadline: u64) -> Result<()> {
        self.place_in_expiry_hour(gem_id, Self::deadline_hour(deadline))?;
        self.called_deadline.write(&gem_id, deadline)
    }

    /// Put a called bucket or Called gem back in the queue at its own deadline, never
    /// before the next hour; an entry that is neither leaves the queue instead.
    pub(crate) fn requeue_or_drop(&mut self, entry: U256, now: u64) -> Result<bool> {
        let bucket = self.called_bucket(entry)?;
        let deadline = match bucket {
            Some(bucket) => Some(self.bucket_deadline(bucket)?),
            None => self
                .gem_items
                .get(entry)?
                .filter(|item| item.state == GemState::Called as u8)
                .map(|item| item.called_at + u64::from(item.call_notice_period_seconds)),
        };
        self.remove_called(entry)?;
        let Some(deadline) = deadline else {
            return Ok(false);
        };
        let day = Self::deadline_hour(deadline).max(Self::deadline_hour(now) + 1);
        self.place_in_expiry_hour(entry, day)?;
        self.called_deadline.write(&entry, deadline)?;
        let retry_at = Self::hour_end(day);
        match bucket {
            Some(bucket) => self.emit(IGem::GemBucketExpiryDeferred {
                bucketKey: bucket,
                retryAt: retry_at,
            })?,
            None => self.emit(IGem::GemExpiryDeferred {
                gemId: entry,
                retryAt: retry_at,
            })?,
        }
        Ok(true)
    }

    fn place_in_expiry_hour(&mut self, gem_id: U256, day: u32) -> Result<()> {
        let slot = self.expiry_bucket_len.read(&day)?;
        self.expiry_bucket_at
            .write(&Self::hour_slot_key(day, slot), gem_id)?;
        self.expiry_bucket_len.write(&day, slot.saturating_add(1))?;
        self.called_bucket_slot
            .write(&gem_id, Self::packed_slot(day, slot))?;

        let live = self.expiry_bucket_live.read(&day)?;
        self.expiry_bucket_live
            .write(&day, live.saturating_add(1))?;
        if live == 0 {
            tree_math::add(&ExpiryDayTree(&*self), day)?;
        }
        Ok(())
    }

    pub(crate) fn remove_called(&mut self, gem_id: U256) -> Result<()> {
        let packed = self.called_bucket_slot.read(&gem_id)?;
        self.called_bucket_slot.clear(&gem_id)?;
        self.called_deadline.clear(&gem_id)?;
        if packed == 0 {
            return Ok(());
        }
        let (day, slot) = Self::unpack_slot(packed);
        self.release_expiry_slot(day, slot, gem_id)
    }

    /// Free one bucket slot, retiring the bucket once nothing waits in it.
    pub(crate) fn release_expiry_slot(&mut self, day: u32, slot: u32, gem_id: U256) -> Result<()> {
        let slot_key = Self::hour_slot_key(day, slot);
        if self.expiry_bucket_at.read(&slot_key)? != gem_id {
            return Ok(());
        }
        self.expiry_bucket_at.clear(&slot_key)?;

        let live = self.expiry_bucket_live.read(&day)?.saturating_sub(1);
        self.expiry_bucket_live.write(&day, live)?;
        if live == 0 {
            self.expiry_bucket_len.clear(&day)?;
            self.expiry_bucket_live.clear(&day)?;
            tree_math::remove(&ExpiryDayTree(&*self), day)?;
            // The cursor names a slot in a length that no longer exists; a refill of
            // this day would otherwise resume past its new end.
            if self.expiry_sweep_day.read()? == day {
                self.expiry_sweep_day.write(0)?;
                self.expiry_cursor.write(0)?;
            }
        }
        Ok(())
    }

    /// Hour since the epoch a deadline falls in: plain UTC, not a WorldwideDay.
    pub(crate) const fn deadline_hour(deadline: u64) -> u32 {
        (deadline / 3_600) as u32
    }

    pub(crate) const fn hour_end(day: u32) -> u64 {
        (day as u64 + 1) * 3_600
    }

    const fn packed_slot(day: u32, slot: u32) -> u64 {
        ((day as u64) << 32) | slot as u64
    }

    const fn unpack_slot(packed: u64) -> (u32, u32) {
        ((packed >> 32) as u32, (packed & 0xffff_ffff) as u32)
    }

    pub(crate) fn hour_slot_key(day: u32, slot: u32) -> B256 {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&day.to_be_bytes());
        buf[4..8].copy_from_slice(&slot.to_be_bytes());
        keccak256(buf)
    }

    pub(crate) fn expiry_slot(&self, day: u32, slot: u32) -> Result<Option<U256>> {
        let id = self
            .expiry_bucket_at
            .read(&Self::hour_slot_key(day, slot))?;
        Ok((!id.is_zero()).then_some(id))
    }

    /// Retire a bucket the sweep has finished: a Called gem still in it moves on, a
    /// stale entry goes. Returns how many were deferred and dropped.
    pub(crate) fn force_retire_hour(&mut self, day: u32, now: u64) -> Result<(u32, u32)> {
        let len = self.expiry_bucket_len.read(&day)?;
        let (mut deferred, mut dropped) = (0u32, 0u32);
        for slot in 0..len {
            let gem_id = self
                .expiry_bucket_at
                .read(&Self::hour_slot_key(day, slot))?;
            if gem_id.is_zero() {
                continue;
            }
            if self.requeue_or_drop(gem_id, now)? {
                deferred += 1;
            } else {
                dropped += 1;
            }
        }
        self.expiry_bucket_len.clear(&day)?;
        self.expiry_bucket_live.clear(&day)?;
        tree_math::remove(&ExpiryDayTree(&*self), day)?;
        if self.expiry_sweep_day.read()? == day {
            self.expiry_sweep_day.write(0)?;
            self.expiry_cursor.write(0)?;
        }
        Ok((deferred, dropped))
    }

    pub(crate) fn first_expiry_day(&self) -> Result<Option<u32>> {
        tree_math::find_first_left_inclusive(&ExpiryDayTree(self), 0)
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
            call_price: self.bucket_call_price.read(&bucket)?,
            call_window: self.bucket_call_window.read(&bucket)?,
            call_threshold: self.bucket_call_threshold.read(&bucket)?,
            call_notice_period: self.bucket_call_notice_period.read(&bucket)?,
        })
    }

    fn seal_bucket(&mut self, bucket: B256, terms: &BucketTerms) -> Result<()> {
        self.bucket_start_day.write(&bucket, terms.start_day)?;
        self.bucket_currency
            .write(&bucket, terms.reference_currency)?;
        self.bucket_call_price.write(&bucket, terms.call_price)?;
        self.bucket_call_window.write(&bucket, terms.call_window)?;
        self.bucket_call_threshold
            .write(&bucket, terms.call_threshold)?;
        self.bucket_call_notice_period
            .write(&bucket, terms.call_notice_period)?;
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
        let deadline = now + u64::from(terms.call_notice_period);
        self.push_called(bucket_entry(bucket), deadline)?;
        self.emit(IGem::GemBucketCalled {
            bucketKey: bucket,
            calledAt: now,
            settlementDeadline: deadline,
        })
    }

    /// Take a member out of its called bucket and queue it on its own, as a Called gem,
    /// no earlier than the next hour: one gem that cannot burn must not hold back the rest.
    pub(crate) fn detach_called_member(&mut self, gem_id: U256, now: u64) -> Result<()> {
        let bucket = self.gem_bucket.read(&gem_id)?;
        let called_at = self.bucket_called_at.read(&bucket)?;
        if called_at == 0 {
            return Err(GemError::InvalidState.into());
        }
        let mut item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
        self.leave_bucket(gem_id)?;
        item.state = GemState::Called as u8;
        item.called_at = called_at;
        self.gem_items.update(&item)?;
        self.requeue_or_drop(gem_id, now)?;
        Ok(())
    }

    /// The called bucket an expiry-queue entry stands for; `None` for a gem id.
    pub(crate) fn called_bucket(&self, entry: U256) -> Result<Option<B256>> {
        let bucket = B256::from(entry.to_be_bytes::<32>());
        Ok((self.bucket_called_at.read(&bucket)? != 0).then_some(bucket))
    }

    /// Settlement deadline of a called bucket.
    pub(crate) fn bucket_deadline(&self, bucket: B256) -> Result<u64> {
        Ok(self.bucket_called_at.read(&bucket)?
            + u64::from(self.bucket_call_notice_period.read(&bucket)?))
    }

    fn close_bucket(&mut self, bucket: B256) -> Result<()> {
        let terms = self.read_bucket_terms(bucket)?;
        self.remove_bucket_bin(bucket, &terms)?;
        self.remove_called(bucket_entry(bucket))?;
        self.bucket_start_day.clear(&bucket)?;
        self.bucket_currency.clear(&bucket)?;
        self.bucket_call_price.clear(&bucket)?;
        self.bucket_call_window.clear(&bucket)?;
        self.bucket_call_threshold.clear(&bucket)?;
        self.bucket_call_notice_period.clear(&bucket)?;
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
        let iso = terms.reference_currency;
        let bin = Self::price_to_bin(terms.call_price)?;
        let scoped = Self::scoped(iso, bin);
        let index = self.bucket_bin_count.read(&scoped)?;
        self.bucket_bin_at
            .write(&Self::bin_index_key(iso, bin, index), bucket)?;
        self.bucket_bin_index.write(&bucket, index + 1)?;
        self.bucket_bin_count.write(&scoped, index + 1)?;
        tree_math::add(&BucketBins(self, iso), bin)?;
        Ok(())
    }

    /// No-op for a bucket the trie no longer holds.
    pub(crate) fn remove_bucket_bin(&mut self, bucket: B256, terms: &BucketTerms) -> Result<()> {
        let Some(index) = self.bucket_bin_index.read(&bucket)?.checked_sub(1) else {
            return Ok(());
        };
        let iso = terms.reference_currency;
        let bin = Self::price_to_bin(terms.call_price)?;
        let scoped = Self::scoped(iso, bin);
        let last = self
            .bucket_bin_count
            .read(&scoped)?
            .checked_sub(1)
            .ok_or_else(|| corrupt(format!("call bin {bin} of {iso} is empty")))?;
        let last_key = Self::bin_index_key(iso, bin, last);
        if index != last {
            let moved = self.bucket_bin_at.read(&last_key)?;
            self.bucket_bin_at
                .write(&Self::bin_index_key(iso, bin, index), moved)?;
            self.bucket_bin_index.write(&moved, index + 1)?;
        }
        self.bucket_bin_at.clear(&last_key)?;
        self.bucket_bin_index.clear(&bucket)?;
        self.bucket_bin_count.write(&scoped, last)?;
        if last == 0 {
            tree_math::remove(&BucketBins(self, iso), bin)?;
        }
        Ok(())
    }

    // --- Bin keys (PancakeSwap LB-style) ----------------------------------

    pub fn price_to_bin(price: U256) -> Result<u32> {
        if price.is_zero() {
            return Ok(0);
        }
        reference_price::coen_iso_price_to_bin_id(price, BIN_STEP_BP)
    }

    /// Namespaces a bin-column key by the gem's reference currency.
    ///
    /// Mapping keys are left-padded to 32 bytes before hashing, so a wider
    /// integer type alone namespaces nothing - the ISO has to occupy real high
    /// bits. Bin ids are 24-bit and the trie's mid/leaf keys are 16-bit, so the
    /// low 32 bits always hold `key` unambiguously.
    pub(crate) const fn scoped(reference_currency: u16, key: u32) -> u64 {
        ((reference_currency as u64) << 32) | key as u64
    }

    pub(crate) fn bin_index_key(reference_currency: u16, bin_id: u32, index: u32) -> B256 {
        let mut buf = [0u8; 10];
        buf[0..2].copy_from_slice(&reference_currency.to_be_bytes());
        buf[2..6].copy_from_slice(&bin_id.to_be_bytes());
        buf[6..10].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }
}

/// Buckets holding a called gem whose notice period has not closed yet.
pub(crate) struct ExpiryDayTree<'a, 'storage>(pub(crate) &'a GemContract<'storage>);

impl BinTreeStorage for ExpiryDayTree<'_, '_> {
    fn read_root(&self) -> Result<U256> {
        self.0.expiry_tree_root.read()
    }
    fn write_root(&self, value: U256) -> Result<()> {
        self.0.expiry_tree_root.write(value)
    }
    fn read_mid(&self, key: u32) -> Result<U256> {
        self.0.expiry_tree_mid.read(&key)
    }
    fn write_mid(&self, key: u32, value: U256) -> Result<()> {
        self.0.expiry_tree_mid.write(&key, value)
    }
    fn read_leaf(&self, key: u32) -> Result<U256> {
        self.0.expiry_tree_leaf.read(&key)
    }
    fn write_leaf(&self, key: u32, value: U256) -> Result<()> {
        self.0.expiry_tree_leaf.write(&key, value)
    }
}

/// The uncalled buckets of one reference currency, by call price.
pub(crate) struct BucketBins<'a, 'storage>(pub(crate) &'a GemContract<'storage>, pub(crate) u16);

impl BinTreeStorage for BucketBins<'_, '_> {
    fn read_root(&self) -> Result<U256> {
        self.0.bucket_bin_tree_root.read(&self.1)
    }
    fn write_root(&self, value: U256) -> Result<()> {
        self.0.bucket_bin_tree_root.write(&self.1, value)
    }
    fn read_mid(&self, key: u32) -> Result<U256> {
        self.0
            .bucket_bin_tree_mid
            .read(&GemContract::scoped(self.1, key))
    }
    fn write_mid(&self, key: u32, value: U256) -> Result<()> {
        self.0
            .bucket_bin_tree_mid
            .write(&GemContract::scoped(self.1, key), value)
    }
    fn read_leaf(&self, key: u32) -> Result<U256> {
        self.0
            .bucket_bin_tree_leaf
            .read(&GemContract::scoped(self.1, key))
    }
    fn write_leaf(&self, key: u32, value: U256) -> Result<()> {
        self.0
            .bucket_bin_tree_leaf
            .write(&GemContract::scoped(self.1, key), value)
    }
}

/// A called bucket's entry in the expiry queue, which otherwise holds gem ids.
pub(crate) fn bucket_entry(bucket: B256) -> U256 {
    U256::from_be_bytes(bucket.0)
}

/// A broken on-chain index is the same on every node, so it reverts: the sweep then
/// defers the entry instead of failing every block.
fn corrupt(message: String) -> PrecompileError {
    PrecompileError::Revert(message)
}

/// An Issued record waits for a call, unless its bucket was called.
fn is_callable(state: u8) -> bool {
    state == GemState::Issued as u8
}
