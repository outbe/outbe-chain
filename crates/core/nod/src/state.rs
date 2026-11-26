use alloy_primitives::{B256, U256};
use outbe_core_common::deadline_queue;
use outbe_core_common::series_id::SeriesId;
use outbe_primitives::error::Result;
use outbe_system_common::call_bins;

use crate::nod_encryption::{NodGroupLoadOpRequestV1, NodGroupLoadOperationV1};
use crate::{
    errors::NodError,
    schema::{CallTerms, NodContract},
};

impl NodContract<'_> {
    // --- ID helpers ---

    // --- Bin index helpers (PancakeSwap LB-style ladder) -------------------

    /// Parks a new group in the bin of its sealed call price.
    pub fn insert_call_bin(&mut self, group_key: B256) -> Result<()> {
        let iso = self.group_currency.read(&group_key)?;
        let bin = outbe_system_common::call_bins::price_to_bin(
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
    // The forfeit pass enumerates unsettled members with a count, positional map
    // and reverse map for O(1) swap removal.

    /// Appends `nod_id` and increments the group's authoritative unsettled count.
    pub(crate) fn insert_group_member(&mut self, group_key: B256, nod_id: U256) -> Result<()> {
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
    pub(crate) fn remove_group_member(&mut self, group_key: B256, nod_id: U256) -> Result<u32> {
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
        self.group_nods.write(&last_key, U256::ZERO)?;
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
        outbe_system_common::call_breach::widen_scan_terms(
            &self.max_call_window_seconds,
            &self.min_call_threshold_seconds,
            terms.reference_currency,
            terms.call_window_seconds,
            terms.call_threshold_seconds,
        )
    }

    /// Queues a called group on the deadline its notice period closes at.
    pub fn push_called(&mut self, group_key: B256, deadline: u64) -> Result<()> {
        deadline_queue::push(&CloseQueue(self), group_key, deadline)
    }

    /// Records every unsettled member called at once: the group leaves the trie for the
    /// close queue, where it waits on `deadline` as one entry.
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
        deadline_queue::remove(&CloseQueue(self), group_key)
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
        self.group_unsettled_load_minor_ct
            .bytes(&group_key)
            .clear()?;
        self.group_is_closed.clear(&group_key)?;
        self.group_closing.clear(&group_key)?;
        self.group_close_load_minor.clear(&group_key)?;
        for sub_series in self.group_series.list(&group_key).read_all()? {
            let gs_key = crate::index_keys::group_series_key(group_key, sub_series);
            self.group_series_present.clear(&gs_key)?;
            self.group_series_closed.clear(&gs_key)?;
            self.group_series_nod_count.clear(&gs_key)?;
            self.group_series_unsettled_load_minor_ct
                .bytes(&gs_key)
                .clear()?;
            self.sub_series_issuance_currency.clear(&sub_series)?;
            self.sub_series_reference_currency.clear(&sub_series)?;
            self.sub_series_series_id.clear(&sub_series)?;
            self.sub_series_number.clear(&sub_series)?;
            self.sub_series_entry_price_minor.clear(&sub_series)?;
            self.sub_series_group.clear(&sub_series)?;
        }
        self.group_series.list(&group_key).clear()?;
        deadline_queue::remove(&ForfeitQueue(self), group_key)
    }

    /// Closes a called group after `NodSeriesClosed` once nothing settled remains.
    pub fn close_group_if_empty(&mut self, group_key: B256) -> Result<()> {
        let Some(group) = self.groups.get(group_key)? else {
            return Ok(());
        };
        if group.settled_nods != 0 || self.group_nod_count.read(&group_key)? != 0 {
            return Ok(());
        }
        self.group_nod_count.clear(&group_key)?;
        self.close_group(group_key)?;
        self.groups.delete(group_key)
    }

    pub(crate) fn register_group_series(
        &mut self,
        group_key: B256,
        (series_id, number): (SeriesId, u16),
        (issuance_currency, reference_currency): (u16, u16),
        entry_price_minor: U256,
    ) -> Result<()> {
        let sub_series = crate::index_keys::sub_series_key(series_id, number);
        let gs_key = crate::index_keys::group_series_key(group_key, sub_series);
        if !self.group_series_present.read(&gs_key)? {
            self.group_series.list(&group_key).push(sub_series)?;
            self.group_series_present.write(&gs_key, true)?;
            self.sub_series_series_id.write(&sub_series, series_id)?;
            self.sub_series_number.write(&sub_series, number)?;
            self.sub_series_issuance_currency
                .write(&sub_series, issuance_currency)?;
            self.sub_series_reference_currency
                .write(&sub_series, reference_currency)?;
        }
        if self
            .sub_series_entry_price_minor
            .read(&sub_series)?
            .is_zero()
        {
            self.sub_series_entry_price_minor
                .write(&sub_series, entry_price_minor)?;
            self.sub_series_group.write(&sub_series, group_key)?;
        }
        Ok(())
    }

    pub(crate) fn increment_series_nod_count(
        &mut self,
        group_key: B256,
        sub_series: B256,
    ) -> Result<()> {
        let gs_key = crate::index_keys::group_series_key(group_key, sub_series);
        let next = self
            .group_series_nod_count
            .read(&gs_key)?
            .checked_add(1)
            .ok_or(NodError::GroupMemberIndexOverflow { group_key })?;
        self.group_series_nod_count.write(&gs_key, next)
    }

    pub(crate) fn decrement_series_nod_count(
        &mut self,
        group_key: B256,
        sub_series: B256,
    ) -> Result<()> {
        let gs_key = crate::index_keys::group_series_key(group_key, sub_series);
        let next = self
            .group_series_nod_count
            .read(&gs_key)?
            .checked_sub(1)
            .ok_or(NodError::GroupMemberCountUnderflowDuringRemoval { group_key })?;
        self.group_series_nod_count.write(&gs_key, next)
    }

    // --- Encrypted group open load of the unsettled Nods -------------------------

    pub fn encrypted_group_open_load(&self, group_key: B256) -> Result<Vec<u8>> {
        self.group_unsettled_load_minor_ct.bytes(&group_key).read()
    }

    fn open_load_record(
        &self,
        bytes: Vec<u8>,
        expected_scope: B256,
    ) -> Result<Option<outbe_core_common::nod_encryption::EncryptedNodGroupLoadV1>> {
        if bytes.is_empty() {
            return Ok(None);
        }
        let record: outbe_core_common::nod_encryption::EncryptedNodGroupLoadV1 =
            postcard::from_bytes(&bytes).map_err(|_| NodError::EncryptedTermsMismatch)?;
        if record.scope != expected_scope
            || record.chain_id != self.storage.chain_id()?
            || record.version().is_none()
        {
            return Err(NodError::EncryptedTermsMismatch.into());
        }
        Ok(Some(record))
    }

    pub(crate) fn group_open_load_record(
        &self,
        group_key: B256,
    ) -> Result<Option<outbe_core_common::nod_encryption::EncryptedNodGroupLoadV1>> {
        self.open_load_record(self.encrypted_group_open_load(group_key)?, group_key)
    }

    pub(crate) fn series_open_load_record(
        &self,
        group_key: B256,
        sub_series: B256,
    ) -> Result<Option<outbe_core_common::nod_encryption::EncryptedNodGroupLoadV1>> {
        let gs_key = crate::index_keys::group_series_key(group_key, sub_series);
        self.open_load_record(
            self.group_series_unsettled_load_minor_ct
                .bytes(&gs_key)
                .read()?,
            sub_series,
        )
    }

    fn read_open_load_record(
        &self,
        record: Option<outbe_core_common::nod_encryption::EncryptedNodGroupLoadV1>,
    ) -> Result<U256> {
        let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&self.storage)?;
        match record {
            None => Ok(U256::ZERO),
            Some(record) => crate::enclave_client::read_group_load(&record)
                .map_err(|err| NodError::AmountReadFailed(err).into()),
        }
    }

    pub fn read_group_open_load(&self, group_key: B256) -> Result<U256> {
        self.read_open_load_record(self.group_open_load_record(group_key)?)
    }

    pub fn read_series_open_load(&self, group_key: B256, sub_series: B256) -> Result<U256> {
        self.read_open_load_record(self.series_open_load_record(group_key, sub_series)?)
    }

    fn apply_scoped_open_load(
        &self,
        group_key: B256,
        scope: B256,
        previous: Option<outbe_core_common::nod_encryption::EncryptedNodGroupLoadV1>,
        operation: NodGroupLoadOperationV1,
    ) -> Result<Vec<u8>> {
        let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&self.storage)?;
        let request = NodGroupLoadOpRequestV1 {
            chain_id: self.storage.chain_id()?,
            scope,
            previous,
            operation,
        };
        let record = crate::enclave_client::apply_group_load_operation(request)
            .map_err(|err| open_load_error(group_key, err))?;
        if record.scope != scope {
            return Err(NodError::EncryptedTermsMismatch.into());
        }
        postcard::to_allocvec(&record).map_err(|_| NodError::EncryptedTermsMismatch.into())
    }

    pub(crate) fn apply_group_open_load(
        &mut self,
        group_key: B256,
        operation: NodGroupLoadOperationV1,
    ) -> Result<()> {
        let previous = self.group_open_load_record(group_key)?;
        let bytes = self.apply_scoped_open_load(group_key, group_key, previous, operation)?;
        self.group_unsettled_load_minor_ct
            .bytes(&group_key)
            .write(&bytes)
    }

    pub(crate) fn apply_series_open_load(
        &mut self,
        group_key: B256,
        sub_series: B256,
        operation: NodGroupLoadOperationV1,
    ) -> Result<()> {
        let previous = self.series_open_load_record(group_key, sub_series)?;
        let bytes = self.apply_scoped_open_load(group_key, sub_series, previous, operation)?;
        let gs_key = crate::index_keys::group_series_key(group_key, sub_series);
        self.group_series_unsettled_load_minor_ct
            .bytes(&gs_key)
            .write(&bytes)
    }

    pub(crate) fn add_nod_to_open_load(
        &mut self,
        group_key: B256,
        nod: &outbe_core_common::nod_encryption::EncryptedNodV1,
    ) -> Result<()> {
        self.apply_group_open_load(
            group_key,
            NodGroupLoadOperationV1::AddNod {
                nod: Box::new(nod.clone()),
            },
        )
    }

    pub(crate) fn add_nod_to_series_open_load(
        &mut self,
        group_key: B256,
        sub_series: B256,
        nod: &outbe_core_common::nod_encryption::EncryptedNodV1,
    ) -> Result<()> {
        self.apply_series_open_load(
            group_key,
            sub_series,
            NodGroupLoadOperationV1::AddNod {
                nod: Box::new(nod.clone()),
            },
        )
    }

    pub(crate) fn sub_nod_from_open_load(
        &mut self,
        group_key: B256,
        nod: &outbe_core_common::nod_encryption::EncryptedNodV1,
    ) -> Result<()> {
        self.apply_group_open_load(
            group_key,
            NodGroupLoadOperationV1::SubNod {
                nod: Box::new(nod.clone()),
            },
        )
    }

    pub(crate) fn sub_nod_from_series_open_load(
        &mut self,
        group_key: B256,
        sub_series: B256,
        nod: &outbe_core_common::nod_encryption::EncryptedNodV1,
    ) -> Result<()> {
        self.apply_series_open_load(
            group_key,
            sub_series,
            NodGroupLoadOperationV1::SubNod {
                nod: Box::new(nod.clone()),
            },
        )
    }

    pub(crate) fn take_unsettled_load(&mut self, group_key: B256, load: U256) -> Result<()> {
        self.apply_group_open_load(
            group_key,
            NodGroupLoadOperationV1::AdjustTransient {
                load_minor: load,
                add: false,
            },
        )
    }

    pub(crate) fn take_series_unsettled_load(
        &mut self,
        group_key: B256,
        sub_series: B256,
        load: U256,
    ) -> Result<()> {
        self.apply_series_open_load(
            group_key,
            sub_series,
            NodGroupLoadOperationV1::AdjustTransient {
                load_minor: load,
                add: false,
            },
        )
    }

    /// Moves a closed group from the close queue to the forfeit pass, which deletes its
    /// unsettled Nods. Done with the group's credit, so a retry never credits it twice.
    pub fn hand_to_forfeit(&mut self, group_key: B256) -> Result<()> {
        let deadline = self.called_deadline.read(&group_key)?;
        deadline_queue::remove(&CloseQueue(self), group_key)?;
        deadline_queue::push(&ForfeitQueue(self), group_key, deadline)
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn first_forfeit_day(&self) -> Result<Option<u32>> {
        deadline_queue::first_day(&ForfeitQueue(self))
    }
}

