//! Reads during execution: a reverted issuance leaves nothing behind, and a removal
//! consumes the capabilities it was handed instead of reading the parent again.

use alloy_primitives::{Address, U256};
use outbe_gem::{api, GemAddParams, GemContract};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

const ISSUED_AT: u64 = 1_704_067_200;

fn params(owner: Address) -> GemAddParams {
    GemAddParams {
        owner,
        gem_type: 2,
        promis_load_minor: U256::from(1_000_000u64),
        entry_price_minor: U256::from(500_000u64),
        issuance_currency: 840,
        reference_currency: 840,
        issued_at: ISSUED_AT,
        call_terms: None,
    }
}

fn provider() -> HashMapStorageProvider {
    HashMapStorageProvider::new(1)
}

#[test]
fn reverted_issuance_rolls_back_state_and_events() {
    let mut provider = provider();
    let owner = Address::repeat_byte(0x66);
    StorageHandle::enter(&mut provider, |storage| {
        let outcome: Result<U256> = storage.with_checkpoint(|| {
            let gem_id = api::add_gem(&storage, params(owner))?;
            assert!(api::get_item(&storage, gem_id)?.is_some());
            Err(PrecompileError::Revert("nested caller reverted".into()))
        });
        assert!(outcome.is_err());
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.total_supply().unwrap(), 0);
        assert!(api::list_by_owner(&storage, owner).unwrap().is_empty());
    });
}

#[test]
fn removal_consumes_loaded_state() {
    let mut provider = provider();
    let owner = Address::repeat_byte(0x41);
    StorageHandle::enter(&mut provider, |storage| {
        let gem_id = api::add_gem(&storage, params(owner)).unwrap();
        let (item, group) = api::load_pair(&storage, gem_id).unwrap().unwrap();

        api::remove_gem(&storage, item, group).unwrap();

        assert!(api::get_item(&storage, gem_id).unwrap().is_none());
        assert_eq!(GemContract::new(storage).total_supply().unwrap(), 0);
    });
}
