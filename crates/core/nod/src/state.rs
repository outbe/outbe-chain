use alloy_primitives::{Address, B256, U256};
use base64::Engine;
use outbe_primitives::error::Result;
use outbe_primitives::math::{
    constants::MAX_BIN_ID,
    price_helper,
    tree_math::{self, BinTreeStorage},
};

use crate::{
    constants::{BIN_STEP_BP, TOKEN_DESCRIPTION, TOKEN_IMAGE_BASE},
    errors::NodError,
    precompile::INod,
    schema::{NodBucketState, NodContract, NodItemState},
};

impl NodContract<'_> {
    // --- ID helpers ---

    pub fn format_nod_id(nod_id: U256) -> String {
        hex::encode(nod_id.to_be_bytes::<32>())
    }

    pub fn parse_nod_id(nod_id: &str) -> Result<U256> {
        let trimmed = nod_id.strip_prefix("0x").unwrap_or(nod_id);
        if trimmed.len() != 64 {
            return Err(NodError::InvalidNodIdLength.into());
        }
        let mut buf = [0u8; 32];
        hex::decode_to_slice(trimmed, &mut buf).map_err(|_| NodError::InvalidNodIdHex)?;
        Ok(U256::from_be_bytes(buf))
    }

    // --- View functions ---

    pub fn total_supply(&self) -> Result<u64> {
        self.total_supply.read()
    }

    pub fn owner_of(&self, nod_id: U256) -> Result<Address> {
        let item = self.get_item(nod_id)?.ok_or(NodError::NodNotFound)?;
        Ok(item.owner)
    }

    pub fn get_item(&self, nod_id: U256) -> Result<Option<NodItemState>> {
        self.nod_items.get(nod_id)
    }

    pub fn get_bucket(&self, bucket_key: B256) -> Result<Option<NodBucketState>> {
        self.nod_buckets.get(bucket_key)
    }

    pub fn token_uri(&self, nod_id: U256) -> Result<String> {
        let item = self.get_item(nod_id)?.ok_or(NodError::NodNotFound)?;
        let bucket_key = self
            .nod_items
            .get(nod_id)?
            .ok_or(NodError::NodNotFound)?
            .bucket_key;
        let bucket = self
            .get_bucket(bucket_key)?
            .ok_or(NodError::BucketNotFound)?;
        let cost_amount_minor = item.cost_amount_minor;
        let nod_id_str = Self::format_nod_id(nod_id);
        let token_id_decimal = nod_id.to_string();
        let json = format!(
            "{{\"name\":\"Nod #{}\",\"description\":\"{}\",\"image\":\"{}{}\",\"attributes\":[{{\"trait_type\":\"token_id\",\"value\":\"{}\"}},{{\"trait_type\":\"worldwide_day\",\"value\":{}}},{{\"trait_type\":\"league_id\",\"value\":{}}},{{\"trait_type\":\"floor_price_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"gratis_load_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"cost_of_gratis_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"cost_amount_minor\",\"value\":\"{}\"}},{{\"trait_type\":\"is_qualified\",\"value\":{}}},{{\"trait_type\":\"issued_at\",\"value\":{}}},{{\"trait_type\":\"reference_currency\",\"value\":{}}},{{\"trait_type\":\"issuance_currency\",\"value\":{}}}]}}",
            &nod_id_str[..8],
            TOKEN_DESCRIPTION,
            TOKEN_IMAGE_BASE,
            nod_id_str,
            token_id_decimal,
            item.worldwide_day,
            item.league_id,
            item.floor_price_minor,
            item.gratis_load_minor,
            bucket.entry_price_minor,
            cost_amount_minor,
            if bucket.is_qualified { "true" } else { "false" },
            item.issued_at,
            item.reference_currency,
            item.issuance_currency,
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        Ok(format!("data:application/json;base64,{}", encoded))
    }

    // --- Enumeration index helpers ---

    pub(crate) fn owner_index_key(owner: Address, index: u32) -> B256 {
        let mut buf = [0u8; 24];
        buf[0..20].copy_from_slice(owner.as_slice());
        buf[20..24].copy_from_slice(&index.to_be_bytes());
        alloy_primitives::keccak256(buf)
    }

    pub fn get_nods_by_owner(&self, owner: Address) -> Result<Vec<U256>> {
        let count = self.owner_nod_counts.read(&owner)?;
        (0..count)
            .map(|i| Self::owner_index_key(owner, i))
            .map(|key| self.owner_nod_ids.read(&key))
            .collect::<Result<Vec<_>>>()
    }

    pub fn get_nod_by_owner_idx(&self, owner: Address, index: u32) -> Result<U256> {
        let count = self.owner_nod_counts.read(&owner)?;
        if index >= count {
            return Err(NodError::IndexOutOfBounds.into());
        }
        let key = Self::owner_index_key(owner, index);
        self.owner_nod_ids.read(&key)
    }

    pub fn get_nods_count_by_owner(&self, owner: Address) -> Result<u32> {
        self.owner_nod_counts.read(&owner)
    }

    /// Swap-and-pop the per-owner index for `nod_id`, keeping the array dense.
    /// The owner's index has no reverse lookup, so finds the slot via linear
    /// scan over the current count (bounded by the owner's balance).
    pub(crate) fn compact_owner_index(&mut self, owner: Address, nod_id: U256) -> Result<()> {
        let count = self.owner_nod_counts.read(&owner)?;
        let last = count.checked_sub(1).ok_or(NodError::NodNotFound)?;
        let mut found: Option<u32> = None;
        for i in 0..count {
            let key = Self::owner_index_key(owner, i);
            if self.owner_nod_ids.read(&key)? == nod_id {
                found = Some(i);
                break;
            }
        }
        let idx = found.ok_or(NodError::NodNotFound)?;
        let last_key = Self::owner_index_key(owner, last);
        if idx != last {
            let last_id = self.owner_nod_ids.read(&last_key)?;
            self.owner_nod_ids
                .write(&Self::owner_index_key(owner, idx), last_id)?;
        }
        self.owner_nod_ids.clear(&last_key)?;
        self.owner_nod_counts.write(&owner, last)?;
        Ok(())
    }

    /// Test-only direct bucket setup without projection emission.
    #[cfg(test)]
    pub fn set_qualified(&mut self, bucket_key: B256, is_qualified: bool) -> Result<()> {
        if let Some(mut state) = self.nod_buckets.get(bucket_key)? {
            state.is_qualified = is_qualified;
            self.nod_buckets.update(&state)?;
        }
        Ok(())
    }

    // --- Collection-wide Nod CRUD ------------------------------------------
    //
    // `add_nod` / `remove_nod` are the single point of contact for every slot
    // collection that mirrors a Nod's existence: the primary `nod_items` map,
    // the per-bucket aggregate (with the unqualified bin-tree for newly-created
    // buckets), the per-owner enumerable index, the global enumerable list and
    // its reverse lookup, and `total_supply`. Callers in `runtime.rs` keep only
    // the business rules (validation, PoW, qualification, event emission) and
    // delegate slot bookkeeping here.

    /// Insert `item` into every Nod slot collection and bump bucket + supply.
    ///
    /// `entry_price_minor` is only consumed when the bucket is created on
    /// this call; it is not stored on `NodItemState` so the caller passes it
    /// explicitly. Caller is responsible for asserting non-existence of
    /// `item.nod_id` and validating inputs before calling.
    pub(crate) fn add_nod(&mut self, item: &NodItemState, entry_price_minor: U256) -> Result<()> {
        self.nod_items.create(item)?;

        let final_bucket = match self.nod_buckets.get(item.bucket_key)? {
            Some(mut bucket) => {
                bucket.total_nods = bucket.total_nods.saturating_add(1);
                self.nod_buckets.update(&bucket)?;
                bucket
            }
            None => {
                let bucket = NodBucketState {
                    bucket_key: item.bucket_key,
                    worldwide_day: item.worldwide_day,
                    floor_price_minor: item.floor_price_minor,
                    is_qualified: false,
                    total_nods: 1,
                    entry_price_minor,
                };
                self.nod_buckets.create(&bucket)?;
                // Park the new bucket in the LB-style bin-tree, indexed by
                // its floor_price_minor. The qualifier hook drains bins
                // ascending until it crosses the oracle rate.
                self.insert_unqualified(item.bucket_key, item.floor_price_minor)?;
                bucket
            }
        };

        let oc = self.owner_nod_counts.read(&item.owner)?;
        self.owner_nod_ids
            .write(&Self::owner_index_key(item.owner, oc), item.nod_id)?;
        self.owner_nod_counts.write(&item.owner, oc + 1)?;

        let idx = self.global_nod_ids.len()?;
        self.global_nod_ids.push(item.nod_id)?;
        self.global_nod_index.write(&item.nod_id, idx)?;

        let supply = self.total_supply.read()?;
        self.total_supply.write(supply + 1)?;

        self.emit_nod_body_stored(item)?;
        self.emit_bucket_body_stored(&final_bucket)?;

        Ok(())
    }

    /// Remove `item` from every Nod slot collection and decrement bucket + supply.
    ///
    /// Caller has already loaded `item` (from `nod_items.get`) and verified
    /// authorization plus any business preconditions (e.g. bucket qualified).
    /// Does not touch the unqualified bin-tree: qualified buckets are no
    /// longer parked there.
    pub(crate) fn remove_nod(&mut self, item: &NodItemState) -> Result<()> {
        self.nod_items.delete(item.nod_id)?;

        let idx = self.global_nod_index.read(&item.nod_id)?;
        let last = self
            .global_nod_ids
            .len()?
            .checked_sub(1)
            .ok_or(NodError::NodNotFound)?;
        if idx != last {
            let last_id = self
                .global_nod_ids
                .get(last)?
                .ok_or(NodError::NodNotFound)?;
            self.global_nod_ids.set(idx, last_id)?;
            self.global_nod_index.write(&last_id, idx)?;
        }
        self.global_nod_ids.pop()?;
        self.global_nod_index.clear(&item.nod_id)?;

        self.compact_owner_index(item.owner, item.nod_id)?;

        let final_bucket = match self.nod_buckets.get(item.bucket_key)? {
            Some(mut bucket) => {
                bucket.total_nods = bucket.total_nods.saturating_sub(1);
                if bucket.total_nods == 0 {
                    self.nod_buckets.delete(item.bucket_key)?;
                    None
                } else {
                    self.nod_buckets.update(&bucket)?;
                    Some(bucket)
                }
            }
            None => return Err(NodError::BucketNotFound.into()),
        };

        let supply = self.total_supply.read()?;
        if supply > 0 {
            self.total_supply.write(supply - 1)?;
        }

        self.emit(INod::NodBodyDeleted { nodId: item.nod_id })?;
        if let Some(bucket) = final_bucket {
            self.emit_bucket_body_stored(&bucket)?;
        } else {
            self.emit(INod::NodBucketBodyDeleted {
                bucketKey: item.bucket_key,
            })?;
        }

        Ok(())
    }

    fn emit_nod_body_stored(&mut self, item: &NodItemState) -> Result<()> {
        self.emit(INod::NodBodyStored {
            nodId: item.nod_id,
            owner: item.owner,
            gratisLoadMinor: item.gratis_load_minor,
            worldwideDay: item.worldwide_day.into(),
            leagueId: item.league_id,
            floorPriceMinor: item.floor_price_minor,
            bucketKey: item.bucket_key,
            costAmountMinor: item.cost_amount_minor,
            issuanceCurrency: item.issuance_currency,
            referenceCurrency: item.reference_currency,
            issuedAt: item.issued_at,
        })
    }

    fn emit_bucket_body_stored(&mut self, bucket: &NodBucketState) -> Result<()> {
        self.emit(INod::NodBucketBodyStored {
            bucketKey: bucket.bucket_key,
            worldwideDay: bucket.worldwide_day.into(),
            floorPriceMinor: bucket.floor_price_minor,
            isQualified: bucket.is_qualified,
            totalNods: bucket.total_nods,
            entryPriceMinor: bucket.entry_price_minor,
        })
    }

    // --- Bin index helpers (PancakeSwap LB-style ladder) -------------------

    /// Maps a 1e18-scaled `floor_price_minor` (or oracle rate) to a 24-bit
    /// bin id on the LB log-spaced ladder. Saturates to `[0, MAX_BIN_ID]` —
    /// see `lb_math::get_id_from_price` for the deviation from LB's revert.
    pub fn price_to_bin(price_minor: U256) -> Result<u32> {
        if price_minor.is_zero() {
            return Ok(0);
        }
        let p_128x128 = price_helper::convert_decimal_price_to_128x128(price_minor)?;
        price_helper::get_id_from_price(p_128x128, BIN_STEP_BP)
    }

    /// Inverse of `price_to_bin`: returns the lower edge of bin `bin_id` in
    /// 1e18-scaled minor units. Diagnostic-only — `bin_to_price_floor` may
    /// fail at extreme bin ids whose LB-pow exponent exceeds `2^20`.
    pub fn bin_to_price_floor(bin_id: u32) -> Result<U256> {
        let p_128x128 = price_helper::get_price_from_id(bin_id, BIN_STEP_BP)?;
        price_helper::convert_128x128_price_to_decimal(p_128x128)
    }

    /// Storage key for the `index`-th bucket_key parked in bin `bin_id`.
    /// Mirrors the existing `owner_index_key` keccak-of-concat pattern.
    pub(crate) fn bin_index_key(bin_id: u32, index: u32) -> B256 {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&bin_id.to_be_bytes());
        buf[4..8].copy_from_slice(&index.to_be_bytes());
        alloy_primitives::keccak256(buf)
    }

    /// Reads all bucket_keys currently parked in `bin_id` in insertion order.
    /// Skips zero entries (defensive — should not occur in normal operation).
    /// Currently unused at runtime (the qualifier hook inlines the iteration
    /// to interleave per-bucket survivor decisions with reads); kept for
    /// admin/debug surfaces.
    #[allow(dead_code)]
    pub(crate) fn read_bin_buckets(&self, bin_id: u32) -> Result<Vec<B256>> {
        let count = self.unqualified_bin_count.read(&bin_id)?;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let bk = self
                .unqualified_bin_buckets
                .read(&Self::bin_index_key(bin_id, i))?;
            if !bk.is_zero() {
                out.push(bk);
            }
        }
        Ok(out)
    }

    /// Parks `bucket_key` in the bin for `floor_price` and marks the bin
    /// non-empty in the bitmap trie. Called from `runtime::issue` when a new
    /// bucket is created (i.e., the first NOD with a given
    /// `(wwd, floor_price)` pair is issued).
    pub(crate) fn insert_unqualified(&mut self, bucket_key: B256, floor_price: U256) -> Result<()> {
        let bin_id = Self::price_to_bin(floor_price)?;
        debug_assert!(bin_id <= MAX_BIN_ID);
        let count = self.unqualified_bin_count.read(&bin_id)?;
        self.unqualified_bin_buckets
            .write(&Self::bin_index_key(bin_id, count), bucket_key)?;
        self.unqualified_bin_count.write(&bin_id, count + 1)?;
        tree_math::add(self, bin_id)?;
        Ok(())
    }
}

// --- BinTreeStorage impl ---------------------------------------------------
//
// Adapter between the contract's three bin-tree storage slots and the
// `tree_math::BinTreeStorage` trait. The trait functions take `&self` —
// storage writes go through the DSL's interior-mutable `StorageHandle`,
// so no `&mut` is needed at any call site.

impl BinTreeStorage for NodContract<'_> {
    fn read_root(&self) -> Result<U256> {
        self.bin_tree_root.read()
    }
    fn write_root(&self, value: U256) -> Result<()> {
        self.bin_tree_root.write(value)
    }
    fn read_mid(&self, key: u32) -> Result<U256> {
        self.bin_tree_mid.read(&key)
    }
    fn write_mid(&self, key: u32, value: U256) -> Result<()> {
        self.bin_tree_mid.write(&key, value)
    }
    fn read_leaf(&self, key: u32) -> Result<U256> {
        self.bin_tree_leaf.read(&key)
    }
    fn write_leaf(&self, key: u32, value: U256) -> Result<()> {
        self.bin_tree_leaf.write(&key, value)
    }
}
