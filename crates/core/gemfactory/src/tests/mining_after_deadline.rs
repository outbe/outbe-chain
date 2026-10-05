//! A paid Gem has no deadline: a settled Gem whose bucket was called mines its
//! Promis after the settlement deadline, while its unpaid twin is refused.

use super::*;
use outbe_gem::GemParams;
use outbe_primitives::addresses::GEM_FACTORY_ADDRESS;

use crate::precompile::IGemFactory;

/// Two Genesis Gems of `load` in one bucket, priced at 2 COEN/USD: Alice's is
/// settled, Bob's is not. The bucket is marked called at `T_NOW` the way the
/// call sweep records it, so the unpaid member reads Called with that stamp and
/// both share the bucket's settlement deadline.
fn called_bucket(load: U256) -> (HashMapStorageProvider, U256, U256) {
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    let ids = StorageHandle::enter(&mut provider, |handle| {
        GemContract::new(handle.clone())
            .config_profile
            .write(outbe_gem::config::PROFILE_PROD)
            .unwrap();
        let oracle = OracleContract::new(handle.clone());
        oracle.reference_currencies.push(840u16).unwrap();
        oracle.config_lookback_duration.write(86_400).unwrap();
        outbe_oracle::api::register_pair(handle.clone(), outbe_oracle::api::DAY_TYPE_PAIR).unwrap();
        outbe_oracle::api::set_exchange_rate(
            handle.clone(),
            Address::ZERO,
            outbe_oracle::api::DAY_TYPE_PAIR,
            rate,
            1,
            T_NOW,
        )
        .unwrap();
        let price = outbe_oracle::api::fresh_coen_rate_for(handle.clone(), 840).unwrap();
        let paid =
            runtime::issue_gem(&handle, ALICE, GemTypes::Genesis, load, 840, 840, price).unwrap();
        let unpaid =
            runtime::issue_gem(&handle, BOB, GemTypes::Genesis, load, 840, 840, price).unwrap();
        // Settling leaves the bucket, so take the key while both are members.
        let bucket = gem_api::bucket_of(&handle, paid).unwrap();
        assert_eq!(gem_api::bucket_of(&handle, unpaid).unwrap(), bucket);
        assert!(!bucket.is_zero());
        gem_api::set_state(&handle, paid, GemState::Settled).unwrap();
        GemContract::new(handle.clone())
            .bucket_called_at
            .write(&bucket, T_NOW)
            .unwrap();
        (paid, unpaid)
    });
    (provider, ids.0, ids.1)
}

#[test]
fn a_settled_gem_mines_after_the_settlement_deadline_while_its_unpaid_twin_is_refused() {
    outbe_promis::enclave_client::test_enclave::install();
    let load = U256::from(10u64) * six_decimal_unit();
    let (mut provider, paid, unpaid) = called_bucket(load);
    let deadline = T_NOW + u64::from(GemParams::PROD.call_notice_period_seconds);
    provider.set_timestamp(U256::from(deadline + 1));

    StorageHandle::enter(&mut provider, |handle| {
        let handle = &handle;
        let twin = gem_api::get_gem(handle, unpaid).unwrap().unwrap();
        assert_eq!(twin.state, GemState::Called as u8, "the bucket is called");
        assert_eq!(twin.called_at, T_NOW);
        let gem = gem_api::get_gem(handle, paid).unwrap().unwrap();
        assert_eq!(gem.state, GemState::Settled as u8);
        assert!(
            handle.timestamp().unwrap().to::<u64>() > deadline,
            "the fixture sits past the settlement deadline"
        );

        // The unpaid twin is refused at the mining gate.
        let refused = runtime::mine_promis(
            handle,
            unpaid,
            find_valid_nonce(unpaid, BOB),
            promis_auth(BOB, load, 0),
        )
        .unwrap_err();
        assert!(
            format!("{refused:?}").contains("invalid state for action"),
            "{refused:?}"
        );
        assert!(gem_api::get_gem(handle, unpaid).unwrap().is_some());

        // The paid Gem mines.
        let nonce = find_valid_nonce(paid, ALICE);
        let minted =
            runtime::mine_promis(handle, paid, nonce, promis_auth(ALICE, load, 0)).unwrap();
        assert_eq!(minted, load);
        assert!(
            gem_api::get_gem(handle, paid).unwrap().is_none(),
            "exercised"
        );

        let sk = outbe_promis::enclave_client::test_enclave::state_key();
        let vk = derive_view_key(&sk, ALICE).unwrap();
        let blob = outbe_promis::api::balance_ct(handle.clone(), ALICE).unwrap();
        assert_eq!(decrypt_balance(&vk, ALICE, &blob).unwrap(), load);
    });
    let exercised: Vec<_> = provider
        .get_events(GEM_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| IGemFactory::GemExercised::decode_log_data(log).ok())
        .collect();
    assert_eq!(exercised.len(), 1);
    assert_eq!(exercised[0].gemId, paid);
    assert_eq!(exercised[0].owner, ALICE);
    assert_eq!(exercised[0].promisLoadMinor, load);
    outbe_promis::enclave_client::test_enclave::uninstall();
}
