//! Issue, bucket membership, settlement and burn through the compressed-body store.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::WwdEntityId;
use outbe_primitives::addresses::GEM_ADDRESS;
use outbe_primitives::math::constants::REAL_ID_SHIFT;
use outbe_primitives::math::tree_math;
use outbe_primitives::storage::StorageHandle;

use super::*;
use outbe_gem::config::GemParams;
use outbe_gem::schema::{GemContract, GemState};

#[test]
fn coen_iso_one_maps_to_the_center_price_bin_at_six_decimals() {
    assert_eq!(
        GemContract::price_to_bin(U256::from(1_000_000u64)).unwrap(),
        REAL_ID_SHIFT as u32
    );
}

#[test]
fn initial_state_empty() {
    with_storage(|storage, world| {
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert!(owned(storage, world, ALICE).is_empty());
    });
}

#[test]
fn add_gem_mints_the_body_and_its_bucket_and_bumps_counters() {
    with_storage(|storage, world| {
        let gem_id = add(storage, world, sample_params(ALICE));
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 1);
        assert_eq!(owned(storage, world, ALICE), [gem_id]);
        let stored = item(storage, world, gem_id).unwrap();
        assert_eq!(stored.owner, ALICE);
        assert_eq!(stored.state, GemState::Issued as u8);
        assert_eq!(stored.gem_id.to_u256(), gem_id);
        let gem = view(storage, world, gem_id);
        assert_eq!(gem.bucket.entry_price_minor, U256::from(500_000u64));
        assert_eq!(gem.bucket.settled_gems, 0);
        assert_eq!(gem.terms.call_rate, GemParams::DEV.call_rate);
        assert_eq!(
            gem.terms.call_price_minor,
            U256::from(500_000u64 * (100 + u64::from(GemParams::DEV.call_rate)) / 100)
        );
    });
}

#[test]
fn same_block_issue_is_visible_to_point_and_owner_list_reads() {
    let (mut provider, world) = provider_at(T_NOW);
    let gem_id = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = add(&storage, &world, sample_params(ALICE));
        assert_eq!(item(&storage, &world, gem_id).unwrap().owner, ALICE);
        assert_eq!(owned(&storage, &world, ALICE), [gem_id]);
        gem_id
    });
    let signatures: Vec<_> = provider
        .get_events(GEM_ADDRESS)
        .iter()
        .map(|event| event.topics()[0])
        .collect();
    assert_eq!(
        signatures,
        [
            IGem::GemBodyStored::SIGNATURE_HASH,
            IGem::GemBucketBodyStored::SIGNATURE_HASH,
            IGem::Transfer::SIGNATURE_HASH,
        ]
    );
    let stored = provider
        .get_events(GEM_ADDRESS)
        .iter()
        .find_map(|log| IGem::GemBodyStored::decode_log_data(log).ok())
        .unwrap();
    assert_eq!(stored.gemId, gem_id);
}

#[test]
fn add_gem_rejects_zero_owner() {
    with_storage(|storage, world| {
        let mut p = sample_params(ALICE);
        p.owner = Address::ZERO;
        assert!(crate::runtime::add_gem(storage, &world.scope, &world.parent, p).is_err());
    });
}

#[test]
fn a_duplicate_id_is_rejected_and_rolls_back() {
    with_storage(|storage, world| {
        let gem_id = add(storage, world, sample_params(ALICE));
        let error =
            crate::runtime::add_gem(storage, &world.scope, &world.parent, sample_params(ALICE))
                .unwrap_err();
        assert!(format!("{error:?}").contains("already exists"), "{error:?}");
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 1);
        assert_eq!(owned(storage, world, ALICE), [gem_id]);
    });
}

#[test]
fn enumerable_returns_only_owned_gems() {
    with_storage(|storage, world| {
        let g1 = add(storage, world, sample_params(ALICE));
        let mut p2 = sample_params(ALICE);
        p2.promis_load_minor = U256::from(2u64);
        let g2 = add(storage, world, p2);
        let _g3 = add(storage, world, sample_params(BOB));

        let alice = owned(storage, world, ALICE);
        assert_eq!(alice.len(), 2);
        assert!(alice.contains(&g1));
        assert!(alice.contains(&g2));
        assert_eq!(owned(storage, world, BOB).len(), 1);
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 3);
    });
}

