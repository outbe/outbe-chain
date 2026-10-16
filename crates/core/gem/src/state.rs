use alloy_primitives::{keccak256, B256, U256};

use outbe_primitives::call_bins;
use outbe_primitives::error::Result;
use outbe_primitives::expiry_queue;

use crate::{
    constants::{TOKEN_NAME, TOKEN_SYMBOL},
    errors::{GemError, GemFatalError},
    schema::{CallTerms, GemContract},
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
        let next = index
            .checked_add(1)
            .ok_or(GemFatalError::BucketMemberIndexOverflow { bucket })?;
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
            .ok_or(GemError::EmptyBucket { bucket })?;
        if index > last
            || self
                .bucket_gems
                .read(&Self::bucket_member_key(bucket, index))?
                != gem_id
        {
            return Err(GemError::GemNotInBucket { gem_id, bucket }.into());
        }
        let last_key = Self::bucket_member_key(bucket, last);
        if index != last {
            let moved = self.bucket_gems.read(&last_key)?;
            if moved.is_zero() {
                return Err(GemError::EmptyBucketSlotOnRemoval { bucket, slot: last }.into());
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

    /// Reads back the terms `seal_bucket_call_terms` sealed at issuance.
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
    pub(crate) fn close_bucket(&mut self, bucket: B256) -> Result<()> {
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
    pub(crate) fn insert_call_bin(&mut self, bucket: B256) -> Result<()> {
        let price = self.bucket_call_price_minor.read(&bucket)?;
        if price.is_zero() {
            return Ok(());
        }
        let iso = self.bucket_currency.read(&bucket)?;
        let bin = call_bins::price_to_bin(price)?;
        call_bins::insert(&CallBins(self, iso), bucket, bin)
    }

    /// No-op for a bucket the trie no longer holds.
    pub(crate) fn remove_call_bin(&mut self, bucket: B256) -> Result<()> {
        let iso = self.bucket_currency.read(&bucket)?;
        call_bins::remove(&CallBins(self, iso), bucket)?;
        Ok(())
    }
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
