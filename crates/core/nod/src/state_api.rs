use crate::errors::NodError;
use crate::precompile::INod;
use crate::schema::{CallTerms, NodBucketState, NodContract, NodItemState};
use crate::storage_records::{StoredNod, StoredNodBucket, StoredNodEntryExt};
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    error::{PrecompileError, Result},
    nod_encryption::NOD_BLOB_LEN,
    storage::StorageHandle,
    wwd_entity_id::WwdEntityId,
};

pub struct LoadedNodItem {
    body: NodItemState,
    current: StoredNod,
}
impl LoadedNodItem {
    pub const fn body(&self) -> &NodItemState {
        &self.body
    }
    pub(crate) fn into_parts(self) -> (NodItemState, StoredNod) {
        (self.body, self.current)
    }
}

pub struct LoadedNodBucket {
    body: NodBucketState,
    current: StoredNodBucket,
}
impl LoadedNodBucket {
    pub const fn body(&self) -> &NodBucketState {
        &self.body
    }
    pub(crate) fn into_parts(self) -> (NodBucketState, StoredNodBucket) {
        (self.body, self.current)
    }
}

pub fn load_item(storage: &StorageHandle<'_>, id: WwdEntityId) -> Result<Option<LoadedNodItem>> {
    let contract = NodContract::new(storage.clone());
    let entry = contract.records.entry(id);
    if !entry.exists()? {
        return Ok(None);
    }
    if !entry
        .encrypted_creator_public_key()
        .has_exact_length(NOD_BLOB_LEN)?
        || !entry
            .encrypted_gratis_amount()
            .has_exact_length(NOD_BLOB_LEN)?
    {
        return Err(PrecompileError::fatal(
            "invalid stored Nod ciphertext length",
        ));
    }
    let current = entry.load()?.ok_or(NodError::NodNotFound)?;
    let body: NodItemState = current.clone().into();
    if !body.encrypted.has_valid_encoding() {
        return Err(NodError::EncryptedTermsMismatch.into());
    }
    Ok(Some(LoadedNodItem { body, current }))
}
pub fn load_bucket(storage: &StorageHandle<'_>, key: B256) -> Result<Option<LoadedNodBucket>> {
    let contract = NodContract::new(storage.clone());
    contract
        .buckets
        .get(key)?
        .map(|current| {
            Ok(LoadedNodBucket {
                body: current.clone().into(),
                current,
            })
        })
        .transpose()
}
pub fn add_item(
    storage: &StorageHandle<'_>,
    item: &NodItemState,
    entry: U256,
    terms: Option<CallTerms>,
) -> Result<()> {
    storage.with_checkpoint(|| {
        NodContract::new(storage.clone()).record_nod_issued_state(item, entry, terms)
    })
}
pub fn record_settled(
    storage: &StorageHandle<'_>,
    item: LoadedNodItem,
    bucket: LoadedNodBucket,
) -> Result<()> {
    storage.with_checkpoint(|| {
        NodContract::new(storage.clone()).record_nod_settled_state(item, bucket)
    })
}
pub fn remove_item(
    storage: &StorageHandle<'_>,
    item: LoadedNodItem,
    bucket: LoadedNodBucket,
) -> Result<()> {
    storage.with_checkpoint(|| {
        NodContract::new(storage.clone()).record_nod_removed_state(item, bucket)
    })
}
fn check_loaded(
    contract: &NodContract<'_>,
    item: &NodItemState,
    current: &StoredNod,
    bucket: &StoredNodBucket,
) -> Result<()> {
    if item.bucket_key != bucket.bucket_key
        || contract.records.get(item.nod_id)?.as_ref() != Some(current)
        || contract.buckets.get(bucket.bucket_key)?.as_ref() != Some(bucket)
    {
        return Err(PrecompileError::revert("Nod state changed after loading"));
    }
    Ok(())
}
impl NodContract<'_> {
    fn record_nod_issued_state(
        &mut self,
        item: &NodItemState,
        entry_price_minor: U256,
        call_terms: Option<CallTerms>,
    ) -> Result<()> {
        crate::issuance::validate_item(item, entry_price_minor)?;
        if self.records.get(item.nod_id)?.is_some() {
            return Err(NodError::NodAlreadyExists.into());
        }

        let bucket_id = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key.0);
        let current_bucket = self.buckets.get(item.bucket_key)?;
        let member_count = self.bucket_nod_count.read(&item.bucket_key)?;
        let settled_count = current_bucket
            .as_ref()
            .map_or(0, |bucket| bucket.settled_nods);
        if current_bucket.is_some() != (member_count > 0 || settled_count > 0) {
            return Err(NodError::BucketExistenceDisagreesWithMemberCount {
                bucket_id,
                member_count,
            }
            .into());
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
                self.bucket_issued_at
                    .write(&item.bucket_key, item.issued_at)?;
                if let Some(terms) = call_terms {
                    self.seal_bucket_call_terms(item.bucket_key, terms)?;
                    self.insert_call_bin(item.bucket_key)?;
                }
                Some(bucket)
            }
        };

        let supply = self
            .total_supply
            .read()?
            .checked_add(1)
            .ok_or(NodError::TotalSupplyOverflowDuringIssuance)?;
        self.total_supply.write(supply)?;
        self.insert_bucket_member(item.bucket_key, item.nod_id)?;
        self.insert_state_record(&StoredNod::from(item))?;
        if let Some(bucket) = new_bucket {
            self.buckets.create(&StoredNodBucket::from(&bucket))?;
        }
        self.emit(INod::Transfer {
            from: Address::ZERO,
            to: item.owner,
            tokenId: item.nod_id.to_u256(),
        })
    }

    fn record_nod_settled_state(
        &mut self,
        item: LoadedNodItem,
        bucket: LoadedNodBucket,
    ) -> Result<()> {
        let (mut item, current_item) = item.into_parts();
        let (mut bucket, current_bucket) = bucket.into_parts();
        check_loaded(self, &item, &current_item, &current_bucket)?;
        if item.is_settled {
            return Err(NodError::AlreadySettled.into());
        }
        bucket.settled_nods = bucket
            .settled_nods
            .checked_add(1)
            .ok_or(NodError::SettledCountOverflow)?;
        self.remove_bucket_member(item.bucket_key, item.nod_id)?;
        item.is_settled = true;
        self.records.update(&StoredNod::from(&item))?;
        self.buckets.update(&StoredNodBucket::from(&bucket))?;
        self.emit(INod::MetadataUpdate {
            _tokenId: item.nod_id.to_u256(),
        })
    }

    fn record_nod_removed_state(
        &mut self,
        item: LoadedNodItem,
        bucket: LoadedNodBucket,
    ) -> Result<()> {
        let (item, current_item) = item.into_parts();
        let (mut bucket, current_bucket) = bucket.into_parts();
        check_loaded(self, &item, &current_item, &current_bucket)?;
        let bucket_id = WwdEntityId::from_day_and_digest(
            current_bucket.worldwide_day,
            current_bucket.bucket_key,
        );
        let supply = self
            .total_supply
            .read()?
            .checked_sub(1)
            .ok_or(NodError::TotalSupplyUnderflowDuringRemoval)?;
        self.total_supply.write(supply)?;
        let remaining = if item.is_settled {
            bucket.settled_nods = bucket
                .settled_nods
                .checked_sub(1)
                .ok_or(NodError::BucketSettledCountUnderflow { bucket_id })?;
            self.bucket_nod_count.read(&item.bucket_key)?
        } else {
            self.remove_bucket_member(item.bucket_key, item.nod_id)?
        };
        self.delete_state_record(item.nod_id)?;
        if remaining == 0 && bucket.settled_nods == 0 {
            self.bucket_worldwide_day.clear(&item.bucket_key)?;
            self.bucket_issued_at.clear(&item.bucket_key)?;
            self.bucket_nod_count.clear(&item.bucket_key)?;
            self.close_bucket(item.bucket_key)?;
            self.buckets.delete(bucket.bucket_key)?;
        } else if item.is_settled {
            self.buckets.update(&StoredNodBucket::from(&bucket))?;
        }
        self.emit(INod::Transfer {
            from: item.owner,
            to: Address::ZERO,
            tokenId: item.nod_id.to_u256(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_support, NodIssueParams};
    use outbe_primitives::{storage::hashmap::HashMapStorageProvider, time::WorldwideDay};

    fn stored_fixture() -> StoredNod {
        let day = WorldwideDay::new(20261015);
        let id = WwdEntityId::from_day_and_digest(day, B256::repeat_byte(7));
        let mut record = StoredNod::with_key(id);
        record.owner = Address::repeat_byte(1);
        record.chain_id = 1;
        record.worldwide_day = day;
        record.encrypted_creator_public_key = vec![0; NOD_BLOB_LEN];
        record.encrypted_creator_public_key[..8].copy_from_slice(&1u64.to_be_bytes());
        record.encrypted_gratis_amount = record.encrypted_creator_public_key.clone();
        record
    }

    #[test]
    fn state_nod_reads_absent_and_settled_records_and_rejects_invalid_encoding() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let contract = NodContract::new(storage.clone());
            let mut record = stored_fixture();
            let id = record.nod_id;
            assert!(load_item(&storage, id).unwrap().is_none());
            record.is_settled = true;
            contract.records.create(&record).unwrap();
            assert!(load_item(&storage, id).unwrap().unwrap().body().is_settled);
            for key_field in [true, false] {
                let mut invalid = record.clone();
                if key_field {
                    invalid.encrypted_creator_public_key[0] = 2;
                } else {
                    invalid.encrypted_gratis_amount[0] = 2;
                }
                contract.records.update(&invalid).unwrap();
                assert!(load_item(&storage, id).is_err());
            }
            record.worldwide_day = WorldwideDay::new(20261016);
            contract.records.update(&record).unwrap();
            assert!(load_item(&storage, id).is_err());
        });
    }

    #[test]
    fn state_nod_rejects_invalid_lengths_before_loading_payloads() {
        use outbe_primitives::storage::types::StorageKey;
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let contract = NodContract::new(storage.clone());
            let record = stored_fixture();
            let id = record.nod_id;
            contract.records.create(&record).unwrap();
            for field_offset in [11, 12] {
                let entry = contract.records.entry(id);
                let field = if field_offset == 11 {
                    entry.encrypted_creator_public_key()
                } else {
                    entry.encrypted_gratis_amount()
                };
                for length in [55, 57] {
                    field.write(&vec![0; length]).unwrap();
                    assert!(load_item(&storage, id).is_err());
                }
                let slot = id.mapping_slot(entry.base_slot() + U256::from(field_offset));
                for raw in [U256::MAX, U256::from(112)] {
                    storage.sstore(entry.address(), slot, raw).unwrap();
                    assert!(load_item(&storage, id).is_err());
                }
                storage
                    .sstore(entry.address(), slot, U256::from(113))
                    .unwrap();
                field.write(&record.encrypted_creator_public_key).unwrap();
            }
            assert!(load_item(&storage, id).unwrap().is_some());
        });
    }

    #[test]
    fn state_only_nod_settlement_rejects_stale_handles_and_burns_atomically() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let params = NodIssueParams {
                owner: Address::repeat_byte(1),
                worldwide_day: WorldwideDay::new(20261015),
                league_id: 1,
                gratis_load_minor: U256::from(5_000_000),
                entry_price_minor: U256::from(1_000_000),
                issuance_currency: 840,
                reference_currency: 840,
            };
            let encrypted = test_support::encrypted_fixture(&params, 1);
            let item = NodItemState {
                nod_id: encrypted.terms.nod_id,
                owner: params.owner,
                worldwide_day: params.worldwide_day,
                league_id: params.league_id,
                bucket_key: crate::identity::bucket_key(
                    params.worldwide_day,
                    params.entry_price_minor,
                    params.reference_currency,
                ),
                issuance_currency: params.issuance_currency,
                reference_currency: params.reference_currency,
                issued_at: 1,
                is_settled: false,
                encrypted,
            };
            add_item(&storage, &item, params.entry_price_minor, None).unwrap();
            let contract = NodContract::new(storage.clone());
            assert_eq!(contract.total_supply().unwrap(), 1);
            assert_eq!(contract.state_records(Some(item.owner)).unwrap().len(), 1);
            let stale_item = load_item(&storage, item.nod_id).unwrap().unwrap();
            let stale_bucket = load_bucket(&storage, item.bucket_key).unwrap().unwrap();
            record_settled(
                &storage,
                load_item(&storage, item.nod_id).unwrap().unwrap(),
                load_bucket(&storage, item.bucket_key).unwrap().unwrap(),
            )
            .unwrap();
            assert!(record_settled(&storage, stale_item, stale_bucket).is_err());
            assert_eq!(
                contract
                    .buckets
                    .get(item.bucket_key)
                    .unwrap()
                    .unwrap()
                    .settled_nods,
                1
            );
            assert_eq!(contract.bucket_nod_count.read(&item.bucket_key).unwrap(), 0);
            remove_item(
                &storage,
                load_item(&storage, item.nod_id).unwrap().unwrap(),
                load_bucket(&storage, item.bucket_key).unwrap().unwrap(),
            )
            .unwrap();
            assert_eq!(contract.total_supply().unwrap(), 0);
            assert!(contract.state_records(None).unwrap().is_empty());
            assert!(contract.buckets.get(item.bucket_key).unwrap().is_none());
        });
    }
}