#[test]
fn burn_requires_settled_state() {
    with_storage(|storage, world| {
        let gem_id = add(storage, world, sample_params(ALICE));
        assert!(
            crate::runtime::burn_settled(storage, &world.scope, &world.parent, gem_id).is_err()
        );

        qualify_and_settle(storage, world, gem_id);
        burn_settled(storage, world, gem_id);

        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert!(owned(storage, world, ALICE).is_empty());
        assert!(item(storage, world, gem_id).is_none());
    });
}

#[test]
fn burn_leaves_the_owners_other_gems_listed() {
    with_storage(|storage, world| {
        let gems = alice_gems(storage, world, &[1, 2, 3]);
        qualify_and_settle(storage, world, gems[0]);
        burn_settled(storage, world, gems[0]);

        let remaining = owned(storage, world, ALICE);
        assert_eq!(remaining.len(), 2);
        assert!(remaining.contains(&gems[1]));
        assert!(remaining.contains(&gems[2]));
    });
}

/// Gems share a bucket when they were issued on the same worldwide day at the same
/// entry price in the same reference currency. Nothing else is a term.
#[test]
fn a_bucket_holds_the_gems_of_one_day_entry_price_and_currency() {
    with_storage(|storage, world| {
        let gems = alice_gems(storage, world, &[1, 2]);
        let bucket = bucket_of(storage, world, gems[0]);
        assert!(!bucket.is_zero());
        assert_eq!(
            bucket_of(storage, world, gems[1]),
            bucket,
            "the load is not a term"
        );
        assert_eq!(
            bucket,
            outbe_gem::identity::bucket_key(
                item(storage, world, gems[0]).unwrap().worldwide_day(),
                U256::from(500_000u64),
                USD
            )
        );

        let nonce = std::cell::Cell::new(0u64);
        let apart = |edit: &dyn Fn(&mut GemAddParams)| {
            let mut params = sample_params(BOB);
            nonce.set(nonce.get() + 1);
            params.promis_load_minor = U256::from(nonce.get());
            edit(&mut params);
            bucket_of(storage, world, add(storage, world, params))
        };
        assert_ne!(apart(&|p| p.issued_at += 86_400), bucket, "another day");
        assert_eq!(apart(&|p| p.issued_at += 1), bucket, "the same day");
        assert_ne!(apart(&|p| p.reference_currency = EUR), bucket);
        assert_ne!(apart(&|p| p.entry_price_minor += U256::from(1u64)), bucket);
        assert_eq!(
            apart(&|p| p.floor_price_minor += U256::from(1u64)),
            bucket,
            "the floor is the gem's own"
        );
        set_profile(storage, outbe_gem::config::PROFILE_PROD);
        assert_eq!(
            apart(&|_| {}),
            bucket,
            "a profile change does not re-term an open bucket"
        );
    });
}

/// A later member inherits the terms the bucket sealed when it opened.
#[test]
fn profile_change_after_open_does_not_reterm_the_bucket() {
    with_storage(|storage, world| {
        set_profile(storage, outbe_gem::config::PROFILE_DEV);
        let first = add(storage, world, sample_params(ALICE));
        set_profile(storage, outbe_gem::config::PROFILE_PROD);
        let mut p = sample_params(BOB);
        p.issued_at += 3_600;
        let second = add(storage, world, p);

        assert_eq!(
            bucket_of(storage, world, first),
            bucket_of(storage, world, second)
        );
        let terms = view(storage, world, second).terms;
        assert_eq!(
            terms.call_window_seconds,
            GemParams::DEV.call_window_seconds
        );
        assert_eq!(terms.call_rate, GemParams::DEV.call_rate);
        assert_eq!(
            GemContract::new(storage.clone())
                .bucket_issued_at
                .read(&bucket_of(storage, world, first))
                .unwrap(),
            T_NOW,
            "the first member's issuance starts the bucket's breach count"
        );
    });
}

#[test]
fn leaving_a_bucket_moves_its_last_member_into_the_hole() {
    with_storage(|storage, world| {
        let gems = alice_gems(storage, world, &[1, 2, 3]);
        let bucket = bucket_of(storage, world, gems[0]);
        let gem = GemContract::new(storage.clone());

        qualify_and_settle(storage, world, gems[0]);
        assert_eq!(gem.bucket_gem_count.read(&bucket).unwrap(), 2);
        assert_eq!(
            gem.bucket_gems
                .read(&GemContract::bucket_member_key(bucket, 0))
                .unwrap(),
            gems[2]
        );
        assert_eq!(gem.bucket_gem_index.read(&gems[2]).unwrap(), 0);
        assert_eq!(gem.bucket_gem_index.read(&gems[0]).unwrap(), 0);
        assert_eq!(
            item(storage, world, gems[0]).unwrap().bucket_key,
            bucket,
            "a settled gem still names its bucket"
        );
    });
}

