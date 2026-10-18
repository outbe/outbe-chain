use alloy_primitives::Address;
use outbe_primitives::wwd_entity_id::WwdEntityId;

use crate::GemItemState;
use outbe_primitives::storage::indexed_list::{create_indexed, delete_indexed, IndexedList};

impl<'storage> crate::GemContract<'storage> {
    pub fn state_records(
        &self,
        owner: Address,
    ) -> outbe_primitives::error::Result<Vec<GemItemState>> {
        let mut ids = self.owner_ids.list(&owner).read_all()?;
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
                if record.owner != owner {
                    return Err(outbe_primitives::error::PrecompileError::fatal(
                        "gem owner index does not match its record",
                    ));
                }
                Ok(record)
            })
            .collect()
    }

    fn record_indexes(&self, record: &GemItemState) -> [IndexedList<'storage, WwdEntityId>; 1] {
        [IndexedList::new(
            self.owner_ids.list(&record.owner),
            self.owner_id_position.clone(),
        )]
    }

    pub fn insert_state_record(
        &self,
        record: &GemItemState,
    ) -> outbe_primitives::error::Result<()> {
        create_indexed(&self.records, record, self.record_indexes(record))
    }

    pub fn delete_state_record(&self, id: WwdEntityId) -> outbe_primitives::error::Result<()> {
        delete_indexed(&self.records, id, |record| self.record_indexes(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{B256, U256};
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use outbe_primitives::time::WorldwideDay;

    #[test]
    fn indexed_record_creation_reverts_if_an_index_rejects_it() {
        let mut provider = HashMapStorageProvider::new(54322345);
        StorageHandle::enter(&mut provider, |storage| {
            let contract = crate::GemContract::new(storage);
            let id =
                WwdEntityId::from_day_and_digest(WorldwideDay::new(20261015), B256::repeat_byte(7));
            let mut record = GemItemState::with_key(id);
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
            let mut record = GemItemState::with_key(id);
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
            let mut record = crate::GemBucketState::with_key(key);
            record.worldwide_day = WorldwideDay::new(20261015);
            contract.buckets.create(&record).unwrap();
            assert_eq!(contract.buckets.get(key).unwrap(), Some(record));
            contract.buckets.delete(key).unwrap();
            assert!(contract.buckets.get(key).unwrap().is_none());
        });
    }
}
