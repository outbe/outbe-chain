use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
    time::WorldwideDay,
    tribute_encryption::{
        EncryptedTributeV2, TRIBUTE_AMOUNTS_BLOB_LEN, TRIBUTE_PUBLIC_KEY_BLOB_LEN,
    },
    wwd_entity_id::{derive_poseidon_entity_id, WwdEntityId},
};

use crate::{
    errors::TributeError,
    precompile::ITribute,
    storage_records::{StoredTribute, StoredTributeEntryExt},
    TributeContract, TributeRecord,
};

pub fn get_record(storage: &StorageHandle<'_>, id: WwdEntityId) -> Result<Option<TributeRecord>> {
    let contract = TributeContract::new(storage.clone());
    if contract.retired_days.read(&id.worldwide_day())? {
        return Ok(None);
    }
    get_retained_record(storage, id)
}

pub fn get_retained_record(
    storage: &StorageHandle<'_>,
    id: WwdEntityId,
) -> Result<Option<TributeRecord>> {
    let contract = TributeContract::new(storage.clone());
    let entry = contract.records.entry(id);
    if !entry.exists()? {
        return Ok(None);
    }
    if !entry
        .encrypted_creator_public_key()
        .has_exact_length(TRIBUTE_PUBLIC_KEY_BLOB_LEN)?
        || !entry
            .encrypted_amounts()
            .has_exact_length(TRIBUTE_AMOUNTS_BLOB_LEN)?
    {
        return Err(PrecompileError::fatal(
            "invalid stored Tribute ciphertext length",
        ));
    }
    let stored = entry.load()?.ok_or(TributeError::TributeNotFound)?;
    let body: EncryptedTributeV2 = stored.into();
    if !body.has_valid_encoding() || body.context.chain_id != storage.chain_id()? {
        return Err(TributeError::InvalidEncryptedEncoding.into());
    }
    Ok(Some(crate::record::from_encrypted(body)))
}

pub fn issue_encrypted(storage: &StorageHandle<'_>, body: &EncryptedTributeV2) -> Result<()> {
    issue_encrypted_with_league(storage, body, None)
}

pub fn issue_encrypted_with_league(
    storage: &StorageHandle<'_>,
    body: &EncryptedTributeV2,
    explicit_league: Option<u16>,
) -> Result<()> {
    if !body.has_valid_encoding() {
        return Err(TributeError::InvalidEncryptedEncoding.into());
    }
    if body.context.chain_id != storage.chain_id()? {
        return Err(TributeError::ChainIdentityMismatch.into());
    }
    if let Some(l) = explicit_league {
        if !(1..=4096).contains(&l) {
            return Err(TributeError::InvalidLeague(l).into());
        }
    }
    let record = crate::record::from_encrypted(body.clone());
    let expected = derive_poseidon_entity_id(record.owner, record.worldwide_day)
        .map_err(TributeError::entity_id_derivation)?;
    if record.owner.is_zero() || record.tribute_id != expected {
        return Err(TributeError::InvalidOwner.into());
    }
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(storage)?;
    let mut contract = TributeContract::new(storage.clone());
    storage.with_checkpoint(|| {
        contract.ensure_day_accepts_tributes(record.worldwide_day)?;
        if contract.retired_days.read(&record.worldwide_day)?
            || contract.records.exists(record.tribute_id)?
        {
            return Err(TributeError::TributeAlreadyExists.into());
        }
        let league = match explicit_league {
            Some(l) => l,
            None => {
                let timestamp = storage.now_secs()?;
                outbe_fidelity::api::league_for_wwd(
                    storage.clone(),
                    record.owner,
                    record.worldwide_day,
                    timestamp,
                )?
            }
        };
        let nominal_amount_minor = record
            .calculation_view()
            .map_err(TributeError::private_amount_read_failed)?
            .nominal_amount_minor;
        contract.bump_day_bucket_record(&record, true)?;
        contract.tribute_leagues.write(&record.tribute_id, league)?;
        contract.adjust_fi_group(record.worldwide_day, league, nominal_amount_minor, true)?;
        let supply = contract
            .total_supply
            .read()?
            .checked_add(1)
            .ok_or(TributeError::TotalSupplyOverflowDuringIssuance)?;
        contract.total_supply.write(supply)?;
        contract.insert_state_record(&StoredTribute::from(body))?;
        contract.emit(ITribute::TributeIssued {
            owner: record.owner,
            tributeId: record.tribute_id.to_u256(),
            worldwideDay: record.worldwide_day.into(),
            issuanceAmountMinor: body.encrypted_amounts.clone().into(),
            settlementCurrency: record.issuance_currency,
            nominalAmountMinor: body.encrypted_amounts.clone().into(),
        })
    })
}