/// The bucket body and its sealed terms outlive the unpaid membership while a paid
/// member exists; the last removal takes everything down.
#[test]
fn bucket_body_and_terms_survive_until_unpaid_and_settled_are_zero() {
    with_storage(|storage, world| {
        let gem_id = add(storage, world, sample_params(ALICE));
        let bucket = bucket_of(storage, world, gem_id);
        let bucket_id = item(storage, world, gem_id).unwrap().bucket_id();
        let gem = GemContract::new(storage.clone());
        let bin =
            GemContract::price_to_bin(view(storage, world, gem_id).terms.call_price_minor).unwrap();
        assert!(tree_math::contains(&outbe_gem::CallBins(&gem, USD), bin).unwrap());

        qualify_and_settle(storage, world, gem_id);
        assert_eq!(gem.bucket_gem_count.read(&bucket).unwrap(), 0);
        let body = api::get_bucket(storage, &world.scope, &world.parent, bucket_id)
            .unwrap()
            .expect("the paid member keeps the bucket");
        assert_eq!(body.settled_gems, 1);
        assert!(!gem.bucket_call_price_minor.read(&bucket).unwrap().is_zero());
        assert_eq!(
            view(storage, world, gem_id).bucket.entry_price_minor,
            U256::from(500_000u64)
        );
        // The bucket keeps its place in the trie while a paid member holds it open; the
        // scan skips a bucket without unpaid members.
        assert_ne!(gem.call_bucket_slot.read(&bucket).unwrap(), 0);
        let mut scan = GemContract::new(storage.clone());
        let window = breach_window(
            T_NOW,
            view(storage, world, gem_id).terms.call_price_minor + U256::from(1u64),
            (outbe_gem::constants::CALL_WINDOW / 86_400) as usize,
        );
        assert!(!crate::called::trigger_bucket_call(&mut scan, &window, bucket, T_NOW).unwrap());

        burn_settled(storage, world, gem_id);
        assert_eq!(
            gem.call_bucket_slot.read(&bucket).unwrap(),
            0,
            "out of the trie"
        );
        assert!(!tree_math::contains(&outbe_gem::CallBins(&gem, USD), bin).unwrap());
        assert!(
            api::get_bucket(storage, &world.scope, &world.parent, bucket_id)
                .unwrap()
                .is_none()
        );
        assert!(gem.bucket_call_price_minor.read(&bucket).unwrap().is_zero());
        assert_eq!(gem.bucket_issued_at.read(&bucket).unwrap(), 0);
    });
}

/// Removing an unpaid gem touches no bucket body; removing a paid one lowers the count.
#[test]
fn unpaid_removal_emits_no_bucket_update_settled_removal_decrements() {
    let (mut provider, world) = provider_at(T_NOW);
    StorageHandle::enter(&mut provider, |storage| {
        let gems = alice_gems(&storage, &world, &[1, 2]);
        call_gem(&storage, &world, gems[0], T_NOW);
        settle(&storage, &world, gems[1]);
        let bucket_id = item(&storage, &world, gems[1]).unwrap().bucket_id();
        assert_eq!(
            api::get_bucket(&storage, &world.scope, &world.parent, bucket_id)
                .unwrap()
                .unwrap()
                .settled_gems,
            1
        );
        // Forfeit the unpaid one: the bucket body stays as it is.
        begin_block_at(
            &storage,
            &world,
            GemContract::hour_end(GemContract::deadline_hour(
                T_NOW + u64::from(GemParams::DEV.call_notice_period_seconds),
            )),
        );
        assert!(item(&storage, &world, gems[0]).is_none());
        assert_eq!(
            api::get_bucket(&storage, &world.scope, &world.parent, bucket_id)
                .unwrap()
                .unwrap()
                .settled_gems,
            1
        );
        burn_settled(&storage, &world, gems[1]);
        assert!(
            api::get_bucket(&storage, &world.scope, &world.parent, bucket_id)
                .unwrap()
                .is_none()
        );
    });
    let bucket_events: Vec<_> = provider
        .get_events(GEM_ADDRESS)
        .iter()
        .filter_map(|log| {
            if IGem::GemBucketBodyStored::decode_log_data(log).is_ok() {
                Some("stored")
            } else if IGem::GemBucketBodyDeleted::decode_log_data(log).is_ok() {
                Some("deleted")
            } else {
                None
            }
        })
        .collect();
    // Open, settle (count up), delete. The forfeit of the unpaid member is silent.
    assert_eq!(bucket_events, ["stored", "stored", "deleted"]);
}

