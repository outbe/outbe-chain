use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    delete, derive_poseidon_entity_id, list, mint, read, update, BodyInput, EntityRef,
    ExecutionScope, IdPageRequest, ParentBodySource, QueryRef, VerifiedBody, WwdEntityId,
    MAX_ID_PAGE_LIMIT,
};
use outbe_primitives::call_bins;
use outbe_primitives::error::Result;
use outbe_primitives::expiry_queue;
use outbe_primitives::time::WorldwideDay;
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    api::{LoadedNodBucket, LoadedNodItem},
    config::NodParams,
    errors::NodError,
    precompile::INod,
    schema::{CallTerms, NodBucketState, NodContract, NodItemState},
};

/// `entry × (100 + call_rate) / 100` plus the profile's other call terms.
///
/// `None` when entry is zero: a zero call price would fire on the first scan.
/// The profile is read exactly here. Every later check reads the bucket's
/// sealed copy, so a retune cannot re-term an already-issued Nod.
pub(crate) fn derived_call_terms(
    entry_price_minor: U256,
    reference_currency: u16,
    params: NodParams,
) -> Result<Option<CallTerms>> {
    if entry_price_minor.is_zero() {
        return Ok(None);
    }
    let call_price = entry_price_minor
        .checked_mul(U256::from(100 + u32::from(params.call_rate)))
        .ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Fatal("Nod call price overflow".into())
        })?
        / U256::from(100u64);
    Ok(Some(CallTerms {
        call_price_minor: call_price,
        reference_currency,
        call_rate: params.call_rate,
        call_window_seconds: params.call_window_seconds,
        call_threshold_seconds: params.call_threshold_seconds,
        call_notice_period_seconds: params.call_notice_period_seconds,
    }))
}

