use crate::errors::{date_step_error, NodError};
use crate::precompile::INod;
use crate::schema::{CallTerms, NodContract, NodGroupState, NodItemState};
use crate::storage_records::{StoredNod, StoredNodEntryExt};
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    error::{PrecompileError, Result},
    nod_encryption::NOD_BLOB_LEN,
    storage::StorageHandle,
};

/// A Nod item exactly as stored when it was loaded; mutation re-checks storage against it.
/// `StoredNod -> NodItemState -> StoredNod` is lossless, so the stored record is not kept twice.
pub struct LoadedNodItem {
    body: NodItemState,
}
impl LoadedNodItem {
    pub const fn body(&self) -> &NodItemState {
        &self.body
    }
    pub(crate) fn into_body(self) -> NodItemState {
        self.body
    }
}

/// A Nod group exactly as stored when it was loaded; mutation re-checks storage against it.
pub struct LoadedNodGroup {
    body: NodGroupState,
}
impl LoadedNodGroup {
    pub const fn body(&self) -> &NodGroupState {
        &self.body
    }
    pub(crate) fn into_body(self) -> NodGroupState {
        self.body
    }
}

pub fn load_item(storage: &StorageHandle<'_>, id: U256) -> Result<Option<LoadedNodItem>> {
    let contract = NodContract::new(storage.clone());
    let entry = contract.records.entry(id);
    if !entry.exists()? {
        return Ok(None);
    }
    if !entry
        .encrypted_gratis_amount()
        .has_exact_length(NOD_BLOB_LEN)?
    {
        return Err(PrecompileError::fatal(
            "invalid stored Nod ciphertext length",
        ));
    }
    let body: NodItemState = entry.load()?.ok_or(NodError::NodNotFound)?.into();
    if !body.encrypted.has_valid_encoding() {
        return Err(NodError::EncryptedTermsMismatch.into());
    }
    Ok(Some(LoadedNodItem { body }))
}
pub fn load_group(storage: &StorageHandle<'_>, key: B256) -> Result<Option<LoadedNodGroup>> {
    let contract = NodContract::new(storage.clone());
    contract
        .groups
        .get(key)?
        .map(|body| Ok(LoadedNodGroup { body }))
        .transpose()
}
pub fn add_item(
    storage: &StorageHandle<'_>,
    item: &NodItemState,
    terms: Option<CallTerms>,
    gratis_load_minor: U256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        NodContract::new(storage.clone()).record_nod_issued_state(
            item,
            terms,
            gratis_load_minor,
            None,
        )
    })
}
pub fn add_series_item(
    storage: &StorageHandle<'_>,
    item: &NodItemState,
    terms: Option<CallTerms>,
    gratis_load_minor: U256,
    series_group_key: B256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        NodContract::new(storage.clone()).record_nod_issued_state(
            item,
            terms,
            gratis_load_minor,
            Some(series_group_key),
        )
    })
}
pub fn clean_item(storage: &StorageHandle<'_>, nod_id: U256) -> Result<()> {
    storage.with_checkpoint(|| {
        let item = load_item(storage, nod_id)?.ok_or(NodError::NodNotFound)?;
        if item.body().is_settled {
            return Err(NodError::NodAlreadySettled.into());
        }
        let group_key = item.body().group_key;
        let contract = NodContract::new(storage.clone());
        if !contract.group_is_forfeited.read(&group_key)? {
            return Err(NodError::NodNotForfeited.into());
        }
        let group = load_group(storage, group_key)?.ok_or(NodError::GroupNotFound)?;
        NodContract::new(storage.clone()).record_nod_removed_state(item, group)
    })
}
pub fn record_settled(
    storage: &StorageHandle<'_>,
    item: LoadedNodItem,
    group: LoadedNodGroup,
) -> Result<()> {
    storage
        .with_checkpoint(|| NodContract::new(storage.clone()).record_nod_settled_state(item, group))
}
pub fn remove_item(
    storage: &StorageHandle<'_>,
    item: LoadedNodItem,
    group: LoadedNodGroup,
) -> Result<()> {
    storage
        .with_checkpoint(|| NodContract::new(storage.clone()).record_nod_removed_state(item, group))
}
/// Must run before `item` or `group` is mutated: both are compared with storage as loaded.
fn check_loaded(
    contract: &NodContract<'_>,
    item: &NodItemState,
    group: &NodGroupState,
) -> Result<()> {
    if item.group_key != group.group_key
        || contract.records.get(item.nod_id)? != Some(StoredNod::from(item))
        || contract.groups.get(group.group_key)?.as_ref() != Some(group)
    {
        return Err(NodError::NodStateChangedAfterLoading.into());
    }
    Ok(())
}
impl NodContract<'_> {
    fn record_nod_issued_state(
        &mut self,
        item: &NodItemState,
        call_terms: Option<CallTerms>,
        gratis_load_minor: U256,
        series_group_key: Option<B256>,
    ) -> Result<()> {
        crate::issuance::validate_item(
            item,
            call_terms.map_or(U256::ZERO, |terms| terms.call_price_minor),
            series_group_key,
        )?;
        if self.records.get(item.nod_id)?.is_some() {
            return Err(NodError::NodAlreadyExists.into());
        }

        let group_id = item.group_id().map_err(date_step_error)?;
        let current_group = self.groups.get(item.group_key)?;
        let member_count = self.group_nod_count.read(&item.group_key)?;
        let settled_count = current_group.as_ref().map_or(0, |group| group.settled_nods);
        let unclaimed_count = self.group_unclaimed_tribute_count.read(&item.group_key)?;
        if current_group.is_some() != (member_count > 0 || settled_count > 0 || unclaimed_count > 0)
        {
            return Err(NodError::GroupExistenceDisagreesWithMemberCount {
                group_id,
                member_count,
            }
            .into());
        }
        let new_group = match current_group {
            Some(_) => None,
            None => {
                let group = NodGroupState {
                    settled_nods: 0,
                    group_key: item.group_key,
                    full_day_after_issue: item.full_day_after_issue().map_err(date_step_error)?,
                    reference_currency: item.reference_currency,
                };
                if let Some(terms) = call_terms {
                    self.seal_group_call_terms(item.group_key, terms)?;
                    self.insert_call_bin(item.group_key)?;
                }
                Some(group)
            }
        };

        if series_group_key.is_some() {
            self.consume_unclaimed_tribute_from_series(item.group_key, gratis_load_minor)?;
        }
        self.add_unpaid_load(item.group_key, gratis_load_minor)?;
        self.insert_group_member(item.group_key, item.nod_id)?;
        self.insert_state_record(&StoredNod::from(item))?;
        if let Some(group) = new_group {
            self.groups.create(&group)?;
        }
        self.emit(INod::Transfer {
            from: Address::ZERO,
            to: item.owner,
            tokenId: item.nod_id,
        })
    }

    fn record_nod_settled_state(
        &mut self,
        item: LoadedNodItem,
        group: LoadedNodGroup,
    ) -> Result<()> {
        let mut item = item.into_body();
        let mut group = group.into_body();
        check_loaded(self, &item, &group)?;
        if item.is_settled {
            return Err(NodError::AlreadySettled.into());
        }
        group.settled_nods = group
            .settled_nods
            .checked_add(1)
            .ok_or(NodError::SettledCountOverflow)?;
        self.remove_group_member(item.group_key, item.nod_id)?;
        self.take_unpaid_load(item.group_key, crate::api::calculation_amount(&item)?)?;
        item.is_settled = true;
        self.records.update(&StoredNod::from(&item))?;
        self.groups.update(&group)?;
        self.emit(INod::MetadataUpdate {
            _tokenId: item.nod_id,
        })
    }

    fn record_nod_removed_state(
        &mut self,
        item: LoadedNodItem,
        group: LoadedNodGroup,
    ) -> Result<()> {
        let item = item.into_body();
        let mut group = group.into_body();
        check_loaded(self, &item, &group)?;
        let group_id = item.group_id().map_err(date_step_error)?;
        let remaining = if item.is_settled {
            group.settled_nods = group
                .settled_nods
                .checked_sub(1)
                .ok_or(NodError::GroupSettledCountUnderflow { group_id })?;
            self.group_nod_count.read(&item.group_key)?
        } else {
            // An unpaid Nod leaves only through its forfeited group's cleanup, after the
            // group's whole unpaid load was returned. So its load stays on the total.
            self.remove_group_member(item.group_key, item.nod_id)?
        };
        self.delete_state_record(item.nod_id)?;
        let unclaimed_count = self.group_unclaimed_tribute_count.read(&item.group_key)?;
        if remaining == 0 && group.settled_nods == 0 && unclaimed_count == 0 {
            self.group_nod_count.clear(&item.group_key)?;
            self.close_group(item.group_key)?;
            self.groups.delete(group.group_key)?;
        } else if item.is_settled {
            self.groups.update(&group)?;
        }
        self.emit(INod::Transfer {
            from: item.owner,
            to: Address::ZERO,
            tokenId: item.nod_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_support, NodIssueParams};
    use outbe_primitives::{
        storage::hashmap::HashMapStorageProvider,
        time::{date_key_to_utc_timestamp, WorldwideDay},
    };

    fn stored_fixture() -> StoredNod {
        let day = WorldwideDay::new(20261015);
        let id = U256::from_be_bytes([7; 32]);
        let mut record = StoredNod::with_key(id);
        record.owner = Address::repeat_byte(1);
        record.chain_id = 1;
        record.worldwide_day = day;
        record.encrypted_gratis_amount = vec![0; NOD_BLOB_LEN];
        record.encrypted_gratis_amount[..8].copy_from_slice(&1u64.to_be_bytes());
        record
    }

    #[test]
    fn stored_nod_survives_the_item_state_round_trip_exactly() {
        let day = WorldwideDay::new(20261015);
        let stored = StoredNod {
            nod_id: U256::from_be_bytes([7; 32]),
            owner: Address::repeat_byte(1),
            chain_id: 2,
            worldwide_day: day,
            league_id: 3,
            entry_price_minor: U256::from(4),
            issuance_currency: 5,
            reference_currency: 6,
            group_key: B256::repeat_byte(8),
            issued_at: 9,
            is_settled: true,
            encryption_binding: B256::repeat_byte(10),
            encrypted_gratis_amount: vec![12; NOD_BLOB_LEN],
        };
        let item = NodItemState::from(stored.clone());
        assert_eq!(StoredNod::from(&item), stored);
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
            let mut invalid = record.clone();
            invalid.encrypted_gratis_amount[0] = 2;
            contract.records.update(&invalid).unwrap();
            assert!(load_item(&storage, id).is_err());
            record.worldwide_day = WorldwideDay::new(20261016);
            contract.records.update(&record).unwrap();
            // Structural reads do not infer a day from an opaque identity.
            assert_eq!(
                load_item(&storage, id)
                    .unwrap()
                    .unwrap()
                    .body()
                    .worldwide_day,
                record.worldwide_day
            );
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
            let entry = contract.records.entry(id);
            let field = entry.encrypted_gratis_amount();
            for length in [55, 57] {
                field.write(&vec![0; length]).unwrap();
                assert!(load_item(&storage, id).is_err());
            }
            let slot = id.mapping_slot(entry.base_slot() + U256::from(11));
            for raw in [U256::MAX, U256::from(112)] {
                storage.sstore(entry.address(), slot, raw).unwrap();
                assert!(load_item(&storage, id).is_err());
            }
            storage
                .sstore(entry.address(), slot, U256::from(113))
                .unwrap();
            field.write(&record.encrypted_gratis_amount).unwrap();
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
                group_key: crate::identity::group_key(
                    date_key_to_utc_timestamp(20_200_101),
                    U256::ZERO,
                    params.reference_currency,
                )
                .expect("date step"),
                issuance_currency: params.issuance_currency,
                reference_currency: params.reference_currency,
                issued_at: date_key_to_utc_timestamp(20_200_101),
                is_settled: false,
                encrypted,
            };
            add_item(&storage, &item, None, params.gratis_load_minor).unwrap();
            let contract = NodContract::new(storage.clone());
            assert_eq!(contract.state_records(item.owner).unwrap().len(), 1);
            let stale_item = load_item(&storage, item.nod_id).unwrap().unwrap();
            let stale_group = load_group(&storage, item.group_key).unwrap().unwrap();
            record_settled(
                &storage,
                load_item(&storage, item.nod_id).unwrap().unwrap(),
                load_group(&storage, item.group_key).unwrap().unwrap(),
            )
            .unwrap();
            assert!(record_settled(&storage, stale_item, stale_group).is_err());
            assert_eq!(
                contract
                    .groups
                    .get(item.group_key)
                    .unwrap()
                    .unwrap()
                    .settled_nods,
                1
            );
            assert_eq!(contract.group_nod_count.read(&item.group_key).unwrap(), 0);
            remove_item(
                &storage,
                load_item(&storage, item.nod_id).unwrap().unwrap(),
                load_group(&storage, item.group_key).unwrap().unwrap(),
            )
            .unwrap();
            assert!(contract.state_records(item.owner).unwrap().is_empty());
            assert!(contract.groups.get(item.group_key).unwrap().is_none());
        });
    }
}
