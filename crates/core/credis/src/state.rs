//! Storage CRUD, the live lists, the sub-series and the call groups of the Credis contract.
//!
//! All functions take a short-lived `&mut CredisContract` (or `&CredisContract`
//! for reads) constructed via `CredisContract::new(storage)`. They only touch
//! local storage. The rules that drive them live in CredisFactory.

use alloy_primitives::{Address, B256, U256};

use outbe_core_common::deadline_queue;
use outbe_core_common::series_id::SeriesId;
use outbe_primitives::error::Result;
use outbe_primitives::storage::dsl::Map;
use outbe_primitives::storage::indexed_list::{create_indexed, delete_indexed, IndexedList};
use outbe_system_common::call_bins;
use outbe_system_common::call_breach::{self, BreachTerms};

use crate::errors::CredisError;
use crate::precompile::ICredis;
use crate::schema::{CallTerms, Credis, CredisContract, CredisView, SubSeriesSummary};

impl<'storage> CredisContract<'storage> {
    // ---------------------------------------------------------------------
    // Credis CRUD
    // ---------------------------------------------------------------------

    pub fn credis_exists(&self, credis_id: U256) -> Result<bool> {
        self.records.exists(credis_id)
    }

    /// Loads a Credis with the call terms and the call time of its group. Reverts on missing.
    pub fn load_credis(&self, credis_id: U256) -> Result<CredisView> {
        let record = self
            .records
            .get(credis_id)?
            .ok_or(CredisError::CredisNotFound)?;
        let group = self.group_of(&record)?;
        Ok(CredisView {
            terms: self.group_call_terms(group)?,
            called_at: self.group_called_at.read(&group)?,
            record,
        })
    }

    /// The call terms that `group` sealed with its first Credis.
    pub fn group_call_terms(&self, group: B256) -> Result<CallTerms> {
        Ok(CallTerms {
            call_window_seconds: self.group_call_window_seconds.read(&group)?,
            call_threshold_seconds: self.group_call_threshold_seconds.read(&group)?,
            call_notice_period_seconds: self.group_call_notice_period_seconds.read(&group)?,
        })
    }

    /// The call group of the Credis's sub-series.
    pub fn group_of(&self, record: &Credis) -> Result<B256> {
        self.sub_series_group
            .read(&Self::sub_series_key(record.series_id, record.sub_series))
    }

    /// The entry and call price a sub-series sealed, `None` while it is not open.
    pub fn sub_series_prices(
        &self,
        series_id: SeriesId,
        sub_series: u8,
    ) -> Result<Option<(U256, U256)>> {
        let key = Self::sub_series_key(series_id, sub_series);
        if self.sub_series_group.read(&key)?.is_zero() {
            return Ok(None);
        }
        Ok(Some((
            self.sub_series_entry_price_minor.read(&key)?,
            self.sub_series_call_price_minor.read(&key)?,
        )))
    }

    /// Records a new Credis, lists it, puts it in its sub-series and group, and emits the ERC-721 mint `Transfer`. A group it opens seals
    /// `terms`. Returns `true` when this Credis opens its sub-series.
    pub fn create_credis_record(
        &mut self,
        record: &Credis,
        terms: &CallTerms,
        call_start_day: u32,
    ) -> Result<bool> {
        create_indexed(&self.records, record, self.record_indexes(record.owner))?;
        let newly_opened = self.join_group(record, terms, call_start_day)?;
        self.emit(ICredis::Transfer {
            from: Address::ZERO,
            to: record.owner,
            tokenId: record.credis_id,
        })?;
        Ok(newly_opened)
    }

    pub fn update_credis_record(&mut self, record: &Credis) -> Result<()> {
        self.records.update(record)?;
        self.emit(ICredis::MetadataUpdate {
            _tokenId: record.credis_id,
        })
    }

    /// Deletes a Credis, unlists it and emits the ERC-721 burn
    /// `Transfer`. Its group membership is handled by the caller.
    pub fn delete_credis_record(&mut self, credis_id: U256) -> Result<()> {
        let record = self
            .records
            .get(credis_id)?
            .ok_or(CredisError::CredisNotFound)?;
        delete_indexed(&self.records, credis_id, |record: &Credis| {
            self.record_indexes(record.owner)
        })?;
        self.emit(ICredis::Transfer {
            from: record.owner,
            to: Address::ZERO,
            tokenId: credis_id,
        })
    }

