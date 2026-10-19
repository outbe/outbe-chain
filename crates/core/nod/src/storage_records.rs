use alloy_primitives::{Address, B256, U256};
use outbe_macros::storage_record;
use outbe_primitives::storage::indexed_list::{create_indexed, delete_indexed, IndexedList};
use outbe_primitives::{time::WorldwideDay, wwd_entity_id::WwdEntityId};

#[derive(Clone, Debug, Eq, PartialEq)]
#[storage_record(exists_field = owner)]
pub struct StoredNod {
    #[key]
    pub nod_id: WwdEntityId,
    #[attribute(order = 0)]
    pub owner: Address,
    #[attribute(order = 1)]
    pub chain_id: u64,
    #[attribute(order = 2)]
    pub worldwide_day: WorldwideDay,
    #[attribute(order = 3)]
    pub league_id: u16,
    #[attribute(order = 4)]
    pub entry_price_minor: U256,
    #[attribute(order = 5)]
    pub issuance_currency: u16,
    #[attribute(order = 6)]
    pub reference_currency: u16,
    #[attribute(order = 7)]
    pub group_key: B256,
    #[attribute(order = 8)]
    pub issued_at: u64,
    #[attribute(order = 9)]
    pub is_settled: bool,
    #[attribute(order = 10)]
    pub encryption_binding: B256,
    #[attribute(order = 11)]
    pub encrypted_creator_public_key: Vec<u8>,
    #[attribute(order = 12)]
    pub encrypted_gratis_amount: Vec<u8>,
}

impl From<&crate::NodItemState> for StoredNod {
    fn from(item: &crate::NodItemState) -> Self {
        Self {
            nod_id: item.nod_id,
            owner: item.owner,
            chain_id: item.encrypted.terms.chain_id,
            worldwide_day: item.worldwide_day,
            league_id: item.league_id,
            entry_price_minor: item.encrypted.terms.entry_price_minor,
            issuance_currency: item.issuance_currency,
            reference_currency: item.reference_currency,
            group_key: item.group_key,
            issued_at: item.issued_at,
            is_settled: item.is_settled,
            encryption_binding: item.encrypted.encryption_binding,
            encrypted_creator_public_key: item.encrypted.encrypted_creator_public_key.clone(),
            encrypted_gratis_amount: item.encrypted.encrypted_gratis_amount.clone(),
        }
    }
}

impl From<StoredNod> for crate::NodItemState {
    fn from(item: StoredNod) -> Self {
        Self {
            nod_id: item.nod_id,
            owner: item.owner,
            worldwide_day: item.worldwide_day,
            league_id: item.league_id,
            group_key: item.group_key,
            issuance_currency: item.issuance_currency,
            reference_currency: item.reference_currency,
            issued_at: item.issued_at,
            is_settled: item.is_settled,
            encrypted: outbe_primitives::nod_encryption::EncryptedNodV2 {
                terms: outbe_primitives::nod_encryption::NodTermsV2 {
                    chain_id: item.chain_id,
                    nod_id: item.nod_id,
                    owner: item.owner,
                    worldwide_day: item.worldwide_day,
                    league_id: item.league_id,
                    entry_price_minor: item.entry_price_minor,
                    issuance_currency: item.issuance_currency,
                    reference_currency: item.reference_currency,
                },
                encryption_binding: item.encryption_binding,
                encrypted_creator_public_key: item.encrypted_creator_public_key,
                encrypted_gratis_amount: item.encrypted_gratis_amount,
            },
        }
    }
}

impl<'storage> crate::NodContract<'storage> {
    pub fn state_records(
        &self,
        owner: Option<Address>,
    ) -> outbe_primitives::error::Result<Vec<StoredNod>> {
        let mut ids = match owner {
            Some(owner) => self.owner_ids.list(&owner).read_all()?,
            None => self.all_ids.read_all()?,
        };
        ids.sort_unstable();
        if ids.windows(2).any(|ids| ids[0] == ids[1]) {
            return Err(outbe_primitives::error::PrecompileError::fatal(
                "nod index repeats an identity",
            ));
        }
        ids.into_iter()
            .map(|id| {
                let loaded =
                    crate::state_api::load_item(&self.records.storage(), id)?.ok_or_else(|| {
                        outbe_primitives::error::PrecompileError::fatal(
                            "nod index selects a missing record",
                        )
                    })?;
                let record = StoredNod::from(loaded.body());
                if owner.is_some_and(|owner| record.owner != owner) {
                    return Err(outbe_primitives::error::PrecompileError::fatal(
                        "nod owner index does not match its record",
                    ));
                }
                Ok(record)
            })
            .collect()
    }

    fn record_indexes(&self, record: &StoredNod) -> [IndexedList<'storage, WwdEntityId>; 2] {
        [
            IndexedList::new(
                self.owner_ids.list(&record.owner),
                self.owner_id_position.clone(),
            ),
            IndexedList::new(self.all_ids.clone(), self.global_id_position.clone()),
        ]
    }

    pub fn insert_state_record(&self, record: &StoredNod) -> outbe_primitives::error::Result<()> {
        create_indexed(&self.records, record, self.record_indexes(record))
    }

    pub fn delete_state_record(&self, id: WwdEntityId) -> outbe_primitives::error::Result<()> {
        delete_indexed(&self.records, id, |record| self.record_indexes(record))
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
            let contract = crate::NodContract::new(storage);
            let id =
                WwdEntityId::from_day_and_digest(WorldwideDay::new(20261015), B256::repeat_byte(7));
            let mut record = StoredNod::with_key(id);
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
            let contract = crate::NodContract::new(storage);
            let id =
                WwdEntityId::from_day_and_digest(WorldwideDay::new(20261015), B256::repeat_byte(7));
            let mut record = StoredNod::with_key(id);
            record.owner = Address::repeat_byte(1);
            record.chain_id = 54322345;
            record.worldwide_day = id.worldwide_day();
            record.encrypted_creator_public_key = vec![0xa5; 56];
            record.encrypted_gratis_amount = vec![0x5a; 88];
            assert!(contract.records.get(id).unwrap().is_none());
            contract.records.create(&record).unwrap();
            assert_eq!(contract.records.get(id).unwrap(), Some(record.clone()));
            assert!(contract.records.create(&record).is_err());
            record.encrypted_gratis_amount = vec![0x33; 1];
            contract.records.update(&record).unwrap();
            assert_eq!(contract.records.get(id).unwrap(), Some(record.clone()));
            contract.records.delete(id).unwrap();
            assert!(contract.records.get(id).unwrap().is_none());
            assert!(contract
                .records
                .entry(id)
                .encrypted_gratis_amount()
                .read()
                .unwrap()
                .is_empty());
            assert!(contract.records.update(&record).is_err());
        });
    }

    #[test]
    fn zero_price_group_exists_until_deleted() {
        let mut provider = HashMapStorageProvider::new(54322345);
        StorageHandle::enter(&mut provider, |storage| {
            let contract = crate::NodContract::new(storage);
            let key = B256::repeat_byte(2);
            let mut record = crate::schema::NodGroupState::with_key(key);
            record.worldwide_day = WorldwideDay::new(20261015);
            contract.groups.create(&record).unwrap();
            assert_eq!(contract.groups.get(key).unwrap(), Some(record));
            contract.groups.delete(key).unwrap();
            assert!(contract.groups.get(key).unwrap().is_none());
        });
    }
}
