use alloy_primitives::{Bytes, B256, U256};
use outbe_compressed_entities::{
    body_commitment, derive_poseidon_entity_id, encode_nod_bucket_v1, encode_nod_item_v1,
    Commitment, CommitmentState, EntityId36, ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_primitives::error::Result;
use outbe_primitives::math::{
    constants::MAX_BIN_ID,
    price_helper,
    tree_math::{self, BinTreeStorage},
};

use crate::{
    constants::BIN_STEP_BP,
    errors::NodError,
    precompile::INod,
    schema::{NodBucketState, NodContract, NodItemState},
    NodRepositoryReader,
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
        reader: &NodRepositoryReader,
        nod_id: EntityId36,
    ) -> Result<Option<NodItemState>> {
        let Some(expected) = CommitmentState::new(self.storage_handle()).nod_item(nod_id)? else {
            return Ok(None);
        };
        let body = reader.get(nod_id)?.ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                "CommittedBodyMissing: Nod item {nod_id}"
            ))
        })?;
        self.verify_item(&body, expected)?;
        Ok(Some(body))
    }

    pub(crate) fn get_bucket_verified(
        &self,
        reader: &NodRepositoryReader,
        bucket_id: EntityId36,
    ) -> Result<Option<NodBucketState>> {
        let Some(expected) = CommitmentState::new(self.storage_handle()).nod_bucket(bucket_id)?
        else {
            return Ok(None);
        };
        let body = reader.get_bucket(bucket_id)?.ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                "CommittedBodyMissing: Nod bucket {bucket_id}"
            ))
        })?;
        self.verify_bucket(&body, expected)?;
        Ok(Some(body))
    }

    pub(crate) fn verify_item(&self, body: &NodItemState, expected: Commitment) -> Result<()> {
        let canonical_id =
            derive_poseidon_entity_id(body.owner, body.worldwide_day).map_err(|error| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(error.to_string())
            })?;
        if body.nod_id != canonical_id {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod item canonical identity mismatch: expected {canonical_id}, found {}",
                    body.nod_id
                )),
            );
        }
        let payload =
            encode_nod_item_v1(&crate::repository::canonical_item(body)).map_err(|error| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(error.to_string())
            })?;
        let actual = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            body.nod_id,
            &payload,
        )
        .map_err(|error| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(error.to_string())
        })?;
        if actual != expected {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod item commitment mismatch for {}",
                    body.nod_id
                )),
            );
        }
        Ok(())
    }

    pub(crate) fn verify_bucket(&self, body: &NodBucketState, expected: Commitment) -> Result<()> {
        let canonical = crate::repository::canonical_bucket(body);
        let payload = encode_nod_bucket_v1(&canonical).map_err(|error| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(error.to_string())
        })?;
        let actual = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            canonical.entity_id(),
            &payload,
        )
        .map_err(|error| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(error.to_string())
        })?;
        if actual != expected {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bucket commitment mismatch for {}",
                    canonical.entity_id()
                )),
            );
        }
        Ok(())
    }

    /// Records compact issuance state and emits the complete projected bodies.
    ///
    /// Full Nod and bucket bodies remain exclusively in the off-chain repository.
    pub(crate) fn record_nod_issued(
        &mut self,
        reader: &NodRepositoryReader,
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
        let commitments = CommitmentState::new(self.storage_handle());
        if commitments.nod_item(item.nod_id)?.is_some() {
            return Err(outbe_primitives::error::PrecompileError::Revert(
                "nod already exists".into(),
            ));
        }

        let item_payload = encode_nod_item_v1(&crate::repository::canonical_item(item))
            .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))?;
        let item_commitment = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            item.nod_id,
            &item_payload,
        )
        .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))?;

        let bucket_id = EntityId36::new(item.worldwide_day, item.bucket_key.0);
        let previous_bucket_commitment = commitments.nod_bucket(bucket_id)?;
        let final_bucket = match previous_bucket_commitment {
            Some(_) => {
                let mut bucket = self
                    .get_bucket_verified(reader, bucket_id)?
                    .ok_or(NodError::BucketNotFound)?;
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

        let bucket_payload = encode_nod_bucket_v1(&crate::repository::canonical_bucket(
            &final_bucket,
        ))
        .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))?;
        let bucket_commitment = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            bucket_id,
            &bucket_payload,
        )
        .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))?;

        let supply = self.total_supply.read()?.checked_add(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Nod total supply overflow during issuance".into(),
            )
        })?;
        self.total_supply.write(supply)?;
        commitments.set_nod_item(item.nod_id, item_commitment)?;
        commitments.set_nod_bucket(bucket_id, bucket_commitment)?;
        self.emit_nod_body_stored(item, None, item_commitment, item_payload)?;
        self.emit_bucket_body_stored(
            bucket_id,
            previous_bucket_commitment,
            bucket_commitment,
            bucket_payload,
        )
    }

    /// Records compact removal state and emits the projected body deletions/update.
    pub(crate) fn record_nod_removed(
        &mut self,
        reader: &NodRepositoryReader,
        item: &NodItemState,
    ) -> Result<()> {
        let commitments = CommitmentState::new(self.storage_handle());
        let previous_item_commitment = commitments.nod_item(item.nod_id)?.ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                "Nod item {} became canonically absent during removal",
                item.nod_id
            ))
        })?;
        let bucket_id = EntityId36::new(item.worldwide_day, item.bucket_key.0);
        let previous_bucket_commitment = commitments.nod_bucket(bucket_id)?.ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                "Nod bucket {bucket_id} became canonically absent during removal"
            ))
        })?;
        let mut bucket = self
            .get_bucket_verified(reader, bucket_id)?
            .ok_or(NodError::BucketNotFound)?;
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
        commitments.clear_nod_item(item.nod_id)?;

        self.emit(INod::NodBodyDeleted {
            nodId: Bytes::copy_from_slice(item.nod_id.as_bytes()),
            previousCommitment: B256::from(*previous_item_commitment.as_bytes()),
        })?;
        if bucket.total_nods == 0 {
            commitments.clear_nod_bucket(bucket_id)?;
            self.bucket_worldwide_day.clear(&item.bucket_key)?;
            self.emit(INod::NodBucketBodyDeleted {
                bucketId: Bytes::copy_from_slice(bucket_id.as_bytes()),
                previousCommitment: B256::from(*previous_bucket_commitment.as_bytes()),
            })
        } else {
            let payload = encode_nod_bucket_v1(&crate::repository::canonical_bucket(&bucket))
                .map_err(|error| {
                    outbe_primitives::error::PrecompileError::Fatal(error.to_string())
                })?;
            let new_commitment = body_commitment(
                ACTIVE_COMMITMENT_SCHEME,
                BODY_SCHEMA_V1,
                bucket_id,
                &payload,
            )
            .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))?;
            commitments.set_nod_bucket(bucket_id, new_commitment)?;
            self.emit_bucket_body_stored(
                bucket_id,
                Some(previous_bucket_commitment),
                new_commitment,
                payload,
            )
        }
    }

    fn emit_nod_body_stored(
        &mut self,
        item: &NodItemState,
        previous: Option<Commitment>,
        new_commitment: Commitment,
        payload: Vec<u8>,
    ) -> Result<()> {
        self.emit(INod::NodBodyStored {
            nodId: Bytes::copy_from_slice(item.nod_id.as_bytes()),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: previous.map_or(B256::ZERO, |value| B256::from(*value.as_bytes())),
            newCommitment: B256::from(*new_commitment.as_bytes()),
            canonicalPayload: Bytes::from(payload),
        })
    }

    pub(crate) fn emit_bucket_body_stored(
        &mut self,
        bucket_id: EntityId36,
        previous: Option<Commitment>,
        new_commitment: Commitment,
        payload: Vec<u8>,
    ) -> Result<()> {
        self.emit(INod::NodBucketBodyStored {
            bucketId: Bytes::copy_from_slice(bucket_id.as_bytes()),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: previous.map_or(B256::ZERO, |value| B256::from(*value.as_bytes())),
            newCommitment: B256::from(*new_commitment.as_bytes()),
            canonicalPayload: Bytes::from(payload),
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
