use alloy_primitives::{Address, U256};

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

    fn record_indexes(&self, record: &GemItemState) -> [IndexedList<'storage, U256>; 1] {
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

    pub fn delete_state_record(&self, id: U256) -> outbe_primitives::error::Result<()> {
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
    fn legacy_wwd_storage_is_read_by_native_u256_records_and_owner_indexes() {
        use outbe_primitives::storage::dsl::{List, Map};
        use outbe_primitives::storage::types::{Mapping, Storable, StorageKey};
        use outbe_primitives::wwd_entity_id::WwdEntityId;

        let mut provider = HashMapStorageProvider::new(54322345);
        StorageHandle::enter(&mut provider, |storage| {
            let contract = crate::GemContract::new(storage.clone());
            let owner = Address::repeat_byte(1);
            let low = U256::from(7);
            let high = (U256::ONE << 255) | U256::from(19);
            let legacy_owners = Map::<Address, List<WwdEntityId>>::new(
                contract.owner_ids.base_slot(),
                contract.owner_ids.address(),
                storage.clone(),
            );
            let legacy_positions = Map::<WwdEntityId, u32>::new(
                contract.owner_id_position.base_slot(),
                contract.owner_id_position.address(),
                storage.clone(),
            );
            let legacy_records = Mapping::<WwdEntityId, Address>::new(
                contract.records.base_slot(),
                contract.records.address(),
                storage.clone(),
            );
            // Write the old record's existence field and indexes, not new U256 records.
            // Other record fields remain zero, as in GemItemState::with_key.
            for (id, position) in [(high, 1), (low, 2)] {
                let legacy = WwdEntityId::from(id.to_be_bytes::<32>());
                assert_eq!(legacy.to_word(), id.to_word());
                assert_eq!(legacy.key_bytes(), id.key_bytes());
                for base in [contract.records.base_slot(), legacy_positions.base_slot()] {
                    assert_eq!(legacy.mapping_slot(base), id.mapping_slot(base));
                }
                legacy_records.write(&legacy, owner).unwrap();
                legacy_owners.list(&owner).push(legacy).unwrap();
                legacy_positions.write(&legacy, position).unwrap();

                let mut expected = GemItemState::with_key(id);
                expected.owner = owner;
                assert_eq!(contract.records.get(id).unwrap(), Some(expected.clone()));
                assert_eq!(crate::api::get_item(&storage, id).unwrap(), Some(expected));
                assert_eq!(contract.owner_id_position.read(&id).unwrap(), position);
            }
            assert_eq!(
                contract.owner_ids.list(&owner).read_all().unwrap(),
                vec![high, low]
            );
            assert_eq!(crate::api::balance_of(&storage, owner).unwrap(), 2);
            assert_eq!(
                crate::api::token_of_owner_by_index(&storage, owner, 0).unwrap(),
                Some(high)
            );
            assert_eq!(
                crate::api::token_of_owner_by_index(&storage, owner, 1).unwrap(),
                Some(low)
            );
            assert_eq!(
                contract
                    .state_records(owner)
                    .unwrap()
                    .iter()
                    .map(|item| item.gem_id)
                    .collect::<Vec<_>>(),
                vec![low, high]
            );

            contract.delete_state_record(high).unwrap();
            assert!(contract.records.get(high).unwrap().is_none());
            assert_eq!(
                contract.owner_ids.list(&owner).read_all().unwrap(),
                vec![low]
            );
            assert_eq!(contract.owner_id_position.read(&low).unwrap(), 1);
            assert_eq!(legacy_positions.read(&WwdEntityId::from(high)).unwrap(), 0);
            assert_eq!(
                legacy_owners.list(&owner).read_all().unwrap(),
                vec![WwdEntityId::from(low)]
            );
        });
    }

    #[test]
    fn indexed_record_creation_reverts_if_an_index_rejects_it() {
        let mut provider = HashMapStorageProvider::new(54322345);
        StorageHandle::enter(&mut provider, |storage| {
            let contract = crate::GemContract::new(storage);
            let id = U256::from_be_bytes([7; 32]);
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
            let id = U256::from_be_bytes([7; 32]);
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
    fn zero_price_group_exists_until_deleted() {
        let mut provider = HashMapStorageProvider::new(54322345);
        StorageHandle::enter(&mut provider, |storage| {
            let contract = crate::GemContract::new(storage);
            let key = B256::repeat_byte(2);
            let mut record = crate::GemGroupState::with_key(key);
            record.full_day_after_issue = WorldwideDay::new(20261015);
            contract.groups.create(&record).unwrap();
            assert_eq!(contract.groups.get(key).unwrap(), Some(record));
            contract.groups.delete(key).unwrap();
            assert!(contract.groups.get(key).unwrap().is_none());
        });
    }
}