impl NodContract<'_> {
    /// A Nod id carries its Worldwide Day as a prefix, so each day is one id range.
    pub(crate) fn emit_days_metadata_update(&mut self, days: &BTreeSet<u32>) -> Result<()> {
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

    pub fn entry_price_snapshot(&self, day: WorldwideDay) -> Result<Option<BTreeMap<u16, U256>>> {
        if !self.entry_prices_frozen.read(&day)? {
            return Ok(None);
        }
        let count = self.entry_price_currency_count.read(&day)?;
        if count > crate::openings::MAX_ENTRY_PRICE_CURRENCIES {
            return Err(NodError::InvalidEntryPriceSnapshot.into());
        }
        let currencies = self.entry_price_currency.get_nested(&day);
        let values = self.entry_price_minor.get_nested(&day);
        let mut prices = BTreeMap::new();
        let mut previous = 0;
        for index in 0..count {
            let iso = currencies.read(&index)?;
            let price = values.read(&iso)?;
            if iso <= previous || price.is_zero() {
                return Err(NodError::InvalidEntryPriceSnapshot.into());
            }
            prices.insert(iso, price);
            previous = iso;
        }
        Ok(Some(prices))
    }

    pub fn entry_price_source_day(&self, day: WorldwideDay) -> Result<Option<u32>> {
        if !self.entry_prices_frozen.read(&day)? {
            return Ok(None);
        }
        Ok(Some(self.entry_price_source_day.read(&day)?))
    }

    pub fn store_entry_price_snapshot(
        &self,
        day: WorldwideDay,
        source_day: u32,
        prices: &BTreeMap<u16, U256>,
    ) -> Result<()> {
        let count = u32::try_from(prices.len()).map_err(|_| NodError::InvalidEntryPriceSnapshot)?;
        if !day.is_valid()
            || source_day == 0
            || count > crate::openings::MAX_ENTRY_PRICE_CURRENCIES
            || prices
                .iter()
                .any(|(iso, price)| *iso == 0 || price.is_zero())
        {
            return Err(NodError::InvalidEntryPriceSnapshot.into());
        }
        self.storage_handle().with_checkpoint(|| {
            if self.entry_prices_frozen.read(&day)? {
                return Err(NodError::EntryPricesAlreadyFrozen.into());
            }
            let currencies = self.entry_price_currency.get_nested(&day);
            let values = self.entry_price_minor.get_nested(&day);
            for (index, (iso, price)) in (0..count).zip(prices) {
                currencies.write(&index, *iso)?;
                values.write(iso, *price)?;
            }
            self.entry_price_currency_count.write(&day, count)?;
            self.entry_price_source_day.write(&day, source_day)?;
            self.entry_prices_frozen.write(&day, true)
        })
    }

    // --- ID helpers ---

    pub fn parse_nod_id(nod_id: &str) -> Result<WwdEntityId> {
        let trimmed = nod_id.strip_prefix("0x").unwrap_or(nod_id);
        if trimmed.len() != WwdEntityId::len_bytes() * 2 {
            return Err(NodError::InvalidNodIdLength.into());
        }
        trimmed
            .parse::<WwdEntityId>()
            .map_err(|_| NodError::InvalidNodIdHex.into())
    }

    // --- View functions ---

    pub fn total_supply(&self) -> Result<u64> {
        self.total_supply.read()
    }

    pub(crate) fn get_item_verified(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        nod_id: WwdEntityId,
    ) -> Result<Option<VerifiedBody>> {
        read(
            self.storage_handle(),
            scope,
            parent,
            EntityRef::NodItem(nod_id),
        )
    }

    pub(crate) fn get_bucket_verified(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        bucket_id: WwdEntityId,
    ) -> Result<Option<VerifiedBody>> {
        read(
            self.storage_handle(),
            scope,
            parent,
            EntityRef::NodBucket(bucket_id),
        )
    }

    pub(crate) fn read_all(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        owner: Option<Address>,
    ) -> Result<Vec<NodItemState>> {
        let query = owner.map_or(QueryRef::NodAll, QueryRef::NodByOwner);
        let mut records = Vec::new();
        let mut after = None;
        loop {
            let (page, next_after) = self.read_page(scope, parent, query, after)?;
            records.extend(page);
            let Some(next) = next_after else {
                return Ok(records);
            };
            after = Some(next);
        }
    }

    /// One page of `query` after `after`, decoded, with the cursor of the next page.
    fn read_page(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        query: QueryRef,
        after: Option<WwdEntityId>,
    ) -> Result<(Vec<NodItemState>, Option<WwdEntityId>)> {
        let page = list(
            self.storage_handle(),
            scope,
            parent,
            query,
            IdPageRequest {
                after,
                limit: MAX_ID_PAGE_LIMIT,
            },
        )?;
        let next_after = page.next_after();
        let records = page
            .into_bodies()
            .iter()
            .map(nod_item_from_verified)
            .collect::<Result<Vec<_>>>()?;
        Ok((records, next_after))
    }

    /// Records compact issuance state and delegates both bodies to the generic lifecycle.
    pub(crate) fn record_nod_issued(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        item: &NodItemState,
        entry_price_minor: U256,
    ) -> Result<()> {
        Self::check_issued_identity(item, entry_price_minor)?;
        if self
            .get_item_verified(scope, parent, item.nod_id)?
            .is_some()
        {
            return Err(outbe_primitives::error::PrecompileError::Revert(
                "nod already exists".into(),
            ));
        }

        let bucket_id = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key.0);
        let current_bucket = self.get_bucket_verified(scope, parent, bucket_id)?;
        let member_count = self.bucket_nod_count.read(&item.bucket_key)?;
        let settled_count = current_bucket
            .as_ref()
            .map(nod_bucket_from_verified)
            .transpose()?
            .map_or(0, |bucket| bucket.settled_nods);
        if current_bucket.is_some() != (member_count > 0 || settled_count > 0) {
            return Err(outbe_primitives::error::PrecompileError::Revert(format!(
                "Nod bucket {bucket_id} existence disagrees with member count {member_count}"
            )));
        }
        let new_bucket = match current_bucket {
            Some(_) => None,
            None => {
                let bucket = NodBucketState {
                    settled_nods: 0,
                    bucket_key: item.bucket_key,
                    worldwide_day: item.worldwide_day,
                    entry_price_minor,
                    reference_currency: item.reference_currency,
                };
                self.bucket_worldwide_day
                    .write(&item.bucket_key, item.worldwide_day)?;
                self.callable_bucket_issued_at
                    .write(&item.bucket_key, item.issued_at)?;
                let params = crate::config::read_from(self, self.storage_handle().chain_id()?)?;
                if let Some(terms) =
                    derived_call_terms(entry_price_minor, item.reference_currency, params)?
                {
                    self.seal_bucket_call_terms(item.bucket_key, terms)?;
                    self.insert_call_bin(item.bucket_key)?;
                }
                Some(bucket)
            }
        };

        let supply = self.total_supply.read()?.checked_add(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Revert(
                "Nod total supply overflow during issuance".into(),
            )
        })?;
        self.total_supply.write(supply)?;
        self.insert_bucket_member(item.bucket_key, item.nod_id)?;
        let canonical_item = crate::repository::canonical_item(item);
        mint(
            self.storage_handle(),
            scope,
            BodyInput::NodItem(&canonical_item),
        )?;
        if let Some(bucket) = new_bucket {
            let canonical_bucket = crate::repository::canonical_bucket(&bucket);
            mint(
                self.storage_handle(),
                scope,
                BodyInput::NodBucket(&canonical_bucket),
            )?;
        }
        self.emit(INod::Transfer {
            from: Address::ZERO,
            to: item.owner,
            tokenId: item.nod_id.to_u256(),
        })
    }

    /// Rejects an item whose identity, payment state or bucket does not match a new issuance.
    fn check_issued_identity(item: &NodItemState, entry_price_minor: U256) -> Result<()> {
        let canonical_id = derive_poseidon_entity_id(item.owner, item.worldwide_day)
            .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))?;
        if item.nod_id != canonical_id {
            return Err(outbe_primitives::error::PrecompileError::Fatal(format!(
                "Nod item canonical identity mismatch: expected {canonical_id}, found {}",
                item.nod_id
            )));
        }
        if item.is_settled {
            return Err(outbe_primitives::error::PrecompileError::Revert(
                "cannot issue a settled Nod".into(),
            ));
        }
        // ISO 0 is not a currency. Its bin namespace aliases the
        // un-namespaced key. ISO 0 also never appears in the oracle's
        // reference-currency registry. A bucket parked there would be
        // invisible to the call scan forever.
        if item.reference_currency == 0 {
            return Err(NodError::ZeroReferenceCurrency.into());
        }

        let canonical_bucket_key = Self::bucket_key(
            item.worldwide_day,
            entry_price_minor,
            item.reference_currency,
        );
        if item.bucket_key != canonical_bucket_key {
            return Err(outbe_primitives::error::PrecompileError::Fatal(format!(
                "Nod bucket identity mismatch: expected {canonical_bucket_key}, found {}",
                item.bucket_key
            )));
        }
        Ok(())
    }

    /// Records compact removal state using capabilities retained by the caller's checks.
    pub(crate) fn record_nod_removed(
        &mut self,
        scope: &ExecutionScope,
        item: LoadedNodItem,
        bucket: LoadedNodBucket,
    ) -> Result<()> {
        let (item, current_item) = item.into_parts();
        let (mut bucket, current_bucket) = bucket.into_parts();
        self.check_loaded_bucket(&item, &current_bucket)?;
        let bucket_id = current_bucket.entity_id();
        let supply = self.total_supply.read()?.checked_sub(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Revert(
                "Nod total supply underflow during removal".into(),
            )
        })?;
        self.total_supply.write(supply)?;
        let remaining = if item.is_settled {
            bucket.settled_nods = bucket.settled_nods.checked_sub(1).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Revert(format!(
                    "Nod bucket {bucket_id} settled count underflow"
                ))
            })?;
            self.bucket_nod_count.read(&item.bucket_key)?
        } else {
            self.remove_bucket_member(item.bucket_key, item.nod_id)?
        };
        delete(self.storage_handle(), scope, current_item)?;
        if remaining == 0 && bucket.settled_nods == 0 {
            self.bucket_worldwide_day.get(&item.bucket_key).delete()?;
            self.callable_bucket_issued_at.clear(&item.bucket_key)?;
            self.bucket_nod_count.clear(&item.bucket_key)?;
            self.remove_callable_bucket(item.bucket_key)?;
            delete(self.storage_handle(), scope, current_bucket)?;
        } else if item.is_settled {
            update(
                self.storage_handle(),
                scope,
                current_bucket,
                BodyInput::NodBucket(&crate::repository::canonical_bucket(&bucket)),
            )?;
        }
        self.emit(INod::Transfer {
            from: item.owner,
            to: Address::ZERO,
            tokenId: item.nod_id.to_u256(),
        })
    }

    /// Moves one unpaid member into the live paid count, preserving ownership and supply.
    pub(crate) fn record_nod_settled(
        &mut self,
        scope: &ExecutionScope,
        item: LoadedNodItem,
        bucket: LoadedNodBucket,
    ) -> Result<()> {
        let (mut item, current_item) = item.into_parts();
        let (mut bucket, current_bucket) = bucket.into_parts();
        self.check_loaded_bucket(&item, &current_bucket)?;
        if item.is_settled || !self.settlement_open(&bucket)? {
            return Err(outbe_primitives::error::PrecompileError::Revert(
                "Nod settlement requires an unpaid item of a called or qualified bucket".into(),
            ));
        }
        bucket.settled_nods = bucket.settled_nods.checked_add(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Revert("Nod settled count overflow".into())
        })?;
        self.remove_bucket_member(item.bucket_key, item.nod_id)?;
        item.is_settled = true;
        update(
            self.storage_handle(),
            scope,
            current_item,
            BodyInput::NodItem(&crate::repository::canonical_item(&item)),
        )?;
        update(
            self.storage_handle(),
            scope,
            current_bucket,
            BodyInput::NodBucket(&crate::repository::canonical_bucket(&bucket)),
        )?;
        self.emit(INod::MetadataUpdate {
            _tokenId: item.nod_id.to_u256(),
        })
    }

    /// A called bucket settles until its deadline, an uncalled one once qualified.
    fn settlement_open(&self, bucket: &NodBucketState) -> Result<bool> {
        let storage = self.storage_handle();
        match crate::api::settlement_deadline(&storage, bucket.bucket_key)? {
            0 => crate::api::is_qualified(&storage, bucket),
            deadline => Ok(storage.timestamp()?.to::<u64>() <= deadline),
        }
    }

    fn check_loaded_bucket(&self, item: &NodItemState, current: &VerifiedBody) -> Result<()> {
        let expected = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key.0);
        if current.entity_id() != expected {
            return Err(outbe_primitives::error::PrecompileError::Revert(format!(
                "loaded Nod bucket {} does not match item bucket {expected}",
                current.entity_id()
            )));
        }
        Ok(())
    }

    // --- Bin index helpers (PancakeSwap LB-style ladder) -------------------

    /// Maps a six-decimal call price (or oracle rate) to a 24-bit
    /// bin id on the LB log-spaced ladder. Saturates to `[0, MAX_BIN_ID]`.
    /// See [`outbe_primitives::math::price_helper::get_id_from_price`] for the saturation
    /// rationale.
    pub fn price_to_bin(price_minor: U256) -> Result<u32> {
        call_bins::price_to_bin(price_minor)
    }

    /// Lower edge of `bin_id` in six-decimal minor units. Diagnostic-only.
    pub fn bin_to_price_floor(bin_id: u32) -> Result<U256> {
        call_bins::bin_to_price_floor(bin_id)
    }

    /// Parks a new bucket in the bin of its sealed call price.
    pub(crate) fn insert_call_bin(&mut self, bucket_key: B256) -> Result<()> {
        let iso = self.callable_bucket_currency.read(&bucket_key)?;
        let bin = Self::price_to_bin(self.callable_bucket_call_price_minor.read(&bucket_key)?)?;
        call_bins::insert(&CallBins(self, iso), bucket_key, bin)
    }

    /// No-op for a bucket the trie does not hold.
    pub(crate) fn remove_call_bin(&mut self, bucket_key: B256) -> Result<()> {
        let iso = self.callable_bucket_currency.read(&bucket_key)?;
        call_bins::remove(&CallBins(self, iso), bucket_key)?;
        Ok(())
    }

    // --- Bucket member index ------------------------------------------------
    //
    // The compressed-entity store answers "give me this Nod" but never "give me
    // this bucket's Nods", so the forfeit sweep needs its own enumeration. The
    // shape mirrors the call-price bin index: a count plus a keccak-of-concat
    // positional map, with a reverse map for O(1) swap-remove.

    /// Storage key for the `index`-th Nod parked in `bucket_key`.
    pub(crate) fn bucket_nod_key(bucket_key: B256, index: u32) -> B256 {
        let mut buf = [0u8; 36];
        buf[0..32].copy_from_slice(bucket_key.as_slice());
        buf[32..36].copy_from_slice(&index.to_be_bytes());
        alloy_primitives::keccak256(buf)
    }

    /// Appends `nod_id` and increments the bucket's authoritative unpaid count.
    pub(crate) fn insert_bucket_member(
        &mut self,
        bucket_key: B256,
        nod_id: WwdEntityId,
    ) -> Result<()> {
        let index = self.bucket_nod_count.read(&bucket_key)?;
        let next = index.checked_add(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Fatal(format!(
                "Nod bucket {bucket_key} member index overflow"
            ))
        })?;
        self.bucket_nods
            .write(&Self::bucket_nod_key(bucket_key, index), nod_id)?;
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
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::Revert(format!(
                    "Nod bucket {bucket_key} member count underflow during removal"
                ))
            })?;
        if index > last
            || self
                .bucket_nods
                .read(&Self::bucket_nod_key(bucket_key, index))?
                != nod_id
        {
            return Err(outbe_primitives::error::PrecompileError::Revert(format!(
                "Nod {nod_id} is not indexed in bucket {bucket_key}"
            )));
        }
        let last_key = Self::bucket_nod_key(bucket_key, last);
        if index != last {
            let moved = self.bucket_nods.read(&last_key)?;
            if moved.is_zero() {
                return Err(outbe_primitives::error::PrecompileError::Revert(format!(
                    "Nod bucket {bucket_key} member slot {last} is empty during removal"
                )));
            }
            self.bucket_nods
                .write(&Self::bucket_nod_key(bucket_key, index), moved)?;
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
        self.callable_bucket_call_price_minor
            .write(&bucket_key, terms.call_price_minor)?;
        self.callable_bucket_currency
            .write(&bucket_key, terms.reference_currency)?;
        self.callable_bucket_call_rate
            .write(&bucket_key, terms.call_rate)?;
        self.callable_bucket_call_window_seconds
            .write(&bucket_key, terms.call_window_seconds)?;
        self.callable_bucket_call_threshold_seconds
            .write(&bucket_key, terms.call_threshold_seconds)?;
        self.callable_bucket_call_notice_period_seconds
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
    pub(crate) fn push_called_bucket(&mut self, bucket_key: B256, deadline: u64) -> Result<()> {
        expiry_queue::push(&ExpiryHours(self), bucket_key, deadline)
    }

    /// Reads back the terms [`Self::seal_bucket_call_terms`] sealed at issuance.
    pub(crate) fn read_call_terms(&self, bucket_key: B256) -> Result<CallTerms> {
        Ok(CallTerms {
            call_price_minor: self.callable_bucket_call_price_minor.read(&bucket_key)?,
            reference_currency: self.callable_bucket_currency.read(&bucket_key)?,
            call_rate: self.callable_bucket_call_rate.read(&bucket_key)?,
            call_window_seconds: self.callable_bucket_call_window_seconds.read(&bucket_key)?,
            call_threshold_seconds: self
                .callable_bucket_call_threshold_seconds
                .read(&bucket_key)?,
            call_notice_period_seconds: self
                .callable_bucket_call_notice_period_seconds
                .read(&bucket_key)?,
        })
    }

    /// No-op for a bucket the queue does not hold.
    pub(crate) fn remove_called_bucket(&mut self, bucket_key: B256) -> Result<()> {
        expiry_queue::remove(&ExpiryHours(self), bucket_key)
    }

    /// No-op for a bucket the call index never held, so the removal funnel can call it
    /// unconditionally.
    pub(crate) fn remove_callable_bucket(&mut self, bucket_key: B256) -> Result<()> {
        self.remove_call_bin(bucket_key)?;
        self.remove_called_bucket(bucket_key)?;
        self.callable_bucket_call_price_minor.clear(&bucket_key)?;
        self.callable_bucket_currency.get(&bucket_key).delete()?;
        self.callable_bucket_call_rate.get(&bucket_key).delete()?;
        self.callable_bucket_call_window_seconds
            .clear(&bucket_key)?;
        self.callable_bucket_call_threshold_seconds
            .clear(&bucket_key)?;
        self.callable_bucket_call_notice_period_seconds
            .clear(&bucket_key)?;
        self.callable_bucket_issued_at.clear(&bucket_key)?;
        self.bucket_called_at.clear(&bucket_key)?;
        Ok(())
    }
}

pub(crate) fn nod_item_from_verified(body: &VerifiedBody) -> Result<NodItemState> {
    let payload = body.payload().as_nod_item().ok_or_else(|| {
        outbe_primitives::error::PrecompileError::Fatal(
            "compressed-entity read returned a non-Nod-item payload".into(),
        )
    })?;
    Ok(crate::repository::from_canonical_item(payload.clone()))
}

pub(crate) fn nod_bucket_from_verified(body: &VerifiedBody) -> Result<NodBucketState> {
    let payload = body.payload().as_nod_bucket().ok_or_else(|| {
        outbe_primitives::error::PrecompileError::Fatal(
            "compressed-entity read returned a non-Nod-bucket payload".into(),
        )
    })?;
    Ok(crate::repository::from_canonical_bucket(payload.clone()))
}

/// One currency's call-price trie, like `outbe_gem::state::CallBins`.
pub(crate) struct CallBins<'a, 'storage>(pub(crate) &'a NodContract<'storage>, pub(crate) u16);

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
