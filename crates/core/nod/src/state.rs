use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    delete, derive_poseidon_entity_id, list, mint, read, update, BodyInput, EntityRef,
    ExecutionScope, IdPageRequest, ParentBodySource, QueryRef, VerifiedBody, WwdEntityId,
    MAX_ID_PAGE_LIMIT,
};
use outbe_primitives::error::Result;
use outbe_primitives::math::{
    reference_price,
    tree_math::{self, BinTreeStorage},
};
use outbe_primitives::time::WorldwideDay;
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    api::{LoadedNodBucket, LoadedNodItem},
    constants::{BIN_STEP_BP, CALL_NOTICE_PERIOD, CALL_RATE_PCT, CALL_THRESHOLD, CALL_WINDOW},
    errors::NodError,
    precompile::INod,
    schema::{CallTerms, NodBucketState, NodContract, NodItemState},
};

/// `entry × (100 + CALL_RATE_PCT) / 100` plus the four call constants.
///
/// `None` when entry is zero: a zero call price would fire on the first scan.
/// The constants are read exactly here. Every later check reads the bucket's
/// sealed copy, so a retune cannot re-term an already-issued Nod.
pub(crate) fn derived_call_terms(
    entry_price_minor: U256,
    reference_currency: u16,
) -> Result<Option<CallTerms>> {
    if entry_price_minor.is_zero() {
        return Ok(None);
    }
    let call_price = entry_price_minor
        .checked_mul(U256::from(100 + CALL_RATE_PCT))
        .ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Fatal("Nod call price overflow".into())
        })?
        / U256::from(100u64);
    Ok(Some(CallTerms {
        call_price,
        reference_currency,
        call_rate: CALL_RATE_PCT,
        call_window: CALL_WINDOW,
        call_threshold: CALL_THRESHOLD,
        call_notice_period: CALL_NOTICE_PERIOD,
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
        let values = self.entry_price_value.get_nested(&day);
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

    pub fn store_entry_price_snapshot(
        &self,
        day: WorldwideDay,
        prices: &BTreeMap<u16, U256>,
    ) -> Result<()> {
        let count = u32::try_from(prices.len()).map_err(|_| NodError::InvalidEntryPriceSnapshot)?;
        if !day.is_valid()
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
            let values = self.entry_price_value.get_nested(&day);
            for (index, (iso, price)) in (0..count).zip(prices) {
                currencies.write(&index, *iso)?;
                values.write(iso, *price)?;
            }
            self.entry_price_currency_count.write(&day, count)?;
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
            let bodies = page.into_bodies();
            records.extend(
                bodies
                    .iter()
                    .map(nod_item_from_verified)
                    .collect::<Result<Vec<_>>>()?,
            );
            let Some(next) = next_after else {
                return Ok(records);
            };
            after = Some(next);
        }
    }

    /// Records compact issuance state and delegates both bodies to the generic lifecycle.
    pub(crate) fn record_nod_issued(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        item: &NodItemState,
        entry_price_minor: U256,
    ) -> Result<()> {
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
        // ISO 0 is not a currency, and its bin namespace aliases the
        // un-namespaced key while never appearing in the oracle's
        // reference-currency registry — a bucket parked there would be
        // invisible to the call scan forever.
        if item.reference_currency == 0 {
            return Err(NodError::ZeroReferenceCurrency.into());
        }

        let canonical_bucket_key = Self::bucket_key(
            item.worldwide_day,
            item.floor_price_minor,
            item.reference_currency,
        );
        if item.bucket_key != canonical_bucket_key {
            return Err(outbe_primitives::error::PrecompileError::Fatal(format!(
                "Nod bucket identity mismatch: expected {canonical_bucket_key}, found {}",
                item.bucket_key
            )));
        }
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
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bucket {bucket_id} existence disagrees with member count {member_count}"
                )),
            );
        }
        let new_bucket = match current_bucket {
            Some(_) => None,
            None => {
                let bucket = NodBucketState {
                    settled_nods: 0,
                    bucket_key: item.bucket_key,
                    worldwide_day: item.worldwide_day,
                    floor_price_minor: item.floor_price_minor,
                    entry_price_minor,
                    reference_currency: item.reference_currency,
                };
                self.bucket_worldwide_day
                    .write(&item.bucket_key, item.worldwide_day)?;
                self.callable_bucket_issued_at
                    .write(&item.bucket_key, item.issued_at)?;
                if let Some(terms) = derived_call_terms(entry_price_minor, item.reference_currency)?
                {
                    self.seal_bucket_call_terms(item.bucket_key, terms)?;
                    self.insert_call_bin(item.bucket_key)?;
                }
                Some(bucket)
            }
        };

        let supply = self.total_supply.read()?.checked_add(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
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
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Nod total supply underflow during removal".into(),
            )
        })?;
        self.total_supply.write(supply)?;
        let remaining = if item.is_settled {
            bucket.settled_nods = bucket.settled_nods.checked_sub(1).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
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
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Nod settled count overflow".into(),
            )
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
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "loaded Nod bucket {} does not match item bucket {expected}",
                    current.entity_id()
                )),
            );
        }
        Ok(())
    }

    // --- Bin index helpers (PancakeSwap LB-style ladder) -------------------

    /// Maps a six-decimal `floor_price_minor` (or oracle rate) to a 24-bit
    /// bin id on the LB log-spaced ladder. Saturates to `[0, MAX_BIN_ID]` -
    /// see `lb_math::get_id_from_price` for the deviation from LB's revert.
    pub fn price_to_bin(price_minor: U256) -> Result<u32> {
        if price_minor.is_zero() {
            return Ok(0);
        }
        reference_price::coen_iso_price_to_bin_id(price_minor, BIN_STEP_BP)
    }

    /// Inverse of `price_to_bin`: returns the lower edge of bin `bin_id` in
    /// six-decimal minor units. Diagnostic-only - `bin_to_price_floor` may
    /// fail at extreme bin ids whose LB-pow exponent exceeds `2^20`.
    pub fn bin_to_price_floor(bin_id: u32) -> Result<U256> {
        reference_price::bin_id_to_coen_iso_price(bin_id, BIN_STEP_BP)
    }

    /// Namespaces a bin-column key by the bucket's reference currency.
    ///
    /// Mapping keys are left-padded to 32 bytes before hashing, so a wider
    /// integer type alone namespaces nothing - the ISO has to occupy real
    /// high bits. Bin ids are 24-bit and the trie's mid/leaf keys are 16-bit,
    /// so the low 32 bits always hold `key` unambiguously. ISO `0` is the one
    /// value that would alias the un-namespaced key; `record_nod_issued`
    /// rejects it at the funnel so it can never be written.
    pub(crate) const fn scoped(reference_currency: u16, key: u32) -> u64 {
        ((reference_currency as u64) << 32) | key as u64
    }

    /// Storage key for the `index`-th bucket_key parked in bin `bin_id` of
    /// `reference_currency`. Mirrors the `owner_index_key` keccak-of-concat
    /// pattern.
    pub(crate) fn bin_index_key(reference_currency: u16, bin_id: u32, index: u32) -> B256 {
        let mut buf = [0u8; 10];
        buf[0..2].copy_from_slice(&reference_currency.to_be_bytes());
        buf[2..6].copy_from_slice(&bin_id.to_be_bytes());
        buf[6..10].copy_from_slice(&index.to_be_bytes());
        alloy_primitives::keccak256(buf)
    }

    /// Parks a new bucket in the bin of its sealed call price.
    pub(crate) fn insert_call_bin(&mut self, bucket_key: B256) -> Result<()> {
        let iso = self.callable_bucket_currency.read(&bucket_key)?;
        let bin_id = Self::price_to_bin(self.callable_bucket_call_price.read(&bucket_key)?)?;
        let scoped = Self::scoped(iso, bin_id);
        let count = self.call_bin_count.read(&scoped)?;
        let next_count = count.checked_add(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Fatal(format!(
                "Nod call bin {iso}:{bin_id} member count overflow"
            ))
        })?;
        self.call_bin_buckets
            .write(&Self::bin_index_key(iso, bin_id, count), bucket_key)?;
        self.call_bin_count.write(&scoped, next_count)?;
        self.call_bucket_bin
            .write(&bucket_key, pack_bin_slot(bin_id, count))?;
        tree_math::add(&CallBins(self, iso), bin_id)?;
        Ok(())
    }

    /// No-op for a bucket the trie does not hold.
    pub(crate) fn remove_call_bin(&mut self, bucket_key: B256) -> Result<()> {
        let packed = self.call_bucket_bin.read(&bucket_key)?;
        if packed == 0 {
            return Ok(());
        }
        let (bin_id, index) = unpack_bin_slot(packed);
        let iso = self.callable_bucket_currency.read(&bucket_key)?;
        if self
            .call_bin_buckets
            .read(&Self::bin_index_key(iso, bin_id, index))?
            != bucket_key
        {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod call bin {iso}:{bin_id} does not hold bucket {bucket_key} at {index}"
                )),
            );
        }
        let scoped = Self::scoped(iso, bin_id);
        let last = self
            .call_bin_count
            .read(&scoped)?
            .checked_sub(1)
            .filter(|last| index <= *last)
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod call bin {iso}:{bin_id} does not hold bucket {bucket_key} at {index}"
                ))
            })?;
        let last_key = Self::bin_index_key(iso, bin_id, last);
        if index != last {
            let moved = self.call_bin_buckets.read(&last_key)?;
            self.call_bin_buckets
                .write(&Self::bin_index_key(iso, bin_id, index), moved)?;
            self.call_bucket_bin
                .write(&moved, pack_bin_slot(bin_id, index))?;
        }
        self.call_bin_buckets.write(&last_key, B256::ZERO)?;
        self.call_bin_count.write(&scoped, last)?;
        self.call_bucket_bin.clear(&bucket_key)?;
        if last == 0 {
            tree_math::remove(&CallBins(self, iso), bin_id)?;
        }
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
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bucket {bucket_key} member count underflow during removal"
                ))
            })?;
        if index > last
            || self
                .bucket_nods
                .read(&Self::bucket_nod_key(bucket_key, index))?
                != nod_id
        {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod {nod_id} is not indexed in bucket {bucket_key}"
                )),
            );
        }
        let last_key = Self::bucket_nod_key(bucket_key, last);
        if index != last {
            let moved = self.bucket_nods.read(&last_key)?;
            if moved.is_zero() {
                return Err(
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Nod bucket {bucket_key} member slot {last} is empty during removal"
                    )),
                );
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
    /// join the same bucket inherit this copy; nothing reads the constants again.
    pub(crate) fn seal_bucket_call_terms(
        &mut self,
        bucket_key: B256,
        terms: CallTerms,
    ) -> Result<()> {
        self.callable_bucket_call_price
            .write(&bucket_key, terms.call_price)?;
        self.callable_bucket_currency
            .write(&bucket_key, terms.reference_currency)?;
        self.callable_bucket_call_rate
            .write(&bucket_key, terms.call_rate)?;
        self.callable_bucket_call_window
            .write(&bucket_key, terms.call_window)?;
        self.callable_bucket_call_threshold
            .write(&bucket_key, terms.call_threshold)?;
        self.callable_bucket_call_notice_period
            .write(&bucket_key, terms.call_notice_period)?;
        self.widen_max_call_window(terms.reference_currency, terms.call_window)
    }

    /// Puts a called bucket on the list the forfeit arm walks.
    pub(crate) fn push_called_bucket(&mut self, bucket_key: B256) -> Result<()> {
        let index = self.called_buckets.len()?;
        self.called_buckets.push(bucket_key)?;
        self.called_bucket_index.write(&bucket_key, index)
    }

    /// Reads back the terms [`Self::seal_bucket_call_terms`] sealed at issuance.
    pub(crate) fn read_call_terms(&self, bucket_key: B256) -> Result<CallTerms> {
        Ok(CallTerms {
            call_price: self.callable_bucket_call_price.read(&bucket_key)?,
            reference_currency: self.callable_bucket_currency.read(&bucket_key)?,
            call_rate: self.callable_bucket_call_rate.read(&bucket_key)?,
            call_window: self.callable_bucket_call_window.read(&bucket_key)?,
            call_threshold: self.callable_bucket_call_threshold.read(&bucket_key)?,
            call_notice_period: self.callable_bucket_call_notice_period.read(&bucket_key)?,
        })
    }

    /// Raises the currency's widest-window high-water mark if this bucket
    /// outruns it. Monotonic, so the daily scan can size one shared VWAP window
    /// per currency and still cover every bucket denominated in it. Mirrors
    /// `outbe_gem`'s `max_call_window`.
    fn widen_max_call_window(&mut self, reference_currency: u16, call_window: u32) -> Result<()> {
        if call_window > self.max_call_window.read(&reference_currency)? {
            self.max_call_window
                .write(&reference_currency, call_window)?;
        }
        Ok(())
    }

    /// Swap-removes a bucket from the called list. No-op for a bucket it does not hold.
    fn remove_called_bucket(&mut self, bucket_key: B256) -> Result<()> {
        let len = self.called_buckets.len()?;
        let index = self.called_bucket_index.read(&bucket_key)?;
        let listed = index < len
            && self
                .called_buckets
                .get(index)?
                .is_some_and(|listed| listed == bucket_key);
        if listed {
            let last = len.checked_sub(1).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod called list underflow removing bucket {bucket_key}"
                ))
            })?;
            if index != last {
                let moved = self.called_buckets.get(last)?.ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Nod called list slot {last} is empty during removal"
                    ))
                })?;
                self.called_buckets.set(index, moved)?;
                self.called_bucket_index.write(&moved, index)?;
            }
            self.called_buckets.pop()?;
        }
        self.called_bucket_index.clear(&bucket_key)
    }

    /// No-op for a bucket the call index never held, so the removal funnel can call it unconditionally.
    pub(crate) fn remove_callable_bucket(&mut self, bucket_key: B256) -> Result<()> {
        self.remove_call_bin(bucket_key)?;
        self.remove_called_bucket(bucket_key)?;
        self.callable_bucket_call_price.clear(&bucket_key)?;
        self.callable_bucket_currency.get(&bucket_key).delete()?;
        self.callable_bucket_call_rate.get(&bucket_key).delete()?;
        self.callable_bucket_call_window.clear(&bucket_key)?;
        self.callable_bucket_call_threshold.clear(&bucket_key)?;
        self.callable_bucket_call_notice_period.clear(&bucket_key)?;
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

const fn pack_bin_slot(bin_id: u32, index: u32) -> u64 {
    ((bin_id as u64) << 32) | (index as u64 + 1)
}

const fn unpack_bin_slot(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, (packed as u32).wrapping_sub(1))
}

/// One currency's call-price trie, like `outbe_gem::state::CallBins`.
pub(crate) struct CallBins<'a, 'storage>(pub(crate) &'a NodContract<'storage>, pub(crate) u16);

impl BinTreeStorage for CallBins<'_, '_> {
    fn read_root(&self) -> Result<U256> {
        self.0.call_bin_tree_root.read(&self.1)
    }
    fn write_root(&self, value: U256) -> Result<()> {
        self.0.call_bin_tree_root.write(&self.1, value)
    }
    fn read_mid(&self, key: u32) -> Result<U256> {
        self.0
            .call_bin_tree_mid
            .read(&NodContract::scoped(self.1, key))
    }
    fn write_mid(&self, key: u32, value: U256) -> Result<()> {
        self.0
            .call_bin_tree_mid
            .write(&NodContract::scoped(self.1, key), value)
    }
    fn read_leaf(&self, key: u32) -> Result<U256> {
        self.0
            .call_bin_tree_leaf
            .read(&NodContract::scoped(self.1, key))
    }
    fn write_leaf(&self, key: u32, value: U256) -> Result<()> {
        self.0
            .call_bin_tree_leaf
            .write(&NodContract::scoped(self.1, key), value)
    }
}
