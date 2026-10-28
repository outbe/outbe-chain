use alloy_primitives::{B256, U256};
use outbe_primitives::call_bins;
use outbe_primitives::deadline_queue;
use outbe_primitives::error::Result;
use outbe_primitives::wwd_entity_id::WwdEntityId;

use crate::{
    errors::NodError,
    schema::{CallTerms, NodContract},
};

impl NodContract<'_> {
    // --- ID helpers ---

    // --- View functions ---

    pub fn total_supply(&self) -> Result<u64> {
        self.total_supply.read()
    }

    // --- Bin index helpers (PancakeSwap LB-style ladder) -------------------

    /// Parks a new group in the bin of its sealed call price.
    pub fn insert_call_bin(&mut self, group_key: B256) -> Result<()> {
        let iso = self.group_currency.read(&group_key)?;
        let bin = outbe_primitives::call_bins::price_to_bin(
            self.group_call_price_minor.read(&group_key)?,
        )?;
        call_bins::insert(&CallBins(self, iso), group_key, bin)
    }

    /// No-op for a group the trie does not hold.
    pub fn remove_call_bin(&mut self, group_key: B256) -> Result<()> {
        let iso = self.group_currency.read(&group_key)?;
        call_bins::remove(&CallBins(self, iso), group_key)?;
        Ok(())
    }

    // --- Group member index ------------------------------------------------
    //
    // The cleanup enumerates unpaid members with a count, positional map
    // and reverse map for O(1) swap removal.

    /// Appends `nod_id` and increments the group's authoritative unpaid count.
    pub(crate) fn insert_group_member(
        &mut self,
        group_key: B256,
        nod_id: WwdEntityId,
    ) -> Result<()> {
        let index = self.group_nod_count.read(&group_key)?;
        let next = index
            .checked_add(1)
            .ok_or(NodError::GroupMemberIndexOverflow { group_key })?;
        self.group_nods
            .write(&crate::index_keys::group_nod_key(group_key, index), nod_id)?;
        self.group_nod_index.write(&nod_id, index)?;
        self.group_nod_count.write(&group_key, next)?;
        Ok(())
    }

    /// Swap-removes `nod_id` and returns the remaining member count.
    pub(crate) fn remove_group_member(
        &mut self,
        group_key: B256,
        nod_id: WwdEntityId,
    ) -> Result<u32> {
        let index = self.group_nod_index.read(&nod_id)?;
        let last = self
            .group_nod_count
            .read(&group_key)?
            .checked_sub(1)
            .ok_or(NodError::GroupMemberCountUnderflowDuringRemoval { group_key })?;
        if index > last
            || self
                .group_nods
                .read(&crate::index_keys::group_nod_key(group_key, index))?
                != nod_id
        {
            return Err(NodError::NodNotIndexedInGroup { nod_id, group_key }.into());
        }
        let last_key = crate::index_keys::group_nod_key(group_key, last);
        if index != last {
            let moved = self.group_nods.read(&last_key)?;
            if moved.is_zero() {
                return Err(NodError::EmptyGroupMemberSlotDuringRemoval {
                    group_key,
                    slot: last,
                }
                .into());
            }
            self.group_nods
                .write(&crate::index_keys::group_nod_key(group_key, index), moved)?;
            self.group_nod_index.write(&moved, index)?;
        }
        self.group_nods.write(&last_key, WwdEntityId::ZERO)?;
        self.group_nod_index.clear(&nod_id)?;
        self.group_nod_count.write(&group_key, last)?;
        Ok(last)
    }

    // --- Callable-group index ----------------------------------------------

    /// Writes the call terms a new group sealed at issuance. Later Nods that
    /// join the same group inherit this copy. Nothing reads the constants again.
    pub(crate) fn seal_group_call_terms(
        &mut self,
        group_key: B256,
        terms: CallTerms,
    ) -> Result<()> {
        self.group_call_price_minor
            .write(&group_key, terms.call_price_minor)?;
        self.group_currency
            .write(&group_key, terms.reference_currency)?;
        self.group_call_rate.write(&group_key, terms.call_rate)?;
        self.group_call_window_seconds
            .write(&group_key, terms.call_window_seconds)?;
        self.group_call_threshold_seconds
            .write(&group_key, terms.call_threshold_seconds)?;
        self.group_call_notice_period_seconds
            .write(&group_key, terms.call_notice_period_seconds)?;
        outbe_primitives::call_breach::widen_scan_terms(
            &self.max_call_window_seconds,
            &self.min_call_threshold_seconds,
            terms.reference_currency,
            terms.call_window_seconds,
            terms.call_threshold_seconds,
        )
    }

    /// Queues a called group on the deadline its notice period closes at.
    pub fn push_called(&mut self, group_key: B256, deadline: u64) -> Result<()> {
        deadline_queue::push(&ForfeitQueue(self), group_key, deadline)
    }

    /// Records every unpaid member called at once: the group leaves the trie for the
    /// forfeit queue, where it waits on `deadline` as one entry.
    pub fn record_group_called(&mut self, group_key: B256, now: u64, deadline: u64) -> Result<()> {
        self.remove_call_bin(group_key)?;
        self.group_called_at.write(&group_key, now)?;
        self.push_called(group_key, deadline)
    }

    /// Reads back the terms `seal_group_call_terms` sealed at issuance.
    pub fn read_call_terms(&self, group_key: B256) -> Result<CallTerms> {
        Ok(CallTerms {
            call_price_minor: self.group_call_price_minor.read(&group_key)?,
            reference_currency: self.group_currency.read(&group_key)?,
            call_rate: self.group_call_rate.read(&group_key)?,
            call_window_seconds: self.group_call_window_seconds.read(&group_key)?,
            call_threshold_seconds: self.group_call_threshold_seconds.read(&group_key)?,
            call_notice_period_seconds: self.group_call_notice_period_seconds.read(&group_key)?,
        })
    }

    /// No-op for a group the queue does not hold.
    pub fn remove_called(&mut self, group_key: B256) -> Result<()> {
        deadline_queue::remove(&ForfeitQueue(self), group_key)
    }

    /// No-op for a group the call index never held, so the removal funnel can call it
    /// unconditionally.
    pub(crate) fn close_group(&mut self, group_key: B256) -> Result<()> {
        self.remove_call_bin(group_key)?;
        self.remove_called(group_key)?;
        self.group_call_price_minor.clear(&group_key)?;
        self.group_currency.clear(&group_key)?;
        self.group_call_rate.clear(&group_key)?;
        self.group_call_window_seconds.clear(&group_key)?;
        self.group_call_threshold_seconds.clear(&group_key)?;
        self.group_call_notice_period_seconds.clear(&group_key)?;
        self.group_called_at.clear(&group_key)?;
        self.group_unpaid_load.clear(&group_key)?;
        deadline_queue::remove(&CleanupQueue(self), group_key)
    }

    // --- Unpaid load ----------------------------------------------------------

    pub(crate) fn add_unpaid_load(&mut self, group_key: B256, load: U256) -> Result<()> {
        let total = self
            .group_unpaid_load
            .read(&group_key)?
            .checked_add(load)
            .ok_or(NodError::GroupUnpaidLoadOverflow { group_key })?;
        self.group_unpaid_load.write(&group_key, total)
    }

    pub(crate) fn take_unpaid_load(&mut self, group_key: B256, load: U256) -> Result<()> {
        let total = self
            .group_unpaid_load
            .read(&group_key)?
            .checked_sub(load)
            .ok_or(NodError::GroupUnpaidLoadUnderflow { group_key })?;
        self.group_unpaid_load.write(&group_key, total)
    }

    /// Moves a forfeited group from the forfeit queue to the cleanup, which deletes its
    /// unpaid Nods. Done with the group's credit, so a retry never credits it twice.
    pub fn hand_to_cleanup(&mut self, group_key: B256) -> Result<()> {
        let deadline = self.called_deadline.read(&group_key)?;
        deadline_queue::remove(&ForfeitQueue(self), group_key)?;
        deadline_queue::push(&CleanupQueue(self), group_key, deadline)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn first_cleanup_day(&self) -> Result<Option<u32>> {
        deadline_queue::first_day(&CleanupQueue(self))
    }
}

