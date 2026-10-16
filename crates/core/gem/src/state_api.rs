use crate::errors::GemError;
use crate::precompile::IGem;
use crate::schema::{CallTerms, GemBucketState, GemContract, GemItemState};
use crate::storage_records::{StoredGem, StoredGemBucket};
use crate::GemState;
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
    wwd_entity_id::WwdEntityId,
};

pub struct LoadedGem {
    body: GemItemState,
    current: StoredGem,
}
impl LoadedGem {
    pub const fn body(&self) -> &GemItemState {
        &self.body
    }
    pub(crate) fn into_parts(self) -> (GemItemState, StoredGem) {
        (self.body, self.current)
    }
}

pub struct LoadedGemBucket {
    body: GemBucketState,
    current: StoredGemBucket,
}
impl LoadedGemBucket {
    pub const fn body(&self) -> &GemBucketState {
        &self.body
    }
    pub(crate) fn into_parts(self) -> (GemBucketState, StoredGemBucket) {
        (self.body, self.current)
    }
}

pub fn load_item(storage: &StorageHandle<'_>, id: WwdEntityId) -> Result<Option<LoadedGem>> {
    let contract = GemContract::new(storage.clone());
    contract
        .records
        .get(id)?
        .map(|current| {
            Ok(LoadedGem {
                body: current.clone().into(),
                current,
            })
        })
        .transpose()
}
pub fn load_bucket(storage: &StorageHandle<'_>, key: B256) -> Result<Option<LoadedGemBucket>> {
    let contract = GemContract::new(storage.clone());
    contract
        .buckets
        .get(key)?
        .map(|current| {
            Ok(LoadedGemBucket {
                body: current.clone().into(),
                current,
            })
        })
        .transpose()
}
pub fn add_item(
    storage: &StorageHandle<'_>,
    item: &GemItemState,
    entry: U256,
    terms: Option<CallTerms>,
) -> Result<()> {
    storage.with_checkpoint(|| {
        GemContract::new(storage.clone()).record_gem_issued_state(item, entry, terms)
    })
}
pub fn record_settled(
    storage: &StorageHandle<'_>,
    item: LoadedGem,
    bucket: LoadedGemBucket,
) -> Result<()> {
    storage.with_checkpoint(|| {
        GemContract::new(storage.clone()).record_gem_settled_state(item, bucket)
    })
}
pub fn remove_item(
    storage: &StorageHandle<'_>,
    item: LoadedGem,
    bucket: LoadedGemBucket,
) -> Result<()> {
    storage.with_checkpoint(|| {
        GemContract::new(storage.clone()).record_gem_removed_state(item, bucket)
    })
}
fn check_loaded(
    contract: &GemContract<'_>,
    item: &GemItemState,
    current: &StoredGem,
    bucket: &StoredGemBucket,
) -> Result<()> {
    if item.bucket_key != bucket.bucket_key
        || contract.records.get(item.gem_id)?.as_ref() != Some(current)
        || contract.buckets.get(bucket.bucket_key)?.as_ref() != Some(bucket)
    {
        return Err(PrecompileError::revert("Gem state changed after loading"));
    }
    Ok(())
}
impl GemContract<'_> {
    fn record_gem_issued_state(
        &mut self,
        item: &GemItemState,
        entry_price_minor: U256,
        call_terms: Option<CallTerms>,
    ) -> Result<()> {
        if self.records.get(item.gem_id)?.is_some() {
            return Err(GemError::AlreadyExists.into());
        }
        let bucket_key = item.bucket_key;
        let bucket_id = item.bucket_id();
        let current_bucket = self.buckets.get(item.bucket_key)?;
        let member_count = self.bucket_gem_count.read(&bucket_key)?;
        let settled_count = current_bucket
            .as_ref()
            .map_or(0, |bucket| bucket.settled_gems);
        if current_bucket.is_some() != (member_count > 0 || settled_count > 0) {
            return Err(GemError::BucketExistenceMismatch {
                bucket_id,
                member_count,
            }
            .into());
        }
        let new_bucket = match current_bucket {
            Some(_) => None,
            None => {
                self.bucket_issued_at.write(&bucket_key, item.issued_at)?;
                if let Some(terms) = call_terms {
                    self.seal_bucket_call_terms(bucket_key, terms)?;
                    self.insert_call_bin(bucket_key)?;
                }
                Some(GemBucketState {
                    bucket_key,
                    worldwide_day: item.worldwide_day(),
                    entry_price_minor,
                    reference_currency: item.reference_currency,
                    settled_gems: 0,
                })
            }
        };

        let supply = self
            .total_supply
            .read()?
            .checked_add(1)
            .ok_or(GemError::TotalSupplyOverflow)?;
        self.total_supply.write(supply)?;
        self.insert_bucket_member(bucket_key, item.gem_id.to_u256())?;
        self.insert_state_record(&StoredGem::from(item))?;
        if let Some(bucket) = new_bucket {
            self.buckets.create(&StoredGemBucket::from(&bucket))?;
        }
        self.emit(IGem::Transfer {
            from: Address::ZERO,
            to: item.owner,
            tokenId: item.gem_id.to_u256(),
        })
    }

    fn record_gem_settled_state(&mut self, item: LoadedGem, bucket: LoadedGemBucket) -> Result<()> {
        let (mut item, current_item) = item.into_parts();
        let (mut bucket, current_bucket) = bucket.into_parts();
        check_loaded(self, &item, &current_item, &current_bucket)?;
        if item.is_settled() {
            return Err(GemError::AlreadySettled.into());
        }
        bucket.settled_gems = bucket
            .settled_gems
            .checked_add(1)
            .ok_or(GemError::SettledCountOverflow)?;
        self.remove_bucket_member(item.bucket_key, item.gem_id.to_u256())?;
        item.called_at = self.bucket_called_at.read(&item.bucket_key)?;
        item.settled_at = self.storage.timestamp()?.to::<u64>();
        item.state = GemState::Settled as u8;
        self.records.update(&StoredGem::from(&item))?;
        self.buckets.update(&StoredGemBucket::from(&bucket))?;
        self.emit(IGem::MetadataUpdate {
            _tokenId: item.gem_id.to_u256(),
        })
    }

    fn record_gem_removed_state(&mut self, item: LoadedGem, bucket: LoadedGemBucket) -> Result<()> {
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
            .ok_or(GemError::TotalSupplyUnderflow)?;
        self.total_supply.write(supply)?;
        let remaining = if item.is_settled() {
            bucket.settled_gems = bucket
                .settled_gems
                .checked_sub(1)
                .ok_or(GemError::SettledCountUnderflow { bucket_id })?;
            self.bucket_gem_count.read(&item.bucket_key)?
        } else {
            self.remove_bucket_member(item.bucket_key, item.gem_id.to_u256())?
        };
        self.delete_state_record(item.gem_id)?;
        if remaining == 0 && bucket.settled_gems == 0 {
            self.close_bucket(item.bucket_key)?;
            self.buckets.delete(bucket.bucket_key)?;
        } else if item.is_settled() {
            self.buckets.update(&StoredGemBucket::from(&bucket))?;
        }
        self.emit(IGem::Transfer {
            from: item.owner,
            to: Address::ZERO,
            tokenId: item.gem_id.to_u256(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::{storage::hashmap::HashMapStorageProvider, time::WorldwideDay};

    #[test]
    fn state_only_gem_settlement_rejects_stale_handles_and_burns_atomically() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let day = WorldwideDay::new(20261015);
            let item = GemItemState {
                gem_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(7)),
                owner: Address::repeat_byte(1),
                gem_type: 0,
                promis_load_minor: U256::from(5_000_000),
                floor_price_minor: U256::ZERO,
                issuance_currency: 840,
                reference_currency: 840,
                issued_at: day.start_timestamp(),
                bucket_key: crate::identity::bucket_key(day, U256::from(1_000_000), 840),
                state: GemState::Issued as u8,
                called_at: 0,
                settled_at: 0,
            };
            add_item(&storage, &item, U256::from(1_000_000), None).unwrap();
            let contract = GemContract::new(storage.clone());
            assert_eq!(contract.total_supply.read().unwrap(), 1);
            let stale_item = load_item(&storage, item.gem_id).unwrap().unwrap();
            let stale_bucket = load_bucket(&storage, item.bucket_key).unwrap().unwrap();
            record_settled(
                &storage,
                load_item(&storage, item.gem_id).unwrap().unwrap(),
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
                    .settled_gems,
                1
            );
            assert_eq!(contract.bucket_gem_count.read(&item.bucket_key).unwrap(), 0);
            remove_item(
                &storage,
                load_item(&storage, item.gem_id).unwrap().unwrap(),
                load_bucket(&storage, item.bucket_key).unwrap().unwrap(),
            )
            .unwrap();
            assert_eq!(contract.total_supply.read().unwrap(), 0);
            assert!(contract.state_records(None).unwrap().is_empty());
            assert!(contract.buckets.get(item.bucket_key).unwrap().is_none());
        });
    }
}
