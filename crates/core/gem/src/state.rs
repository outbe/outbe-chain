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

    // --- Group member index ----------------------------------------------------

    pub fn group_member_key(group: B256, index: u32) -> B256 {
        let mut buf = [0u8; 36];
        buf[0..32].copy_from_slice(group.as_slice());
        buf[32..36].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }

    /// Appends `gem_id` and increments the group's unpaid count.
    pub(crate) fn insert_group_member(&mut self, group: B256, gem_id: U256) -> Result<()> {
        let index = self.group_gem_count.read(&group)?;
        let next = index
            .checked_add(1)
            .ok_or(GemFatalError::GroupMemberIndexOverflow { group })?;
        self.group_gems
            .write(&Self::group_member_key(group, index), gem_id)?;
        self.group_gem_index.write(&gem_id, index)?;
        self.group_gem_count.write(&group, next)?;
        Ok(())
    }

    /// Swap-removes `gem_id` and returns the remaining unpaid count. The group's
    /// terms stay: paid members may still need them.
    pub(crate) fn remove_group_member(&mut self, group: B256, gem_id: U256) -> Result<u32> {
        let index = self.group_gem_index.read(&gem_id)?;
        let last = self
            .group_gem_count
            .read(&group)?
            .checked_sub(1)
            .ok_or(GemError::EmptyGroup { group })?;
        if index > last
            || self
                .group_gems
                .read(&Self::group_member_key(group, index))?
                != gem_id
        {
            return Err(GemError::GemNotInGroup { gem_id, group }.into());
        }
        let last_key = Self::group_member_key(group, last);
        if index != last {
            let moved = self.group_gems.read(&last_key)?;
            if moved.is_zero() {
                return Err(GemError::EmptyGroupSlotOnRemoval { group, slot: last }.into());
            }
            self.group_gems
                .write(&Self::group_member_key(group, index), moved)?;
            self.group_gem_index.write(&moved, index)?;
        }
        self.group_gems.clear(&last_key)?;
        self.group_gem_index.clear(&gem_id)?;
        self.group_gem_count.write(&group, last)?;
        Ok(last)
    }

    // --- Sealed terms ------------------------------------------------------------

    /// Writes the call terms a new group sealed at issuance. Later gems that join the
    /// same group inherit this copy. Nothing reads the profile again.
    pub(crate) fn seal_group_call_terms(&mut self, group: B256, terms: CallTerms) -> Result<()> {
        self.seal_group_prices(group, &terms)?;
        self.seal_group_calendar(group, &terms)?;
        outbe_primitives::call_breach::widen_scan_terms(
            &self.max_call_window_seconds,
            &self.min_call_threshold_seconds,
            terms.reference_currency,
            terms.call_window_seconds,
            terms.call_threshold_seconds,
        )
    }
    fn seal_group_prices(&self, group: B256, terms: &CallTerms) -> Result<()> {
        self.group_call_price_minor
            .write(&group, terms.call_price_minor)?;
        self.group_currency
            .write(&group, terms.reference_currency)?;
        self.group_call_rate.write(&group, terms.call_rate)?;
        Ok(())
    }

    fn seal_group_calendar(&self, group: B256, terms: &CallTerms) -> Result<()> {
        self.group_call_window_seconds
            .write(&group, terms.call_window_seconds)?;
        self.group_call_threshold_seconds
            .write(&group, terms.call_threshold_seconds)?;
        self.group_call_notice_period_seconds
            .write(&group, terms.call_notice_period_seconds)?;
        Ok(())
    }

    /// Reads back the terms `seal_group_call_terms` sealed at issuance.
    pub fn read_call_terms(&self, group: B256) -> Result<CallTerms> {
        Ok(CallTerms {
            call_price_minor: self.group_call_price_minor.read(&group)?,
            reference_currency: self.group_currency.read(&group)?,
            call_rate: self.group_call_rate.read(&group)?,
            call_window_seconds: self.group_call_window_seconds.read(&group)?,
            call_threshold_seconds: self.group_call_threshold_seconds.read(&group)?,
            call_notice_period_seconds: self.group_call_notice_period_seconds.read(&group)?,
        })
    }

    /// Records every unpaid member called at once: the group leaves the trie for the
    /// expiry queue, where it waits on `deadline` as one entry.
    pub fn record_group_called(&mut self, group: B256, now: u64, deadline: u64) -> Result<()> {
        self.remove_call_bin(group)?;
        self.group_called_at.write(&group, now)?;
        self.push_called(group, deadline)
    }

    /// Forgets a group nothing refers to any more: its terms, its place in the call
    /// trie and the expiry queue, its day and its call.
    pub(crate) fn close_group(&mut self, group: B256) -> Result<()> {
        self.remove_call_bin(group)?;
        self.remove_called(group)?;
        self.group_call_price_minor.clear(&group)?;
        self.group_currency.clear(&group)?;
        self.group_call_rate.clear(&group)?;
        self.group_call_window_seconds.clear(&group)?;
        self.group_call_threshold_seconds.clear(&group)?;
        self.group_call_notice_period_seconds.clear(&group)?;
        self.group_issued_at.clear(&group)?;
        self.group_called_at.clear(&group)?;
        self.group_gem_count.clear(&group)
    }

    // --- Expiry queue -------------------------------------------------------------

    pub fn push_called(&mut self, group: B256, deadline: u64) -> Result<()> {
        expiry_queue::push(&ExpiryHours(self), group, deadline)
    }

    pub fn remove_called(&mut self, group: B256) -> Result<()> {
        expiry_queue::remove(&ExpiryHours(self), group)
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

    // --- Group bins: uncalled groups by call price --------------------------------

    /// Parks a group in the bin of its sealed call price. No-op for a group without
    /// a call price.
    pub(crate) fn insert_call_bin(&mut self, group: B256) -> Result<()> {
        let price = self.group_call_price_minor.read(&group)?;
        if price.is_zero() {
            return Ok(());
        }
        let iso = self.group_currency.read(&group)?;
        let bin = call_bins::price_to_bin(price)?;
        call_bins::insert(&CallBins(self, iso), group, bin)
    }

    /// No-op for a group the trie no longer holds.
    pub(crate) fn remove_call_bin(&mut self, group: B256) -> Result<()> {
        let iso = self.group_currency.read(&group)?;
        call_bins::remove(&CallBins(self, iso), group)?;
        Ok(())
    }
}

/// Called groups, queued by the hour their notice period closes in.
pub struct ExpiryHours<'a, 'storage>(pub &'a GemContract<'storage>);

outbe_primitives::impl_expiry_queue!(ExpiryHours<B256> {
    root: expiry_tree_root,
    mid: expiry_tree_mid,
    leaf: expiry_tree_leaf,
    len: expiry_bucket_len,
    live: expiry_bucket_live,
    at: expiry_bucket_at,
    slot: called_group_slot,
    deadline: called_deadline,
    sweep_bucket: expiry_sweep_hour,
    cursor: expiry_cursor,
});

/// The uncalled groups of one reference currency, by call price.
pub struct CallBins<'a, 'storage>(pub &'a GemContract<'storage>, pub u16);

outbe_primitives::impl_call_bins!(CallBins<B256> {
    root: call_bin_tree_root,
    mid: call_bin_tree_mid,
    leaf: call_bin_tree_leaf,
    count: call_bin_count,
    at: call_bin_groups,
    slot: call_group_slot,
    cursor: call_bin_cursor,
    failed: call_scan_failed_day,
});