fn open_load_error(group_key: B256, err: outbe_tee::TransportError) -> NodError {
    use crate::nod_encryption::{encryption_cause, NodEncryptionError};
    let overflow = matches!(
        encryption_cause(&err),
        Some(NodEncryptionError::GroupLoadOverflow)
    );
    let underflow = matches!(
        encryption_cause(&err),
        Some(NodEncryptionError::GroupLoadUnderflow)
    );
    if overflow {
        NodError::GroupUnsettledLoadOverflow { group_key }
    } else if underflow {
        NodError::GroupUnsettledLoadUnderflow { group_key }
    } else {
        NodError::AmountReadFailed(err)
    }
}

/// Closed groups whose unsettled Nods wait to be deleted.
pub struct ForfeitQueue<'a, 'storage>(pub &'a NodContract<'storage>);

outbe_core_common::impl_deadline_queue!(ForfeitQueue<B256> {
    root: forfeit_tree_root,
    mid: forfeit_tree_mid,
    leaf: forfeit_tree_leaf,
    len: forfeit_day_len,
    live: forfeit_day_live,
    at: forfeit_day_at,
    slot: forfeit_group_slot,
    deadline: forfeit_deadline,
    sweep_day: forfeit_sweep_day,
    cursor: forfeit_cursor,
});

/// One currency's call-price trie, like `outbe_gem::state::CallBins`.
pub struct CallBins<'a, 'storage>(pub &'a NodContract<'storage>, pub u16);

outbe_system_common::impl_call_bins!(CallBins<B256> {
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
pub struct CloseQueue<'a, 'storage>(pub &'a NodContract<'storage>);

outbe_core_common::impl_deadline_queue!(CloseQueue<B256> {
    root: close_tree_root,
    mid: close_tree_mid,
    leaf: close_tree_leaf,
    len: close_day_len,
    live: close_day_live,
    at: close_day_at,
    slot: called_group_slot,
    deadline: called_deadline,
    sweep_day: close_pass_day,
    cursor: close_cursor,
});
