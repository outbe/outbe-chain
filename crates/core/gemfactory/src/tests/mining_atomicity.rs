//! Mining is one transition: a Gem that fails to mint its Promis is not burned.

use super::*;

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
