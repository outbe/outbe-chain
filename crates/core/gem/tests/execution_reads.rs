//! Reads during execution: a reverted issuance leaves nothing behind, and a removal
//! consumes the capabilities it was handed instead of reading the parent again.

use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_gem::{api, precompile::IGem, GemAddParams, GemContract};
use outbe_primitives::addresses::GEM_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

const ISSUED_AT: u64 = 1_704_067_200;

fn params(owner: Address) -> GemAddParams {
    GemAddParams {
        owner,
        promis_load_minor: U256::from(1_000_000u64),
        entry_price_minor: U256::from(500_000u64),
        issuance_currency: 840,
        reference_currency: 840,
        issued_at: ISSUED_AT,
        call_terms: None,
        market_price: true,
    }
}

fn provider() -> HashMapStorageProvider {
    HashMapStorageProvider::new(1)
}

#[test]
fn reverted_issuance_rolls_back_state_and_events() {
    let mut provider = provider();
    let owner = Address::repeat_byte(0x66);
    let gem_id = StorageHandle::enter(&mut provider, |storage| {
        let params = params(owner);
        let series = GemContract::series_of(
            params.issued_at,
            params.issuance_currency,
            params.reference_currency,
        )
        .unwrap();
        let expected =
            GemContract::generate_gem_id(series, 0, owner, storage.block_number().unwrap());
        let outcome: Result<U256> = storage.with_checkpoint(|| {
            let gem_id = api::add_gem(&storage, params.clone())?;
            assert_eq!(gem_id, expected);
            assert!(api::get_item(&storage, gem_id)?.is_some());
            Err(PrecompileError::Revert("nested caller reverted".into()))
        });
        assert!(outcome.is_err());
        assert!(api::list_by_owner(&storage, owner).unwrap().is_empty());
        assert!(api::get_item(&storage, expected).unwrap().is_none());
        let gem_id = api::add_gem(&storage, params).unwrap();
        assert_eq!(gem_id, expected);
        assert_eq!(api::balance_of(&storage, owner).unwrap(), 1);
        let owned = api::list_by_owner(&storage, owner).unwrap();
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].gem_id, gem_id);
        assert_eq!(
            api::get_item(&storage, gem_id).unwrap().unwrap().gem_id,
            gem_id
        );
        assert_eq!(api::tokens_by_owner(&storage, owner).unwrap(), vec![gem_id]);
        let output = outbe_gem::precompile::dispatch(
            storage.clone(),
            &IGem::getGemCall { gemId: gem_id }.abi_encode(),
            owner,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IGem::getGemCall::abi_decode_returns(&output).unwrap().gemId,
            gem_id
        );
        gem_id
    });
    let events = provider.get_events(GEM_ADDRESS);
    assert_eq!(events.len(), 1);
    let transfer = IGem::Transfer::decode_log_data(&events[0]).unwrap();
    assert_eq!(transfer.from, Address::ZERO);
    assert_eq!(transfer.to, owner);
    assert_eq!(transfer.tokenId, gem_id);
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
        assert_eq!(api::balance_of(&storage, owner).unwrap(), 0);
    });
}
