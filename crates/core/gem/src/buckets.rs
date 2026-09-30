use alloy_primitives::{keccak256, B256, U256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::math::tree_math::{self, BinTreeStorage};
use outbe_primitives::time::first_full_day;

use crate::errors::GemError;
use crate::precompile::IGem;
use crate::schema::{GemContract, GemData};

/// Everything a call decision reads off a gem. Gems that share it breach on the same
/// days, so one decision covers them all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BucketTerms {
    pub(crate) start_day: u32,
    pub(crate) reference_currency: u16,
    pub(crate) call_price: U256,
    pub(crate) call_window: u32,
    pub(crate) call_threshold: u32,
    pub(crate) call_notice_period: u32,
}

impl BucketTerms {
    pub(crate) fn of(item: &GemData) -> Self {
        Self {
            start_day: first_full_day(item.issued_at),
            reference_currency: item.reference_currency,
            call_price: item.call_price_minor,
            call_window: item.call_window_seconds,
            call_threshold: item.call_threshold_seconds,
            call_notice_period: item.call_notice_period_seconds,
        }
    }

    pub(crate) fn key(&self) -> B256 {
        let mut buf = [0u8; 4 + 2 + 32 + 4 + 4 + 4];
        buf[0..4].copy_from_slice(&self.start_day.to_be_bytes());
        buf[4..6].copy_from_slice(&self.reference_currency.to_be_bytes());
        buf[6..38].copy_from_slice(&self.call_price.to_be_bytes::<32>());
        buf[38..42].copy_from_slice(&self.call_window.to_be_bytes());
        buf[42..46].copy_from_slice(&self.call_threshold.to_be_bytes());
        buf[46..50].copy_from_slice(&self.call_notice_period.to_be_bytes());
        keccak256(buf)
    }
}

impl GemContract<'_> {
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
        let next = index.checked_add(1).ok_or_else(|| {
            PrecompileError::Fatal(format!("gem bucket {bucket} member index overflow"))
        })?;
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
        })?;
        self.emit(IGem::BatchMetadataUpdate {
            _fromTokenId: U256::ZERO,
            _toTokenId: U256::MAX,
        })
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

    pub(crate) fn bucket_member_key(bucket: B256, index: u32) -> B256 {
        let mut buf = [0u8; 36];
        buf[0..32].copy_from_slice(bucket.as_slice());
        buf[32..36].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }
}

/// A called bucket's entry in the expiry queue, which otherwise holds gem ids.
pub(crate) fn bucket_entry(bucket: B256) -> U256 {
    U256::from_be_bytes(bucket.0)
}

fn corrupt(message: String) -> PrecompileError {
    PrecompileError::BodyReadCorruption(message)
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
