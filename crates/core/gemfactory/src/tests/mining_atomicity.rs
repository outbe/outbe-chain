//! Mining is one transition: a Gem that fails to mint its Promis is not burned.

use super::*;
use outbe_primitives::addresses::GEM_FACTORY_ADDRESS;

use crate::precompile::IGemFactory;

/// One settled Genesis Gem of `load` Promis owned by Alice, priced at 2 COEN/USD.
fn settled_gem(load: U256) -> (HashMapStorageProvider, U256) {
    let mut provider = test_storage(Some(U256::from(2u64) * six_decimal_unit()));
    let gem_id = StorageHandle::enter(&mut provider, |storage| {
        let gem_id =
            issue_at_live_rate(&storage, ALICE, GemTypes::Genesis, load, 840, 840).unwrap();
        gem_api::set_state(&storage, gem_id, GemState::Settled).unwrap();
        gem_id
    });
    (provider, gem_id)
}

fn promis_balance(storage: &StorageHandle<'_>, account: Address) -> U256 {
    let sk = outbe_promis::enclave_client::test_enclave::state_key();
    let vk = derive_view_key(&sk, account).unwrap();
    let blob = outbe_promis::api::balance_ct(storage.clone(), account).unwrap();
    if blob.is_empty() {
        U256::ZERO
    } else {
        decrypt_balance(&vk, account, &blob).unwrap()
    }
}

#[test]
fn a_rejected_promis_mint_leaves_the_settled_gem_and_its_nonce_untouched() {
    outbe_promis::enclave_client::test_enclave::install();
    let load = U256::from(10u64) * six_decimal_unit();
    let (mut provider, gem_id) = settled_gem(load);
    StorageHandle::enter(&mut provider, |storage| {
        let storage = &storage;
        let nonce = find_valid_nonce(gem_id, ALICE);
        // A modify authorization for the wrong amount: the enclave rejects the
        // mint as a business failure (not an infrastructure fault).
        let wrong = promis_auth(ALICE, load - U256::ONE, 0);
        let error = runtime::mine_promis(storage, gem_id, nonce, wrong).unwrap_err();
        assert!(
            matches!(error, outbe_primitives::error::PrecompileError::Revert(_)),
            "{error:?}"
        );

        let gem = GemContract::new(storage.clone());
        let item = gem
            .get_gem(gem_id)
            .unwrap()
            .expect("a failed mint must not burn the Gem");
        assert_eq!(item.state, GemState::Settled as u8);
        assert_eq!(gem.total_supply().unwrap(), 1);
        assert_eq!(promis_balance(storage, ALICE), U256::ZERO);
        assert_eq!(
            outbe_promis::api::op_nonce(storage.clone(), ALICE).unwrap(),
            0,
            "a rejected op consumes no ledger nonce"
        );

        // The entitlement is intact: the same nonce and a correct authorization mine it.
        let minted =
            runtime::mine_promis(storage, gem_id, nonce, promis_auth(ALICE, load, 0)).unwrap();
        assert_eq!(minted, load);
        assert!(gem.get_gem(gem_id).unwrap().is_none());
        assert_eq!(promis_balance(storage, ALICE), load);
    });
    outbe_promis::enclave_client::test_enclave::uninstall();
}

#[test]
fn a_mining_that_fails_after_any_write_keeps_the_settled_gem_and_mines_once_on_retry() {
    outbe_promis::enclave_client::test_enclave::install();
    let load = U256::from(10u64) * six_decimal_unit();
    let mine = |provider: &mut HashMapStorageProvider, gem_id: U256| {
        let nonce = find_valid_nonce(gem_id, ALICE);
        StorageHandle::enter(provider, |storage| {
            runtime::mine_promis(&storage, gem_id, nonce, promis_auth(ALICE, load, 0))
        })
    };

    let (mut provider, gem_id) = settled_gem(load);
    provider.clear_mutation_failure();
    assert_eq!(mine(&mut provider, gem_id).unwrap(), load);
    let writes = provider.clear_mutation_failure();
    assert!(writes > 0);

    for failure_at in 0..writes {
        let (mut provider, gem_id) = settled_gem(load);
        let slots = provider.storage.clone();
        let events = provider.get_ordered_events().to_vec();
        provider.fail_after_mutation_at(failure_at);
        assert!(mine(&mut provider, gem_id).is_err(), "failure {failure_at}");
        assert_eq!(
            provider.storage, slots,
            "a write survived failure {failure_at}"
        );
        assert_eq!(
            provider.get_ordered_events(),
            events,
            "failure {failure_at}"
        );
        StorageHandle::enter(&mut provider, |storage| {
            let item = gem_api::get_gem(&storage, gem_id).unwrap().unwrap();
            assert_eq!(item.state, GemState::Settled as u8, "failure {failure_at}");
            assert_eq!(
                outbe_promis::api::op_nonce(storage.clone(), ALICE).unwrap(),
                0
            );
            assert!(outbe_promis::api::balance_ct(storage.clone(), ALICE)
                .unwrap()
                .is_empty());
        });

        provider.clear_mutation_failure();
        assert_eq!(mine(&mut provider, gem_id).unwrap(), load);
        assert!(mine(&mut provider, gem_id).is_err(), "the Gem mines once");
        StorageHandle::enter(&mut provider, |storage| {
            assert_eq!(promis_balance(&storage, ALICE), load);
            assert_eq!(
                outbe_promis::api::op_nonce(storage.clone(), ALICE).unwrap(),
                1
            );
        });
        let exercised = provider
            .get_events(GEM_FACTORY_ADDRESS)
            .iter()
            .filter(|log| IGemFactory::GemExercised::decode_log_data(log).is_ok())
            .count();
        assert_eq!(exercised, 1, "failure {failure_at}");
    }
    outbe_promis::enclave_client::test_enclave::uninstall();
}
