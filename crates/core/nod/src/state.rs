use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    delete, derive_poseidon_entity_id, list, mint, read, update, BodyInput, EntityId36, EntityRef,
    ExecutionScope, IdPageRequest, ParentBodySource, QueryRef, VerifiedBody, MAX_ID_PAGE_LIMIT,
};
use outbe_primitives::error::Result;
use outbe_primitives::math::{
    constants::MAX_BIN_ID,
    price_helper,
    tree_math::{self, BinTreeStorage},
};

use crate::{
    api::{LoadedNodBucket, LoadedNodItem},
    constants::BIN_STEP_BP,
    errors::NodError,
    schema::{NodBucketState, NodContract, NodItemState},
};

impl NodContract<'_> {
    // --- ID helpers ---

    pub fn format_nod_id(nod_id: EntityId36) -> String {
        nod_id.to_string()
    }

    pub fn parse_nod_id(nod_id: &str) -> Result<EntityId36> {
        let trimmed = nod_id.strip_prefix("0x").unwrap_or(nod_id);
        if trimmed.len() != EntityId36::LEN * 2 {
            return Err(NodError::InvalidNodIdLength.into());
        }
        let mut buf = [0u8; EntityId36::LEN];
        hex::decode_to_slice(trimmed, &mut buf).map_err(|_| NodError::InvalidNodIdHex)?;
        EntityId36::try_from(buf.as_slice())
            .map_err(|error| outbe_primitives::error::PrecompileError::Revert(error.to_string()))
    }

    // --- View functions ---

    pub fn total_supply(&self) -> Result<u64> {
        self.total_supply.read()
    }

    pub(crate) fn get_item_verified(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        nod_id: EntityId36,
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
        bucket_id: EntityId36,
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
        if self
            .get_item_verified(scope, parent, item.nod_id)?
            .is_some()
        {
            return Err(outbe_primitives::error::PrecompileError::Revert(
                "nod already exists".into(),
            ));
        }

        let bucket_id = EntityId36::new(item.worldwide_day, item.bucket_key.0);
        let current_bucket = self.get_bucket_verified(scope, parent, bucket_id)?;
        let final_bucket = match current_bucket.as_ref() {
            Some(current) => {
                let mut bucket = nod_bucket_from_verified(current)?;
                bucket.total_nods = bucket.total_nods.checked_add(1).ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Nod bucket {bucket_id} member count overflow"
                    ))
                })?;
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
                self.bucket_worldwide_day
                    .write(&item.bucket_key, item.worldwide_day)?;
                self.insert_unqualified(item.bucket_key, item.floor_price_minor)?;
                bucket
            }
        };

        let supply = self.total_supply.read()?.checked_add(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Nod total supply overflow during issuance".into(),
            )
        })?;
        self.total_supply.write(supply)?;
        let canonical_item = crate::repository::canonical_item(item);
        mint(
            self.storage_handle(),
            scope,
            BodyInput::NodItem(&canonical_item),
        )?;
        let canonical_bucket = crate::repository::canonical_bucket(&final_bucket);
        if let Some(current) = current_bucket {
            update(
                self.storage_handle(),
                scope,
                current,
                BodyInput::NodBucket(&canonical_bucket),
            )
        } else {
            mint(
                self.storage_handle(),
                scope,
                BodyInput::NodBucket(&canonical_bucket),
            )
        }
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
        let bucket_id = EntityId36::new(item.worldwide_day, item.bucket_key.0);
        if current_bucket.entity_id() != bucket_id {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "loaded Nod bucket {} does not match item bucket {bucket_id}",
                    current_bucket.entity_id()
                )),
            );
        }
        bucket.total_nods = bucket.total_nods.checked_sub(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                "Nod bucket {bucket_id} has zero members during removal"
            ))
        })?;

        let supply = self.total_supply.read()?.checked_sub(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Nod total supply underflow during removal".into(),
            )
        })?;
        self.total_supply.write(supply)?;
        delete(self.storage_handle(), scope, current_item)?;
        if bucket.total_nods == 0 {
            self.bucket_worldwide_day.clear(&item.bucket_key)?;
            delete(self.storage_handle(), scope, current_bucket)
        } else {
            let canonical = crate::repository::canonical_bucket(&bucket);
            update(
                self.storage_handle(),
                scope,
                current_bucket,
                BodyInput::NodBucket(&canonical),
            )
        }
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
        let next_count = count.checked_add(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Fatal(format!(
                "Nod unqualified bin {bin_id} member count overflow"
            ))
        })?;
        self.unqualified_bin_count.write(&bin_id, next_count)?;
        tree_math::add(self, bin_id)?;
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