    pub fn take_outstanding_amount(&mut self, record: &Credis, amount: U256) -> Result<()> {
        take_from(
            &self.sub_series_outstanding_amount_minor,
            Self::sub_series_key(record.series_id, record.sub_series),
            amount,
        )
    }

    fn record_indexes(&self, owner: Address) -> [IndexedList<'storage, U256>; 1] {
        [IndexedList::new(
            self.owner_ids.list(&owner),
            self.owner_id_position.clone(),
        )]
    }

    // ---------------------------------------------------------------------
    // Live lists
    // ---------------------------------------------------------------------

    pub fn owner_credis_count(&self, account: Address) -> Result<u32> {
        self.owner_ids.list(&account).len()
    }

    pub fn owner_credis_ids(&self, account: Address) -> Result<Vec<U256>> {
        self.owner_ids.list(&account).read_all()
    }

    // ---------------------------------------------------------------------
    // Sub-series and call groups
    // ---------------------------------------------------------------------

    fn group_index(&self, group: B256) -> IndexedList<'storage, U256> {
        IndexedList::new(
            self.group_members.list(&group),
            self.group_member_position.clone(),
        )
    }

    /// Returns `true` when this Credis opens its sub-series.
    fn join_group(
        &mut self,
        record: &Credis,
        terms: &CallTerms,
        call_start_day: u32,
    ) -> Result<bool> {
        let key = Self::sub_series_key(record.series_id, record.sub_series);
        let mut group = self.sub_series_group.read(&key)?;
        let newly_opened = group.is_zero();
        if newly_opened {
            group = call_breach::group_key(
                call_start_day,
                record.call_price_minor,
                record.reference_currency,
            );
            self.open_sub_series(key, group, record)?;
            if self.group_start_day.read(&group)? == 0 {
                self.seal_group(group, record, terms, call_start_day)?;
            }
            self.group_sub_series.list(&group).push(key)?;
        }
        if self.group_member_count(group)? == 0 && self.group_called_at.read(&group)? == 0 {
            let bin = call_bins::price_to_bin(self.group_call_price_minor.read(&group)?)?;
            let currency = self.group_currency.read(&group)?;
            call_bins::insert(&CallBins(self, currency), group, bin)?;
        }
        let count = self
            .sub_series_unsettled_count
            .read(&key)?
            .checked_add(1)
            .ok_or(CredisError::ArithmeticOverflow)?;
        self.sub_series_unsettled_count.write(&key, count)?;
        add_to(
            &self.sub_series_outstanding_amount_minor,
            key,
            record.outstanding_amount_minor,
        )?;
        self.group_index(group).insert(record.credis_id)?;
        Ok(newly_opened)
    }

    fn open_sub_series(&mut self, key: B256, group: B256, record: &Credis) -> Result<()> {
        self.sub_series_group.write(&key, group)?;
        self.sub_series_series_id.write(&key, record.series_id)?;
        self.sub_series_number.write(&key, record.sub_series)?;
        self.sub_series_issuance_currency
            .write(&key, record.issuance_currency)?;
        self.sub_series_entry_price_minor
            .write(&key, record.entry_price_minor)?;
        self.sub_series_call_price_minor
            .write(&key, record.call_price_minor)
    }

    fn seal_group(
        &mut self,
        group: B256,
        record: &Credis,
        terms: &CallTerms,
        call_start_day: u32,
    ) -> Result<()> {
        self.group_currency
            .write(&group, record.reference_currency)?;
        self.group_start_day.write(&group, call_start_day)?;
        self.group_call_price_minor
            .write(&group, record.call_price_minor)?;
        self.group_call_window_seconds
            .write(&group, terms.call_window_seconds)?;
        self.group_call_threshold_seconds
            .write(&group, terms.call_threshold_seconds)?;
        self.group_call_notice_period_seconds
            .write(&group, terms.call_notice_period_seconds)?;
        call_breach::widen_scan_terms(
            &self.max_call_window_seconds,
            &self.min_call_threshold_seconds,
            record.reference_currency,
            terms.call_window_seconds,
            terms.call_threshold_seconds,
        )
    }

    /// A called group stays in the close queue until its deadline, even when empty.
    pub fn leave_group(&mut self, record: &Credis) -> Result<()> {
        let key = Self::sub_series_key(record.series_id, record.sub_series);
        let group = self.sub_series_group.read(&key)?;
        self.group_index(group).remove(record.credis_id)?;
        let count = self
            .sub_series_unsettled_count
            .read(&key)?
            .checked_sub(1)
            .ok_or(CredisError::ArithmeticOverflow)?;
        self.sub_series_unsettled_count.write(&key, count)?;
        if self.group_member_count(group)? == 0 && self.group_called_at.read(&group)? == 0 {
            let currency = self.group_currency.read(&group)?;
            call_bins::remove(&CallBins(self, currency), group)?;
        }
        Ok(())
    }

    pub fn group_member_count(&self, group: B256) -> Result<u32> {
        self.group_members.list(&group).len()
    }

    pub fn group_member(&self, group: B256, index: u32) -> Result<U256> {
        self.group_members
            .list(&group)
            .get(index)?
            .ok_or_else(|| CredisError::IndexOutOfBounds.into())
    }

    pub fn group_sub_series(&self, group: B256) -> Result<Vec<SubSeriesSummary>> {
        let keys = self.group_sub_series.list(&group).read_all()?;
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            out.push(SubSeriesSummary {
                series_id: self.sub_series_series_id.read(&key)?,
                sub_series: self.sub_series_number.read(&key)?,
                issuance_currency: self.sub_series_issuance_currency.read(&key)?,
                unsettled_count: self.sub_series_unsettled_count.read(&key)?,
                outstanding_amount_minor: self.sub_series_outstanding_amount_minor.read(&key)?,
            });
        }
        Ok(out)
    }

    pub fn group_breach_terms(&self, group: B256) -> Result<BreachTerms> {
        Ok(BreachTerms {
            call_price: self.group_call_price_minor.read(&group)?,
            window_seconds: self.group_call_window_seconds.read(&group)?,
            threshold_seconds: self.group_call_threshold_seconds.read(&group)?,
            start_day: self.group_start_day.read(&group)?,
        })
    }

    pub fn record_group_called(&mut self, group: B256, now: u64) -> Result<u64> {
        let currency = self.group_currency.read(&group)?;
        call_bins::remove(&CallBins(self, currency), group)?;
        self.group_called_at.write(&group, now)?;
        let deadline = outbe_core_common::settlement::settlement_deadline_of(
            now,
            self.group_call_notice_period_seconds.read(&group)?,
        );
        self.push_called(group, deadline)?;
        Ok(deadline)
    }

    pub fn hand_to_forfeit(&mut self, group: B256) -> Result<()> {
        let deadline = self.called_deadline.read(&group)?;
        deadline_queue::remove(&CloseQueue(self), group)?;
        deadline_queue::push(&ForfeitQueue(self), group, deadline)
    }

    pub fn push_called(&mut self, group: B256, deadline: u64) -> Result<()> {
        self.group_settlement_deadline.write(&group, deadline)?;
        deadline_queue::push(&CloseQueue(self), group, deadline)
    }

    pub fn remove_called(&mut self, group: B256) -> Result<()> {
        deadline_queue::remove(&CloseQueue(self), group)
    }
}

fn add_to<K: outbe_primitives::storage::types::StorageKey>(
    sums: &Map<'_, K, U256>,
    key: K,
    amount: U256,
) -> Result<()> {
    let total = sums
        .read(&key)?
        .checked_add(amount)
        .ok_or(CredisError::ArithmeticOverflow)?;
    sums.write(&key, total)
}

fn take_from<K: outbe_primitives::storage::types::StorageKey>(
    sums: &Map<'_, K, U256>,
    key: K,
    amount: U256,
) -> Result<()> {
    let total = sums
        .read(&key)?
        .checked_sub(amount)
        .ok_or(CredisError::ArithmeticOverflow)?;
    sums.write(&key, total)
}

/// This queue orders called groups by the UTC day that contains their settlement deadline.
pub struct CloseQueue<'a, 'storage>(pub &'a CredisContract<'storage>);

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

/// Closed groups whose Credis wait to be deleted.
pub struct ForfeitQueue<'a, 'storage>(pub &'a CredisContract<'storage>);

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

/// One reference currency's uncalled groups, by call price.
pub struct CallBins<'a, 'storage>(pub &'a CredisContract<'storage>, pub u16);

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
