use alloy_primitives::B256;
use outbe_primitives::call_bins;
use outbe_primitives::error::Result;
use outbe_primitives::expiry_queue;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use std::collections::BTreeSet;

use crate::{
    errors::NodError,
    precompile::INod,
    schema::{CallTerms, NodContract},
};

impl NodContract<'_> {
    /// A Nod id carries its Worldwide Day as a prefix, so each day is one id range.
    pub fn emit_days_metadata_update(&mut self, days: &BTreeSet<u32>) -> Result<()> {
        for &day in days {
            let day = WorldwideDay::new(day);
            self.emit(INod::BatchMetadataUpdate {
                _fromTokenId: WwdEntityId::from_day_and_digest(day, B256::ZERO).to_u256(),
                _toTokenId: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(0xff))
                    .to_u256(),
            })?;
        }
        Ok(())
    }

    // --- ID helpers ---

    // --- View functions ---

    pub fn total_supply(&self) -> Result<u64> {
        self.total_supply.read()
    }

    // --- Bin index helpers (PancakeSwap LB-style ladder) -------------------

    /// Parks a new bucket in the bin of its sealed call price.
    pub fn insert_call_bin(&mut self, bucket_key: B256) -> Result<()> {
        let iso = self.bucket_currency.read(&bucket_key)?;
        let bin = outbe_primitives::call_bins::price_to_bin(
            self.bucket_call_price_minor.read(&bucket_key)?,
        )?;
        call_bins::insert(&CallBins(self, iso), bucket_key, bin)
    }

    /// No-op for a bucket the trie does not hold.
    pub fn remove_call_bin(&mut self, bucket_key: B256) -> Result<()> {
        let iso = self.bucket_currency.read(&bucket_key)?;
        call_bins::remove(&CallBins(self, iso), bucket_key)?;
        Ok(())
    }

    // --- Bucket member index ------------------------------------------------
    //
    // The forfeit sweep enumerates unpaid members with a count, positional map
    // and reverse map for O(1) swap removal.

    /// Appends `nod_id` and increments the bucket's authoritative unpaid count.
    pub(crate) fn insert_bucket_member(
        &mut self,
        bucket_key: B256,
        nod_id: WwdEntityId,
    ) -> Result<()> {
        let index = self.bucket_nod_count.read(&bucket_key)?;
        let next = index
            .checked_add(1)
            .ok_or(NodError::BucketMemberIndexOverflow { bucket_key })?;
        self.bucket_nods.write(
            &crate::index_keys::bucket_nod_key(bucket_key, index),
            nod_id,
        )?;
        self.bucket_nod_index.write(&nod_id, index)?;
        self.bucket_nod_count.write(&bucket_key, next)?;
        Ok(())
    }

    /// Swap-removes `nod_id` and returns the remaining member count.
    pub(crate) fn remove_bucket_member(
        &mut self,
        bucket_key: B256,
        nod_id: WwdEntityId,
    ) -> Result<u32> {
        let index = self.bucket_nod_index.read(&nod_id)?;
        let last = self
            .bucket_nod_count
            .read(&bucket_key)?
            .checked_sub(1)
            .ok_or(NodError::BucketMemberCountUnderflowDuringRemoval { bucket_key })?;
        if index > last
            || self
                .bucket_nods
                .read(&crate::index_keys::bucket_nod_key(bucket_key, index))?
                != nod_id
        {
            return Err(NodError::NodNotIndexedInBucket { nod_id, bucket_key }.into());
        }
        let last_key = crate::index_keys::bucket_nod_key(bucket_key, last);
        if index != last {
            let moved = self.bucket_nods.read(&last_key)?;
            if moved.is_zero() {
                return Err(NodError::EmptyBucketMemberSlotDuringRemoval {
                    bucket_key,
                    slot: last,
                }
                .into());
            }
            self.bucket_nods
                .write(&crate::index_keys::bucket_nod_key(bucket_key, index), moved)?;
            self.bucket_nod_index.write(&moved, index)?;
        }
        self.bucket_nods.write(&last_key, WwdEntityId::ZERO)?;
        self.bucket_nod_index.clear(&nod_id)?;
        self.bucket_nod_count.write(&bucket_key, last)?;
        Ok(last)
    }

    // --- Callable-bucket index ----------------------------------------------

    /// Writes the call terms a new bucket sealed at issuance. Later Nods that
    /// join the same bucket inherit this copy. Nothing reads the constants again.
    pub(crate) fn seal_bucket_call_terms(
        &mut self,
        bucket_key: B256,
        terms: CallTerms,
    ) -> Result<()> {
        self.bucket_call_price_minor
            .write(&bucket_key, terms.call_price_minor)?;
        self.bucket_currency
            .write(&bucket_key, terms.reference_currency)?;
        self.bucket_call_rate.write(&bucket_key, terms.call_rate)?;
        self.bucket_call_window_seconds
            .write(&bucket_key, terms.call_window_seconds)?;
        self.bucket_call_threshold_seconds
            .write(&bucket_key, terms.call_threshold_seconds)?;
        self.bucket_call_notice_period_seconds
            .write(&bucket_key, terms.call_notice_period_seconds)?;
        outbe_primitives::call_breach::widen_scan_terms(
            &self.max_call_window_seconds,
            &self.min_call_threshold_seconds,
            terms.reference_currency,
            terms.call_window_seconds,
            terms.call_threshold_seconds,
        )
    }

    /// Queues a called bucket on the deadline its notice period closes at.
    pub fn push_called(&mut self, bucket_key: B256, deadline: u64) -> Result<()> {
        expiry_queue::push(&ExpiryHours(self), bucket_key, deadline)
    }

    /// Records every unpaid member called at once: the bucket leaves the trie for the
    /// expiry queue, where it waits on `deadline` as one entry.
    pub fn record_bucket_called(
        &mut self,
        bucket_key: B256,
        now: u64,
        deadline: u64,
    ) -> Result<()> {
        self.remove_call_bin(bucket_key)?;
        self.bucket_called_at.write(&bucket_key, now)?;
        self.push_called(bucket_key, deadline)
    }

    /// Reads back the terms `seal_bucket_call_terms` sealed at issuance.
    pub fn read_call_terms(&self, bucket_key: B256) -> Result<CallTerms> {
        Ok(CallTerms {
            call_price_minor: self.bucket_call_price_minor.read(&bucket_key)?,
            reference_currency: self.bucket_currency.read(&bucket_key)?,
            call_rate: self.bucket_call_rate.read(&bucket_key)?,
            call_window_seconds: self.bucket_call_window_seconds.read(&bucket_key)?,
            call_threshold_seconds: self.bucket_call_threshold_seconds.read(&bucket_key)?,
            call_notice_period_seconds: self.bucket_call_notice_period_seconds.read(&bucket_key)?,
        })
    }

    /// No-op for a bucket the queue does not hold.
    pub fn remove_called(&mut self, bucket_key: B256) -> Result<()> {
        expiry_queue::remove(&ExpiryHours(self), bucket_key)
    }

    /// No-op for a bucket the call index never held, so the removal funnel can call it
    /// unconditionally.
    pub(crate) fn close_bucket(&mut self, bucket_key: B256) -> Result<()> {
        self.remove_call_bin(bucket_key)?;
        self.remove_called(bucket_key)?;
        self.bucket_call_price_minor.clear(&bucket_key)?;
        self.bucket_currency.clear(&bucket_key)?;
        self.bucket_call_rate.clear(&bucket_key)?;
        self.bucket_call_window_seconds.clear(&bucket_key)?;
        self.bucket_call_threshold_seconds.clear(&bucket_key)?;
        self.bucket_call_notice_period_seconds.clear(&bucket_key)?;
        self.bucket_issued_at.clear(&bucket_key)?;
        self.bucket_called_at.clear(&bucket_key)?;
        Ok(())
    }
}

/// One currency's call-price trie, like `outbe_gem::state::CallBins`.
pub struct CallBins<'a, 'storage>(pub &'a NodContract<'storage>, pub u16);

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

/// Called buckets, queued by the hour their notice period closes in.
pub struct ExpiryHours<'a, 'storage>(pub &'a NodContract<'storage>);

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
