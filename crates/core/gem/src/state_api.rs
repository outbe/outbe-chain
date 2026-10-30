use crate::errors::{date_step_error, GemError};
use crate::precompile::IGem;
use crate::schema::{CallTerms, GemContract, GemGroupState, GemItemState};
use crate::GemState;
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{error::Result, storage::StorageHandle};

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

pub fn load_item(storage: &StorageHandle<'_>, id: U256) -> Result<Option<LoadedGem>> {
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
    terms: Option<CallTerms>,
) -> Result<()> {
    storage
        .with_checkpoint(|| GemContract::new(storage.clone()).record_gem_issued_state(item, terms))
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
        return Err(GemError::GemStateChangedAfterLoading.into());
    }
    Ok(())
}
impl GemContract<'_> {
    fn record_gem_issued_state(
        &mut self,
        item: &GemItemState,
        call_terms: Option<CallTerms>,
    ) -> Result<()> {
        if item.owner.is_zero() {
            return Err(GemError::ZeroOwner.into());
        }
        if self.records.get(item.gem_id)?.is_some() {
            return Err(GemError::AlreadyExists.into());
        }
        let group_key = item.group_key;
        let group_id = item.group_id().map_err(date_step_error)?;
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
                if let Some(terms) = call_terms {
                    self.seal_group_call_terms(group_key, terms)?;
                    self.insert_call_bin(group_key)?;
                }
                Some(GemGroupState {
                    group_key,
                    full_day_after_issue: item.full_day_after_issue().map_err(date_step_error)?,
                    reference_currency: item.reference_currency,
                    settled_gems: 0,
                })
            }
        };
        self.add_unpaid_load(group_key, item.promis_load_minor)?;

        self.insert_group_member(group_key, item.gem_id)?;
        self.insert_state_record(item)?;
        if let Some(group) = new_group {
            self.groups.create(&group)?;
        }
        self.emit(IGem::Transfer {
            from: Address::ZERO,
            to: item.owner,
            tokenId: item.gem_id,
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
        self.remove_group_member(item.group_key, item.gem_id)?;
        self.take_unpaid_load(item.group_key, item.promis_load_minor)?;
        item.called_at = self.group_called_at.read(&item.group_key)?;
        item.settled_at = self.storage.now_secs()?;
        item.state = GemState::Settled as u8;
        self.records.update(&item)?;
        self.groups.update(&group)?;
        self.emit(IGem::MetadataUpdate {
            _tokenId: item.gem_id,
        })
    }

    fn record_gem_removed_state(&mut self, item: LoadedGem, group: LoadedGemGroup) -> Result<()> {
        let item = item.into_body();
        let mut group = group.into_body();
        check_loaded(self, &item, &group)?;
        let group_id = group.entity_id();
        let remaining = if item.is_settled() {
            group.settled_gems = group
                .settled_gems
                .checked_sub(1)
                .ok_or(GemError::SettledCountUnderflow { group_id })?;
            self.group_gem_count.read(&item.group_key)?
        } else {
            self.take_unpaid_load(item.group_key, item.promis_load_minor)?;
            self.remove_group_member(item.group_key, item.gem_id)?
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
            tokenId: item.gem_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use alloy_sol_types::SolError;
    use outbe_primitives::{
        error::PrecompileError, storage::hashmap::HashMapStorageProvider, time::WorldwideDay,
    };

    #[test]
    fn state_only_gem_settlement_rejects_stale_handles_and_burns_atomically() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let day = WorldwideDay::new(20261015);
            let item = GemItemState {
                gem_id: U256::from_be_bytes([7; 32]),
                owner: Address::repeat_byte(1),
                promis_load_minor: U256::from(5_000_000),
                issuance_currency: 840,
                reference_currency: 840,
                issued_at: day.start_timestamp(),
                group_key: outbe_primitives::call_breach::group_key(
                    day.value(),
                    U256::from(2_280_000),
                    840,
                ),
                state: GemState::Issued as u8,
                called_at: 0,
                settled_at: 0,
                entry_price_minor: U256::from(1_000_000),
                series_id: Default::default(),
            };
            add_item(&storage, &item, None).unwrap();
            let contract = GemContract::new(storage.clone());
            assert_eq!(crate::api::balance_of(&storage, item.owner).unwrap(), 1);
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
            assert_eq!(crate::api::balance_of(&storage, item.owner).unwrap(), 0);
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
                gem_id: U256::from_be_bytes([7; 32]),
                owner: Address::ZERO,
                promis_load_minor: U256::from(5_000_000),
                issuance_currency: 840,
                reference_currency: 840,
                issued_at: day.start_timestamp(),
                group_key: outbe_primitives::call_breach::group_key(
                    day.value(),
                    U256::from(2_280_000),
                    840,
                ),
                state: GemState::Issued as u8,
                called_at: 0,
                settled_at: 0,
                entry_price_minor: U256::from(1_000_000),
                series_id: Default::default(),
            };
            assert!(matches!(
                add_item(&storage, &item, None),
                Err(PrecompileError::RevertBytes(bytes))
                    if bytes.as_ref() == IGem::ZeroOwner {}.abi_encode().as_slice()
            ));
            let contract = GemContract::new(storage.clone());
            assert_eq!(crate::api::balance_of(&storage, item.owner).unwrap(), 0);
            assert!(contract.groups.get(item.group_key).unwrap().is_none());
        });
    }
}