/// A Tribute exactly as stored when it was loaded; the burn re-checks storage against it.
/// Its record always carries the encrypted body, checked by `load`.
pub struct LoadedTribute {
    record: TributeRecord,
}
impl LoadedTribute {
    pub const fn record(&self) -> &TributeRecord {
        &self.record
    }
}

pub fn load(storage: &StorageHandle<'_>, id: WwdEntityId) -> Result<Option<LoadedTribute>> {
    get_record(storage, id)?
        .map(|record| {
            record
                .encrypted()
                .ok_or(TributeError::InvalidEncryptedEncoding)?;
            Ok(LoadedTribute { record })
        })
        .transpose()
}

pub fn burn_loaded(storage: &StorageHandle<'_>, loaded: LoadedTribute) -> Result<()> {
    let LoadedTribute { record } = loaded;
    let current = StoredTribute::from(
        record
            .encrypted()
            .ok_or(TributeError::InvalidEncryptedEncoding)?,
    );
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(storage)?;
    let mut contract = TributeContract::new(storage.clone());
    storage.with_checkpoint(|| {
        contract.ensure_day_accepts_tributes(record.worldwide_day)?;
        if contract.retained_state_record(record.tribute_id)?.as_ref() != Some(&current) {
            return Err(PrecompileError::revert(
                "Tribute state changed after loading",
            ));
        }
        contract.bump_day_bucket_record(&record, false)?;
        let league = contract.tribute_leagues.read(&record.tribute_id)?;
        if league != 0 {
            let nominal = record
                .calculation_view()
                .map_err(TributeError::private_amount_read_failed)?
                .nominal_amount_minor;
            contract.adjust_fi_group(record.worldwide_day, league, nominal, false)?;
            contract.tribute_leagues.clear(&record.tribute_id)?;
        }
        let supply = contract
            .total_supply
            .read()?
            .checked_sub(1)
            .ok_or(TributeError::TotalSupplyUnderflowDuringBurn)?;
        contract.total_supply.write(supply)?;
        contract.delete_loaded_state_record(&record)?;
        contract.emit(ITribute::TributeBurned {
            tributeId: record.tribute_id.to_u256(),
            owner: record.owner,
            worldwideDay: record.worldwide_day.into(),
        })
    })
}

