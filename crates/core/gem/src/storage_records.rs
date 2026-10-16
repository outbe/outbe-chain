use alloy_primitives::{Address, B256, U256};
use outbe_macros::storage_record;
use outbe_primitives::{time::WorldwideDay, wwd_entity_id::WwdEntityId};

#[derive(Clone, Debug, Eq, PartialEq)]
#[storage_record(exists_field = owner)]
pub struct StoredGem {
    #[key]
    pub gem_id: WwdEntityId,
    #[attribute(order = 0)]
    pub owner: Address,
    #[attribute(order = 1)]
    pub gem_type: u8,
    #[attribute(order = 2)]
    pub promis_load_minor: U256,
    #[attribute(order = 3)]
    pub issuance_currency: u16,
    #[attribute(order = 4)]
    pub reference_currency: u16,
    #[attribute(order = 5)]
    pub issued_at: u64,
    #[attribute(order = 6)]
    pub bucket_key: B256,
    #[attribute(order = 7)]
    pub state: u8,
    #[attribute(order = 8)]
    pub called_at: u64,
    #[attribute(order = 9)]
    pub settled_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[storage_record(exists_field = present)]
pub struct StoredGemBucket {
    #[key]
    pub bucket_key: B256,
    #[attribute(order = 0)]
    pub present: bool,
    #[attribute(order = 1)]
    pub worldwide_day: WorldwideDay,
    #[attribute(order = 2)]
    pub entry_price_minor: U256,
    #[attribute(order = 3)]
    pub reference_currency: u16,
    #[attribute(order = 4)]
    pub settled_gems: u64,
}

impl From<&crate::GemItemState> for StoredGem {
    fn from(body: &crate::GemItemState) -> Self {
        Self {
            gem_id: body.gem_id,
            owner: body.owner,
            gem_type: body.gem_type,
            promis_load_minor: body.promis_load_minor,
            issuance_currency: body.issuance_currency,
            reference_currency: body.reference_currency,
            issued_at: body.issued_at,
            bucket_key: body.bucket_key,
            state: body.state,
            called_at: body.called_at,
            settled_at: body.settled_at,
        }
    }
}

impl From<StoredGem> for crate::GemItemState {
    fn from(body: StoredGem) -> Self {
        Self {
            gem_id: body.gem_id,
            owner: body.owner,
            gem_type: body.gem_type,
            promis_load_minor: body.promis_load_minor,
            issuance_currency: body.issuance_currency,
            reference_currency: body.reference_currency,
            issued_at: body.issued_at,
            bucket_key: body.bucket_key,
            state: body.state,
            called_at: body.called_at,
            settled_at: body.settled_at,
        }
    }
}

impl From<&crate::GemBucketState> for StoredGemBucket {
    fn from(body: &crate::GemBucketState) -> Self {
        Self {
            present: true,
            bucket_key: body.bucket_key,
            worldwide_day: body.worldwide_day,
            entry_price_minor: body.entry_price_minor,
            reference_currency: body.reference_currency,
            settled_gems: body.settled_gems,
        }
    }
}

impl From<StoredGemBucket> for crate::GemBucketState {
    fn from(body: StoredGemBucket) -> Self {
        Self {
            bucket_key: body.bucket_key,
            worldwide_day: body.worldwide_day,
            entry_price_minor: body.entry_price_minor,
            reference_currency: body.reference_currency,
            settled_gems: body.settled_gems,
        }
    }
}

impl crate::GemContract<'_> {
    pub fn state_records(
        &self,
        owner: Option<Address>,
    ) -> outbe_primitives::error::Result<Vec<StoredGem>> {
        let mut ids = match owner {
            Some(owner) => self.owner_ids.list(&owner).read_all()?,
            None => self.all_ids.read_all()?,
        };
        ids.sort_unstable();
        if ids.windows(2).any(|ids| ids[0] == ids[1]) {
            return Err(outbe_primitives::error::PrecompileError::fatal(
                "gem index repeats an identity",
            ));
        }
        ids.into_iter()
            .map(|id| {
                let record = self.records.get(id)?.ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::fatal(
                        "gem index selects a missing record",
                    )
                })?;
                if owner.is_some_and(|owner| record.owner != owner) {
                    return Err(outbe_primitives::error::PrecompileError::fatal(
                        "gem owner index does not match its record",
                    ));
                }
                Ok(record)
            })
            .collect()
    }

    pub fn insert_state_record(&self, record: &StoredGem) -> outbe_primitives::error::Result<()> {
        use outbe_primitives::storage::indexed_list::IndexedList;
        let storage = self.records.storage();
        storage.with_checkpoint(|| {
            self.records.create(record)?;
            IndexedList::new(
                &self.owner_ids.list(&record.owner),
                self.owner_id_position.clone(),
            )
            .insert(record.gem_id)?;
            IndexedList::new(&self.all_ids, self.global_id_position.clone())
                .insert(record.gem_id)?;
            Ok(())
        })
    }

    pub fn delete_state_record(&self, id: WwdEntityId) -> outbe_primitives::error::Result<()> {
        use outbe_primitives::storage::indexed_list::IndexedList;
        let storage = self.records.storage();
        storage.with_checkpoint(|| {
            let record = self.records.get(id)?.ok_or_else(|| {
                outbe_primitives::error::PrecompileError::fatal(
                    "state record is absent from its index",
                )
            })?;
            IndexedList::new(
                &self.owner_ids.list(&record.owner),
                self.owner_id_position.clone(),
            )
            .remove(id)?;
            IndexedList::new(&self.all_ids, self.global_id_position.clone()).remove(id)?;
            self.records.delete(id)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    #[test]
    fn indexed_record_creation_reverts_if_an_index_rejects_it() {
        let mut provider = HashMapStorageProvider::new(54322345);
        StorageHandle::enter(&mut provider, |storage| {
            let contract = crate::GemContract::new(storage);
            let id =
                WwdEntityId::from_day_and_digest(WorldwideDay::new(20261015), B256::repeat_byte(7));
            let mut record = StoredGem::with_key(id);
            record.owner = Address::repeat_byte(1);
            contract.owner_id_position.write(&id, 9).unwrap();
            assert!(contract.insert_state_record(&record).is_err());
            assert!(contract.records.get(id).unwrap().is_none());
            assert!(contract.owner_ids.list(&record.owner).is_empty().unwrap());
            assert_eq!(contract.owner_id_position.read(&id).unwrap(), 9);
        });
    }

    #[test]
    fn state_record_crud_preserves_all_fields_and_clears_dynamic_data() {
        let mut provider = HashMapStorageProvider::new(54322345);
        StorageHandle::enter(&mut provider, |storage| {
            let contract = crate::GemContract::new(storage);
            let id =
                WwdEntityId::from_day_and_digest(WorldwideDay::new(20261015), B256::repeat_byte(7));
            let mut record = StoredGem::with_key(id);
            record.owner = Address::repeat_byte(1);
            record.promis_load_minor = U256::from(123);
            assert!(contract.records.get(id).unwrap().is_none());
            contract.records.create(&record).unwrap();
            assert_eq!(contract.records.get(id).unwrap(), Some(record.clone()));
            assert!(contract.records.create(&record).is_err());
            record.promis_load_minor = U256::from(456);
            contract.records.update(&record).unwrap();
            assert_eq!(contract.records.get(id).unwrap(), Some(record.clone()));
            contract.records.delete(id).unwrap();
            assert!(contract.records.get(id).unwrap().is_none());
            assert!(contract.records.update(&record).is_err());
        });
    }

    #[test]
    fn zero_price_bucket_exists_until_deleted() {
        let mut provider = HashMapStorageProvider::new(54322345);
        StorageHandle::enter(&mut provider, |storage| {
            let contract = crate::GemContract::new(storage);
            let key = B256::repeat_byte(2);
            let mut record = StoredGemBucket::with_key(key);
            record.present = true;
            record.worldwide_day = WorldwideDay::new(20261015);
            contract.buckets.create(&record).unwrap();
            assert_eq!(contract.buckets.get(key).unwrap(), Some(record));
            contract.buckets.delete(key).unwrap();
            assert!(contract.buckets.get(key).unwrap().is_none());
        });
    }
}