/// Forfeited groups whose unpaid Nods wait to be deleted.
pub struct CleanupQueue<'a, 'storage>(pub &'a NodContract<'storage>);

outbe_primitives::impl_deadline_queue!(CleanupQueue<B256> {
    root: cleanup_tree_root,
    mid: cleanup_tree_mid,
    leaf: cleanup_tree_leaf,
    len: cleanup_day_len,
    live: cleanup_day_live,
    at: cleanup_day_at,
    slot: cleanup_group_slot,
    deadline: cleanup_deadline,
    sweep_day: cleanup_sweep_day,
    cursor: cleanup_cursor,
});

/// One currency's call-price trie, like `outbe_gem::state::CallBins`.
pub struct CallBins<'a, 'storage>(pub &'a NodContract<'storage>, pub u16);

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

/// This queue orders called groups by the UTC day when their notice period closes.
pub struct ForfeitQueue<'a, 'storage>(pub &'a NodContract<'storage>);

outbe_primitives::impl_deadline_queue!(ForfeitQueue<B256> {
    root: forfeit_tree_root,
    mid: forfeit_tree_mid,
    leaf: forfeit_tree_leaf,
    len: forfeit_day_len,
    live: forfeit_day_live,
    at: forfeit_day_at,
    slot: called_group_slot,
    deadline: called_deadline,
    sweep_day: forfeit_pass_day,
    cursor: forfeit_cursor,
});
