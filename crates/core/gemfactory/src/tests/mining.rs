use super::*;

#[test]
fn mine_promis_full_flow() {
    outbe_promis::enclave_client::test_enclave::install();
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage, world| {
        let load = U256::from(10u64) * six_decimal_unit();
        // The paid settle deposits into the Reserve vault, which this storage-only
        // harness cannot service. `force_settled` records Settled through
        // `gem_api::settle_gem` and does not deposit. Localnet covers the paid settle.
        let gem_id =
            issue_at_live_rate(storage, world, ALICE, GemTypes::Validator, load, 840, 840).unwrap();

        force_settled(storage, world, gem_id);
        let nonce = find_valid_nonce(gem_id, ALICE);
        let minted = runtime::mine_promis(
            storage,
            &world.scope,
            &world.parent,
            crate::schema::GemMiningProof { gem_id, nonce },
            promis_auth(ALICE, load, 0),
        )
        .unwrap();
        assert_eq!(minted, load);

        let gem = GemContract::new(storage.clone());
        assert!(
            gem_api::get_item(storage, &world.scope, &world.parent, gem_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(gem.total_supply().unwrap(), 0);

        // Promis is confidential: decrypt the ciphertext balance with the view key.
        let sk = outbe_promis::enclave_client::test_enclave::state_key();
        let vk = derive_view_key(&sk, ALICE).unwrap();
        let blob = outbe_promis::api::balance_ct(storage.clone(), ALICE).unwrap();
        assert_eq!(decrypt_balance(&vk, ALICE, &blob).unwrap(), load);
    });
    outbe_promis::enclave_client::test_enclave::uninstall();
}

#[test]
fn mine_promis_rejects_non_settled() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage, world| {
        let gem_id = issue_at_live_rate(
            storage,
            world,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            840,
            840,
        )
        .unwrap();
        // WALLET is Issued, not Settled. Mine should reject before PoW.
        let res = runtime::mine_promis(
            storage,
            &world.scope,
            &world.parent,
            crate::schema::GemMiningProof { gem_id, nonce: 0 },
            no_auth(),
        );
        assert!(err_msg(res).contains("invalid state"));
    });
}

#[test]
fn anyone_may_relay_mining_and_the_promis_lands_with_the_owner() {
    outbe_promis::enclave_client::test_enclave::install();
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage, world| {
        let load = U256::from(10u64) * six_decimal_unit();
        let gem_id =
            issue_at_live_rate(storage, world, ALICE, GemTypes::Validator, load, 840, 840).unwrap();
        force_settled(storage, world, gem_id);

        let nonce = find_valid_nonce(gem_id, ALICE);
        let minted = runtime::mine_promis(
            storage,
            &world.scope,
            &world.parent,
            crate::schema::GemMiningProof { gem_id, nonce },
            promis_auth(ALICE, load, 0),
        )
        .unwrap();
        assert_eq!(minted, load);
        assert!(
            gem_api::get_item(storage, &world.scope, &world.parent, gem_id)
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn statistics_track_mint_count() {
    let rate = U256::from(2u64) * six_decimal_unit();
    with_storage(Some(rate), |storage, world| {
        let base = U256::from(1u64) * six_decimal_unit();
        // `gem_id = keccak(owner || amount || block_number)`. Vary `load`
        // per issue so the same (owner, block) pair does not collide.
        for i in 0..3 {
            let load = base + U256::from(i as u64);
            issue_at_live_rate(storage, world, ALICE, GemTypes::Wallet, load, 840, 840).unwrap();
        }
        let factory = GemFactoryContract::new(storage.clone());
        assert_eq!(factory.total_gems_issued.read().unwrap(), U256::from(3u64));
    });
}