pub fn retire_completed_day(storage: &StorageHandle<'_>, day: WorldwideDay) -> Result<bool> {
    let mut contract = TributeContract::new(storage.clone());
    storage.with_checkpoint(|| {
        if contract.retired_days.read(&day)? {
            return Ok(false);
        }
        let totals = contract
            .day_totals
            .get(day)?
            .ok_or(TributeError::WorldwideDayNotCompletedAndEmpty)?;
        if !totals.initialized
            || !totals.is_sealed
            || totals.tribute_count != 0
            || !contract.day_nominal_amount(day, false)?.is_zero()
        {
            return Err(TributeError::WorldwideDayNotCompletedAndEmpty.into());
        }
        contract.retired_days.write(&day, true)?;
        contract.emit(ITribute::TributePartitionRetired {
            worldwideDay: day.into(),
        })?;
        Ok(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, U256};
    use outbe_primitives::{
        storage::hashmap::HashMapStorageProvider,
        tribute_encryption::{TributeAmountsV2, TributeContextV2},
    };

    #[test]
    fn state_tribute_rejects_invalid_lengths_before_loading_payloads() {
        use outbe_primitives::storage::types::StorageKey;
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let contract = TributeContract::new(storage.clone());
            let day = WorldwideDay::new(20261015);
            let id = WwdEntityId::from_day_and_digest(day, B256::repeat_byte(7));
            let mut record = StoredTribute::with_key(id);
            record.owner = Address::repeat_byte(1);
            record.chain_id = 1;
            record.worldwide_day = day;
            record.encrypted_creator_public_key = vec![0; TRIBUTE_PUBLIC_KEY_BLOB_LEN];
            record.encrypted_creator_public_key[..8].copy_from_slice(&1u64.to_be_bytes());
            record.encrypted_amounts = vec![0; TRIBUTE_AMOUNTS_BLOB_LEN];
            record.encrypted_amounts[..8].copy_from_slice(&1u64.to_be_bytes());
            assert!(get_retained_record(&storage, id).unwrap().is_none());
            contract.records.create(&record).unwrap();
            assert!(get_retained_record(&storage, id).unwrap().is_some());
            for (offset, expected) in [
                (7, TRIBUTE_PUBLIC_KEY_BLOB_LEN),
                (8, TRIBUTE_AMOUNTS_BLOB_LEN),
            ] {
                let entry = contract.records.entry(id);
                let field = if offset == 7 {
                    entry.encrypted_creator_public_key()
                } else {
                    entry.encrypted_amounts()
                };
                for length in [expected - 1, expected + 1] {
                    field.write(&vec![0; length]).unwrap();
                    assert!(get_retained_record(&storage, id).is_err());
                }
                let slot = id.mapping_slot(entry.base_slot() + U256::from(offset));
                for raw in [U256::MAX, U256::from(expected * 2)] {
                    storage.sstore(entry.address(), slot, raw).unwrap();
                    assert!(get_retained_record(&storage, id).is_err());
                }
                storage
                    .sstore(entry.address(), slot, U256::from(expected * 2 + 1))
                    .unwrap();
                let mut blob = vec![0; expected];
                blob[..8].copy_from_slice(&1u64.to_be_bytes());
                field.write(&blob).unwrap();
            }
            assert!(get_retained_record(&storage, id).unwrap().is_some());
        });
    }

    #[test]
    fn encrypted_tribute_issues_and_burns_without_ce_storage() {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let day = WorldwideDay::new(20261015);
            let owner = Address::repeat_byte(1);
            let id = derive_poseidon_entity_id(owner, day).unwrap();
            let mut totals = crate::day_schema::StoredDayTotals::with_key(day);
            totals.initialized = true;
            let contract = TributeContract::new(storage.clone());
            contract.day_totals.create(&totals).unwrap();
            let amounts = TributeAmountsV2 {
                issuance_amount_minor: U256::from(123_000_000),
                nominal_amount_minor: U256::from(100_000_000),
            };
            let body = outbe_tee_enclave::tribute_encryption::encrypt_tribute(
                &crate::enclave_client::test_enclave::NETWORK_SECRET,
                &[7; 32],
                TributeContextV2 {
                    chain_id: 1,
                    tribute_id: id,
                    owner,
                    worldwide_day: day,
                    issuance_currency: 840,
                    reference_currency: 840,
                    tribute_price_minor: U256::from(1_000_000),
                    offer_input_hash: B256::repeat_byte(9),
                },
                &amounts,
            )
            .unwrap();
            issue_encrypted(&storage, &body).unwrap();
            assert!(issue_encrypted(&storage, &body).is_err());
            assert_eq!(contract.total_supply().unwrap(), 1);
            assert_eq!(
                get_record(&storage, id).unwrap().unwrap().encrypted(),
                Some(&body)
            );
            assert_eq!(contract.state_records_by_owner(owner).unwrap().len(), 1);
            assert_eq!(contract.state_records_by_day(day).unwrap().len(), 1);
            assert_eq!(
                contract
                    .get_day_totals(day)
                    .unwrap()
                    .tribute_nominal_total_minor,
                amounts.nominal_amount_minor
            );
            burn_loaded(&storage, load(&storage, id).unwrap().unwrap()).unwrap();
            assert!(get_record(&storage, id).unwrap().is_none());
            assert_eq!(contract.total_supply().unwrap(), 0);
            assert!(contract.state_records_by_owner(owner).unwrap().is_empty());
        });
    }

    #[test]
    fn retired_tribute_day_hides_live_records_and_preserves_ciphertext() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let day = WorldwideDay::new(20261015);
            let id = WwdEntityId::from_day_and_digest(day, B256::repeat_byte(7));
            let contract = TributeContract::new(storage);
            let mut record = StoredTribute::with_key(id);
            record.owner = Address::repeat_byte(1);
            record.worldwide_day = day;
            record.encrypted_amounts = vec![0x5a; 88];
            contract.insert_state_record(&record).unwrap();
            contract.retired_days.write(&day, true).unwrap();
            assert!(contract.state_record(id).unwrap().is_none());
            assert!(contract
                .state_records_by_owner(record.owner)
                .unwrap()
                .is_empty());
            assert!(contract.state_records_by_day(day).unwrap().is_empty());
            assert_eq!(contract.records.get(id).unwrap(), Some(record));
            assert_eq!(contract.day_ids.list(&day).len().unwrap(), 1);
        });
    }
}
