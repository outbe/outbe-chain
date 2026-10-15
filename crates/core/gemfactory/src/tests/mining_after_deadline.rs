//! A paid Gem has no deadline: a settled Gem whose bucket was called mines its
//! Promis after the settlement deadline, while its unpaid twin is refused.

use super::*;
use outbe_gem::GemParams;
use outbe_primitives::addresses::GEM_FACTORY_ADDRESS;

use crate::precompile::IGemFactory;

/// Alice's and Bob's Genesis Gems of `load` share a bucket called at `T_NOW`;
/// Alice's is settled after the call, so it keeps that `called_at`.
fn called_bucket(load: U256) -> (HashMapStorageProvider, GemWorld, U256, U256) {
    let (mut provider, world) = test_storage(Some(U256::from(2u64) * six_decimal_unit()));
    let ids = StorageHandle::enter(&mut provider, |handle| {
        let paid =
            issue_at_live_rate(&handle, &world, ALICE, GemTypes::Genesis, load, 840, 840).unwrap();
        let unpaid =
            issue_at_live_rate(&handle, &world, BOB, GemTypes::Genesis, load, 840, 840).unwrap();
        let bucket = gem_item(&handle, &world, paid).bucket_key;
        assert_eq!(gem_item(&handle, &world, unpaid).bucket_key, bucket);
        assert!(!bucket.is_zero());
        GemContract::new(handle.clone())
            .bucket_called_at
            .write(&bucket, T_NOW)
            .unwrap();
        record_settled(&handle, &world, paid);
        (paid, unpaid)
    });
    (provider, world, ids.0, ids.1)
}

#[test]
fn a_settled_gem_mines_after_the_settlement_deadline_while_its_unpaid_twin_is_refused() {
    outbe_promis::enclave_client::test_enclave::install();
    let load = U256::from(10u64) * six_decimal_unit();
    let (mut provider, world, paid, unpaid) = called_bucket(load);
    let deadline = T_NOW + u64::from(GemParams::PROD.call_notice_period_seconds);
    provider.set_timestamp(U256::from(deadline + 1));

    StorageHandle::enter(&mut provider, |handle| {
        let handle = &handle;
        let now = handle.timestamp().unwrap().to::<u64>();
        let twin = gem_view(handle, &world, unpaid);
        assert_eq!(
            twin.effective_state(T_NOW),
            GemState::Called as u8,
            "the bucket is called"
        );
        assert_eq!(twin.called_at(), T_NOW);
        assert_eq!(
            twin.effective_state(now),
            GemState::Forfeited as u8,
            "the settlement deadline has passed"
        );
        let gem = gem_view(handle, &world, paid);
        assert_eq!(gem.item.state, GemState::Settled as u8);
        assert_eq!(gem.called_at(), T_NOW, "the paid Gem shares the deadline");

        // The unpaid twin is refused at the mining gate.
        let refused = runtime::mine_promis(
            handle,
            &world.scope,
            &world.parent,
            crate::schema::GemMiningProof {
                gem_id: unpaid,
                nonce: find_valid_nonce(unpaid, BOB),
            },
            promis_auth(BOB, load, 0),
        )
        .unwrap_err();
        assert!(
            format!("{refused:?}").contains("invalid state for action"),
            "{refused:?}"
        );
        assert!(
            gem_api::get_item(handle, &world.scope, &world.parent, unpaid)
                .unwrap()
                .is_some()
        );

        // The paid Gem mines.
        let nonce = find_valid_nonce(paid, ALICE);
        let minted = runtime::mine_promis(
            handle,
            &world.scope,
            &world.parent,
            crate::schema::GemMiningProof {
                gem_id: paid,
                nonce,
            },
            promis_auth(ALICE, load, 0),
        )
        .unwrap();
        assert_eq!(minted, load);
        assert!(
            gem_api::get_item(handle, &world.scope, &world.parent, paid)
                .unwrap()
                .is_none(),
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