#[test]
fn settle_requires_a_called_or_qualified_gem() {
    with_storage(|storage, world| {
        let gem_id = add(storage, world, sample_params(ALICE));
        assert!(try_settle(storage, world, gem_id).is_err());
        assert_eq!(gem_state(storage, world, gem_id), GemState::Issued as u8);

        qualify_and_settle(storage, world, gem_id);
        let settled = item(storage, world, gem_id).unwrap();
        assert_eq!(settled.state, GemState::Settled as u8);
        assert_eq!(settled.settled_at, T_NOW);
        assert_eq!(settled.called_at, 0);
        assert!(try_settle(storage, world, gem_id).is_err());
    });
}

/// A low-floor sibling in the same bucket does not qualify a high-floor gem.
#[test]
fn settle_requires_per_item_qualification_not_bucket_sibling() {
    with_storage(|storage, world| {
        let low = add(storage, world, sample_params(ALICE));
        let mut high = sample_params(BOB);
        high.floor_price_minor = U256::from(900_000u64);
        let high = add(storage, world, high);
        assert_eq!(
            bucket_of(storage, world, low),
            bucket_of(storage, world, high)
        );

        seed_day_price(storage, USD, Some(U256::from(600_000u64)));
        assert!(is_qualified(storage, world, low));
        assert!(!is_qualified(storage, world, high));
        settle(storage, world, low);
        assert!(try_settle(storage, world, high).is_err());
    });
}

/// Settling one gem of a called bucket keeps its call stamp and leaves the rest called.
#[test]
fn settling_one_gem_of_a_called_bucket_leaves_the_rest_called() {
    with_storage(|storage, world| {
        let gems = alice_gems(storage, world, &[1, 2]);
        call_gem(storage, world, gems[0], T_NOW);
        assert_eq!(gem_state(storage, world, gems[1]), GemState::Called as u8);

        settle(storage, world, gems[0]);
        let settled = item(storage, world, gems[0]).unwrap();
        assert_eq!(settled.state, GemState::Settled as u8);
        assert_eq!(settled.called_at, T_NOW);
        assert_eq!(gem_state(storage, world, gems[0]), GemState::Settled as u8);
        assert_eq!(gem_state(storage, world, gems[1]), GemState::Called as u8);
        let entry = bucket_of(storage, world, gems[1]);
        assert_ne!(
            GemContract::new(storage.clone())
                .called_bucket_slot
                .read(&entry)
                .unwrap(),
            0,
            "the bucket still waits for its deadline"
        );
    });
}

#[test]
fn leaving_called_frees_the_expiry_slot() {
    with_storage(|storage, world| {
        let gem_id = mature_gem(storage, world);
        let gem = GemContract::new(storage.clone());
        call_gem(storage, world, gem_id, T_NOW);
        let hour = GemContract::deadline_hour(T_NOW + 7 * 86_400);
        assert_eq!(gem.expiry_bucket_live.read(&hour).unwrap(), 1);

        settle(storage, world, gem_id);
        burn_settled(storage, world, gem_id);
        assert_eq!(gem.expiry_bucket_live.read(&hour).unwrap(), 0);
        assert_eq!(gem.first_expiry_day().unwrap(), None);
    });
}

#[test]
fn only_a_genesis_gem_is_issued_without_a_floor() {
    with_storage(|storage, world| {
        let mut p = sample_params(ALICE);
        p.floor_price_minor = U256::ZERO;
        assert!(crate::runtime::add_gem(storage, &world.scope, &world.parent, p.clone()).is_err());

        p.gem_type = outbe_gem::GENESIS_GEM_TYPE;
        assert!(crate::runtime::add_gem(storage, &world.scope, &world.parent, p).is_ok());
    });
}

/// A stale bucket capability (loaded before a sibling settled) is rejected by the
/// generic store, and the gem's own accounting stays untouched.
#[test]
fn stale_bucket_capability_is_rejected() {
    with_storage(|storage, world| {
        let gems = alice_gems(storage, world, &[1, 2]);
        call_gem(storage, world, gems[0], T_NOW);
        let (first, stale_bucket) = api::load_pair(storage, &world.scope, &world.parent, gems[0])
            .unwrap()
            .unwrap();
        settle(storage, world, gems[1]);
        let error = api::remove_gem(storage, &world.scope, first, stale_bucket).unwrap_err();
        assert!(!format!("{error:?}").is_empty());
        assert!(item(storage, world, gems[0]).is_some(), "rolled back");
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 2);
    });
}

