use crate::errors::GemError;
use crate::precompile::IGem;
use crate::schema::{CallTerms, GemContract, GemGroupState, GemItemState};
use crate::GemState;
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
    wwd_entity_id::WwdEntityId,
};

/// A gem exactly as stored when it was loaded; mutation re-checks storage against it.
pub struct LoadedGem {
    body: GemItemState,
}
impl LoadedGem {
    pub const fn body(&self) -> &GemItemState {
        &self.body
    }
    pub(crate) fn into_body(self) -> GemItemState {
        self.body
    }
}

/// A gem group exactly as stored when it was loaded; mutation re-checks storage against it.
pub struct LoadedGemGroup {
    body: GemGroupState,
}
impl LoadedGemGroup {
    pub const fn body(&self) -> &GemGroupState {
        &self.body
    }
    pub(crate) fn into_body(self) -> GemGroupState {
        self.body
    }
}

pub fn load_item(storage: &StorageHandle<'_>, id: WwdEntityId) -> Result<Option<LoadedGem>> {
    let contract = GemContract::new(storage.clone());
    contract
        .records
        .get(id)?
        .map(|body| Ok(LoadedGem { body }))
        .transpose()
}
pub fn load_group(storage: &StorageHandle<'_>, key: B256) -> Result<Option<LoadedGemGroup>> {
    let contract = GemContract::new(storage.clone());
    contract
        .groups
        .get(key)?
        .map(|body| Ok(LoadedGemGroup { body }))
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
    group: LoadedGemGroup,
) -> Result<()> {
    storage
        .with_checkpoint(|| GemContract::new(storage.clone()).record_gem_settled_state(item, group))
}
pub fn remove_item(
    storage: &StorageHandle<'_>,
    item: LoadedGem,
    group: LoadedGemGroup,
) -> Result<()> {
    storage
        .with_checkpoint(|| GemContract::new(storage.clone()).record_gem_removed_state(item, group))
}
/// Must run before `item` or `group` is mutated: both are compared with storage as loaded.
fn check_loaded(
    contract: &GemContract<'_>,
    item: &GemItemState,
    group: &GemGroupState,
) -> Result<()> {
    if item.group_key != group.group_key
        || contract.records.get(item.gem_id)?.as_ref() != Some(item)
        || contract.groups.get(group.group_key)?.as_ref() != Some(group)
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
        if item.owner.is_zero() {
            return Err(GemError::ZeroOwner.into());
        }
        if self.records.get(item.gem_id)?.is_some() {
            return Err(GemError::AlreadyExists.into());
        }
        let group_key = item.group_key;
        let group_id = item.group_id();
        let current_group = self.groups.get(item.group_key)?;
        let member_count = self.group_gem_count.read(&group_key)?;
        let settled_count = current_group.as_ref().map_or(0, |group| group.settled_gems);
        if current_group.is_some() != (member_count > 0 || settled_count > 0) {
            return Err(GemError::GroupExistenceMismatch {
                group_id,
                member_count,
            }
            .into());
        }
        let new_group = match current_group {
            Some(_) => None,
            None => {
                self.group_issued_at.write(&group_key, item.issued_at)?;
                if let Some(terms) = call_terms {
                    self.seal_group_call_terms(group_key, terms)?;
                    self.insert_call_bin(group_key)?;
                }
                Some(GemGroupState {
                    group_key,
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
        self.insert_group_member(group_key, item.gem_id.to_u256())?;
        self.insert_state_record(item)?;
        if let Some(group) = new_group {
            self.groups.create(&group)?;
        }
        self.emit(IGem::Transfer {
            from: Address::ZERO,
            to: item.owner,
            tokenId: item.gem_id.to_u256(),
        })
    }

    fn record_gem_settled_state(&mut self, item: LoadedGem, group: LoadedGemGroup) -> Result<()> {
        let mut item = item.into_body();
        let mut group = group.into_body();
        check_loaded(self, &item, &group)?;
        if item.is_settled() {
            return Err(GemError::AlreadySettled.into());
        }
        group.settled_gems = group
            .settled_gems
            .checked_add(1)
            .ok_or(GemError::SettledCountOverflow)?;
        self.remove_group_member(item.group_key, item.gem_id.to_u256())?;
        item.called_at = self.group_called_at.read(&item.group_key)?;
        item.settled_at = self.storage.timestamp()?.to::<u64>();
        item.state = GemState::Settled as u8;
        self.records.update(&item)?;
        self.groups.update(&group)?;
        self.emit(IGem::MetadataUpdate {
            _tokenId: item.gem_id.to_u256(),
        })
    }

    fn record_gem_removed_state(&mut self, item: LoadedGem, group: LoadedGemGroup) -> Result<()> {
        let item = item.into_body();
        let mut group = group.into_body();
        check_loaded(self, &item, &group)?;
        let group_id = WwdEntityId::from_day_and_digest(group.worldwide_day, group.group_key);
        let supply = self
            .total_supply
            .read()?
            .checked_sub(1)
            .ok_or(GemError::TotalSupplyUnderflow)?;
        self.total_supply.write(supply)?;
        let remaining = if item.is_settled() {
            group.settled_gems = group
                .settled_gems
                .checked_sub(1)
                .ok_or(GemError::SettledCountUnderflow { group_id })?;
            self.group_gem_count.read(&item.group_key)?
        } else {
            self.remove_group_member(item.group_key, item.gem_id.to_u256())?
        };
        self.delete_state_record(item.gem_id)?;
        if remaining == 0 && group.settled_gems == 0 {
            self.close_group(item.group_key)?;
            self.groups.delete(group.group_key)?;
        } else if item.is_settled() {
            self.groups.update(&group)?;
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
                issuance_currency: 840,
                reference_currency: 840,
                issued_at: day.start_timestamp(),
                group_key: crate::identity::group_key(day, U256::from(1_000_000), 840),
                state: GemState::Issued as u8,
                called_at: 0,
                settled_at: 0,
            };
            add_item(&storage, &item, U256::from(1_000_000), None).unwrap();
            let contract = GemContract::new(storage.clone());
            assert_eq!(contract.total_supply.read().unwrap(), 1);
            let stale_item = load_item(&storage, item.gem_id).unwrap().unwrap();
            let stale_group = load_group(&storage, item.group_key).unwrap().unwrap();
            record_settled(
                &storage,
                load_item(&storage, item.gem_id).unwrap().unwrap(),
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
                    .settled_gems,
                1
            );
            assert_eq!(contract.group_gem_count.read(&item.group_key).unwrap(), 0);
            remove_item(
                &storage,
                load_item(&storage, item.gem_id).unwrap().unwrap(),
                load_group(&storage, item.group_key).unwrap().unwrap(),
            )
            .unwrap();
            assert_eq!(contract.total_supply.read().unwrap(), 0);
            assert!(contract.state_records(item.owner).unwrap().is_empty());
            assert!(contract.groups.get(item.group_key).unwrap().is_none());
        });
    }

    #[test]
    fn a_gem_without_an_owner_is_rejected_before_any_write() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let day = WorldwideDay::new(20261015);
            let item = GemItemState {
                gem_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(7)),
                owner: Address::ZERO,
                gem_type: 0,
                promis_load_minor: U256::from(5_000_000),
                issuance_currency: 840,
                reference_currency: 840,
                issued_at: day.start_timestamp(),
                group_key: crate::identity::group_key(day, U256::from(1_000_000), 840),
                state: GemState::Issued as u8,
                called_at: 0,
                settled_at: 0,
            };
            assert!(matches!(
                add_item(&storage, &item, U256::from(1_000_000), None),
                Err(PrecompileError::Revert(reason)) if reason == GemError::ZeroOwner.to_string()
            ));
            let contract = GemContract::new(storage.clone());
            assert_eq!(contract.total_supply.read().unwrap(), 0);
            assert!(contract.groups.get(item.group_key).unwrap().is_none());
        });
    }
}
