use alloy_primitives::{Address, B256, U256};
use outbe_macros::storage_record;
use outbe_primitives::{time::WorldwideDay, wwd_entity_id::WwdEntityId};

#[derive(Clone, Debug, Eq, PartialEq)]
#[storage_record(exists_field = owner)]
pub struct StoredTribute {
    #[key]
    pub tribute_id: WwdEntityId,
    #[attribute(order = 0)]
    pub owner: Address,
    #[attribute(order = 1)]
    pub chain_id: u64,
    #[attribute(order = 2)]
    pub worldwide_day: WorldwideDay,
    #[attribute(order = 3)]
    pub issuance_currency: u16,
    #[attribute(order = 4)]
    pub reference_currency: u16,
    #[attribute(order = 5)]
    pub tribute_price_minor: U256,
    #[attribute(order = 6)]
    pub offer_input_hash: B256,
    #[attribute(order = 7)]
    pub encrypted_creator_public_key: Vec<u8>,
    #[attribute(order = 8)]
    pub encrypted_amounts: Vec<u8>,
}

impl From<&outbe_primitives::tribute_encryption::EncryptedTributeV2> for StoredTribute {
    fn from(body: &outbe_primitives::tribute_encryption::EncryptedTributeV2) -> Self {
        let context = &body.context;
        Self {
            tribute_id: context.tribute_id,
            owner: context.owner,
            chain_id: context.chain_id,
            worldwide_day: context.worldwide_day,
            issuance_currency: context.issuance_currency,
            reference_currency: context.reference_currency,
            tribute_price_minor: context.tribute_price_minor,
            offer_input_hash: context.offer_input_hash,
            encrypted_creator_public_key: body.encrypted_creator_public_key.clone(),
            encrypted_amounts: body.encrypted_amounts.clone(),
        }
    }
}

impl From<StoredTribute> for outbe_primitives::tribute_encryption::EncryptedTributeV2 {
    fn from(body: StoredTribute) -> Self {
        Self {
            context: outbe_primitives::tribute_encryption::TributeContextV2 {
                chain_id: body.chain_id,
                tribute_id: body.tribute_id,
                owner: body.owner,
                worldwide_day: body.worldwide_day,
                issuance_currency: body.issuance_currency,
                reference_currency: body.reference_currency,
                tribute_price_minor: body.tribute_price_minor,
                offer_input_hash: body.offer_input_hash,
            },
            encrypted_creator_public_key: body.encrypted_creator_public_key,
            encrypted_amounts: body.encrypted_amounts,
        }
    }
}

impl crate::TributeContract<'_> {
    pub fn state_record(
        &self,
        id: WwdEntityId,
    ) -> outbe_primitives::error::Result<Option<StoredTribute>> {
        if self.retired_days.read(&id.worldwide_day())? {
            return Ok(None);
        }
        self.retained_state_record(id)
    }

    pub(crate) fn retained_state_record(
        &self,
        id: WwdEntityId,
    ) -> outbe_primitives::error::Result<Option<StoredTribute>> {
        crate::state_api::get_retained_record(&self.records.storage(), id)?
            .map(|record| {
                record
                    .encrypted()
                    .map(StoredTribute::from)
                    .ok_or_else(|| crate::errors::TributeError::InvalidEncryptedEncoding.into())
            })
            .transpose()
    }

    pub fn state_records_by_owner(
        &self,
        owner: Address,
    ) -> outbe_primitives::error::Result<Vec<StoredTribute>> {
        let mut ids = self.owner_ids.list(&owner).read_all()?;
        ids.sort_unstable();
        let mut records = Vec::new();
        if ids.windows(2).any(|ids| ids[0] == ids[1]) {
            return Err(outbe_primitives::error::PrecompileError::fatal(
                "Tribute owner index repeats an identity",
            ));
        }
        for id in ids {
            if self.retired_days.read(&id.worldwide_day())? {
                continue;
            }
            let record = self.retained_state_record(id)?.ok_or_else(|| {
                outbe_primitives::error::PrecompileError::fatal(
                    "Tribute owner index selects a missing record",
                )
            })?;
            if record.owner != owner || record.worldwide_day != id.worldwide_day() {
                return Err(outbe_primitives::error::PrecompileError::fatal(
                    "Tribute owner index does not match its record",
                ));
            }
            records.push(record);
        }
        Ok(records)
    }

    pub fn state_records_by_day(
        &self,
        day: WorldwideDay,
    ) -> outbe_primitives::error::Result<Vec<StoredTribute>> {
        if self.retired_days.read(&day)? {
            return Ok(Vec::new());
        }
        let mut ids = self.day_ids.list(&day).read_all()?;
        ids.sort_unstable();
        if ids.windows(2).any(|ids| ids[0] == ids[1]) {
            return Err(outbe_primitives::error::PrecompileError::fatal(
                "Tribute day index repeats an identity",
            ));
        }
        ids.into_iter()
            .map(|id| {
                let record = self.retained_state_record(id)?.ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::fatal(
                        "Tribute day index selects a missing record",
                    )
                })?;
                if record.worldwide_day != day || id.worldwide_day() != day {
                    return Err(outbe_primitives::error::PrecompileError::fatal(
                        "Tribute day index does not match its record",
                    ));
                }
                Ok(record)
            })
            .collect()
    }

    pub fn insert_state_record(
        &self,
        record: &StoredTribute,
    ) -> outbe_primitives::error::Result<()> {
        use outbe_primitives::storage::indexed_list::IndexedList;
        let storage = self.records.storage();
        storage.with_checkpoint(|| {
            self.records.create(record)?;
            IndexedList::new(
                &self.owner_ids.list(&record.owner),
                self.owner_id_position.clone(),
            )
            .insert(record.tribute_id)?;
            IndexedList::new(
                &self.day_ids.list(&record.worldwide_day),
                self.day_id_position.clone(),
            )
            .insert(record.tribute_id)?;
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
            IndexedList::new(
                &self.day_ids.list(&record.worldwide_day),
                self.day_id_position.clone(),
            )
            .remove(id)?;
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
            let contract = crate::TributeContract::new(storage);
            let id =
                WwdEntityId::from_day_and_digest(WorldwideDay::new(20261015), B256::repeat_byte(7));
            let mut record = StoredTribute::with_key(id);
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
            let contract = crate::TributeContract::new(storage);
            let id =
                WwdEntityId::from_day_and_digest(WorldwideDay::new(20261015), B256::repeat_byte(7));
            let mut record = StoredTribute::with_key(id);
            record.owner = Address::repeat_byte(1);
            record.chain_id = 54322345;
            record.worldwide_day = id.worldwide_day();
            record.encrypted_creator_public_key = vec![0xa5; 56];
            record.encrypted_amounts = vec![0x5a; 88];
            assert!(contract.records.get(id).unwrap().is_none());
            contract.records.create(&record).unwrap();
            assert_eq!(contract.records.get(id).unwrap(), Some(record.clone()));
            assert!(contract.records.create(&record).is_err());
            record.encrypted_amounts = vec![0x33; 1];
            contract.records.update(&record).unwrap();
            assert_eq!(contract.records.get(id).unwrap(), Some(record.clone()));
            contract.records.delete(id).unwrap();
            assert!(contract.records.get(id).unwrap().is_none());
            assert!(contract
                .records
                .entry(id)
                .encrypted_amounts()
                .read()
                .unwrap()
                .is_empty());
            assert!(contract.records.update(&record).is_err());
        });
    }
}