#[test]
fn member_count_overflow_and_underflow_roll_back_gem_mutations() {
    with_storage(|storage, world| {
        let gem_id = add(storage, world, sample_params(ALICE));
        let bucket = bucket_of(storage, world, gem_id);
        let gem = GemContract::new(storage.clone());
        gem.bucket_gem_count.write(&bucket, 0).unwrap();
        call_gem(storage, world, gem_id, T_NOW);
        assert!(try_settle(storage, world, gem_id).is_err());
        assert_eq!(
            item(storage, world, gem_id).unwrap().state,
            GemState::Issued as u8
        );
        assert_eq!(gem.total_supply().unwrap(), 1);
    });
}

#[test]
fn a_gem_id_is_its_raw_keccak_and_the_bucket_id_carries_the_day() {
    with_storage(|storage, world| {
        let gem_id = add(storage, world, sample_params(ALICE));
        assert_eq!(
            gem_id,
            GemContract::generate_gem_id(
                ALICE,
                U256::from(1_000_000u64),
                storage.block_number().unwrap()
            )
        );
        let stored = item(storage, world, gem_id).unwrap();
        assert_eq!(WwdEntityId::from(gem_id), stored.gem_id);
        assert_eq!(
            stored.bucket_id(),
            WwdEntityId::from_day_and_digest(stored.worldwide_day(), stored.bucket_key)
        );
        assert_eq!(stored.worldwide_day().value(), timestamp_to_date_key(T_NOW));
    });
}

/// The e2e arming path re-issues the gem on another day: it leaves its bucket, which
/// closes when it was the only member, and opens the bucket body of the new day.
#[cfg(feature = "e2e-test")]
#[test]
fn backdate_that_changes_the_day_moves_the_gem_and_mints_a_new_bucket_body() {
    use alloy_sol_types::SolCall;
    use outbe_gem::precompile::IGemTestArming;

    with_storage(|storage, world| {
        let gem_id = add(storage, world, sample_params(ALICE));
        let before = item(storage, world, gem_id).unwrap();
        let issued_at = T_NOW - 3 * 86_400;

        call(
            storage,
            world,
            IGemTestArming::backdateGemForTestCall {
                gemId: gem_id,
                issuedAt: issued_at,
            }
            .abi_encode(),
        )
        .unwrap();

        let after = item(storage, world, gem_id).unwrap();
        assert_eq!(after.issued_at, issued_at);
        assert_ne!(after.bucket_key, before.bucket_key);
        assert_eq!(
            after.bucket_key,
            outbe_gem::identity::bucket_key(after.worldwide_day(), U256::from(500_000u64), USD)
        );
        assert!(
            api::get_bucket(storage, &world.scope, &world.parent, before.bucket_id())
                .unwrap()
                .is_none(),
            "the only member left, so the old bucket body is gone"
        );
        let bucket = api::get_bucket(storage, &world.scope, &world.parent, after.bucket_id())
            .unwrap()
            .unwrap();
        assert_eq!(bucket.worldwide_day, after.worldwide_day());
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 1);
        assert_eq!(owned(storage, world, ALICE), vec![gem_id]);
    });
}

/// A throwaway build gets a paid gem out of thin air: the body reads Settled and the
/// bucket counts it as paid, with no unpaid member left to sweep.
#[cfg(feature = "e2e-test")]
#[test]
fn settled_for_test_issues_a_paid_body_and_counts_it_in_the_bucket() {
    with_storage(|storage, world| {
        let gem_id =
            api::issue_settled_for_test(storage, &world.scope, &world.parent, sample_params(ALICE))
                .unwrap();
        let gem = item(storage, world, gem_id).unwrap();
        assert!(gem.is_settled());
        let bucket = api::get_bucket(storage, &world.scope, &world.parent, gem.bucket_id())
            .unwrap()
            .unwrap();
        assert_eq!(bucket.settled_gems, 1);
        let contract = GemContract::new(storage.clone());
        assert_eq!(contract.bucket_gem_count.read(&gem.bucket_key).unwrap(), 0);
        assert_eq!(contract.total_supply().unwrap(), 1);
    });
}
