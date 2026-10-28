//! Issue, group membership, settlement and burn through Gem storage.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::GEM_ADDRESS;
use outbe_primitives::math::constants::REAL_ID_SHIFT;
use outbe_primitives::math::tree_math;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_primitives::time::SECONDS_PER_DAY_U32;
use outbe_primitives::wwd_entity_id::WwdEntityId;

use super::*;
use outbe_gem::config::GemParams;
use outbe_gem::schema::{GemContract, GemState};

#[test]
fn coen_iso_one_maps_to_the_center_price_bin_at_six_decimals() {
    assert_eq!(
        outbe_primitives::call_bins::price_to_bin(U256::from(1_000_000u64)).unwrap(),
        REAL_ID_SHIFT as u32
    );
}

#[test]
fn initial_state_empty() {
    with_storage(|storage| {
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert!(owned(storage, ALICE).is_empty());
    });
}

#[test]
fn add_gem_mints_the_gem_and_its_group_and_bumps_counters() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 1);
        assert_eq!(owned(storage, ALICE), [gem_id]);
        let stored = item(storage, gem_id).unwrap();
        assert_eq!(stored.owner, ALICE);
        assert_eq!(stored.state, GemState::Issued as u8);
        assert_eq!(stored.gem_id.to_u256(), gem_id);
        let gem = view(storage, gem_id);
        assert_eq!(gem.item.entry_price_minor, U256::from(500_000u64));
        assert_eq!(gem.group.settled_gems, 0);
        assert_eq!(gem.terms.call_rate, GemParams::DEV.call_rate);
        assert_eq!(
            gem.terms.call_price_minor,
            U256::from(500_000u64 * (100 + u64::from(GemParams::DEV.call_rate)) / 100)
        );
    });
}

#[test]
fn same_block_issue_is_visible_to_point_and_owner_list_reads() {
    let mut provider = provider_at(T_NOW);
    let gem_id = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = add(&storage, sample_params(ALICE));
        assert_eq!(item(&storage, gem_id).unwrap().owner, ALICE);
        assert_eq!(owned(&storage, ALICE), [gem_id]);
        gem_id
    });
    let signatures: Vec<_> = provider
        .get_events(GEM_ADDRESS)
        .iter()
        .map(|event| event.topics()[0])
        .collect();
    assert_eq!(signatures, [IGem::Transfer::SIGNATURE_HASH]);
    StorageHandle::enter(&mut provider, |storage| {
        let stored = GemContract::new(storage)
            .records
            .get(outbe_primitives::wwd_entity_id::WwdEntityId::from(gem_id))
            .unwrap()
            .unwrap();
        assert_eq!(stored.owner, ALICE);
    });
}

#[test]
fn add_gem_rejects_zero_owner() {
    with_storage(|storage| {
        let mut p = sample_params(ALICE);
        p.owner = Address::ZERO;
        assert!(crate::runtime::add_gem(storage, p).is_err());
    });
}

#[test]
fn same_block_issuances_with_different_currencies_have_distinct_ids() {
    with_storage(|storage| {
        let mut ids = Vec::new();
        for (issuance_currency, reference_currency) in [(USD, USD), (EUR, USD), (USD, EUR)] {
            let mut params = sample_params(ALICE);
            params.issuance_currency = issuance_currency;
            params.reference_currency = reference_currency;
            let gem_id = add(storage, params);
            assert!(!ids.contains(&gem_id));
            let stored = item(storage, gem_id).unwrap();
            assert_eq!(stored.issuance_currency, issuance_currency);
            assert_eq!(stored.reference_currency, reference_currency);
            ids.push(gem_id);
        }
        let mut owned = owned(storage, ALICE);
        owned.sort_unstable();
        ids.sort_unstable();
        assert_eq!(owned, ids);
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 3);
    });
}

#[test]
fn same_block_identical_issuances_have_distinct_ids() {
    with_storage(|storage| {
        let first = add(storage, sample_params(ALICE));
        let second = add(storage, sample_params(ALICE));
        assert_ne!(first, second);
        let owned = owned(storage, ALICE);
        assert_eq!(owned.len(), 2);
        assert!(owned.contains(&first));
        assert!(owned.contains(&second));
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 2);
    });
}

#[test]
fn a_duplicate_id_is_rejected_and_rolls_back() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        let before = view(storage, gem_id);
        let error = api::record_issued(storage, &before.item, Some(before.terms)).unwrap_err();
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::RevertBytes(bytes)
                if bytes.as_ref() == alloy_sol_types::SolError::abi_encode(
                    &IGem::AlreadyExists {}
                ).as_slice()
        ));
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 1);
        assert_eq!(owned(storage, ALICE), [gem_id]);
        assert_eq!(view(storage, gem_id), before);
    });
}

#[test]
fn removal_can_collide_with_a_surviving_same_block_issuance() {
    with_storage(|storage| {
        let first = add(storage, sample_params(ALICE));
        let second = add(storage, sample_params(ALICE));
        settle(storage, first);
        burn_settled(storage, first);
        let before = view(storage, second);
        let error = crate::runtime::add_gem(storage, sample_params(ALICE)).unwrap_err();
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::RevertBytes(bytes)
                if bytes.as_ref() == alloy_sol_types::SolError::abi_encode(
                    &IGem::AlreadyExists {}
                ).as_slice()
        ));
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 1);
        assert_eq!(owned(storage, ALICE), [second]);
        assert!(item(storage, first).is_none());
        assert_eq!(view(storage, second), before);
    });
}

#[test]
fn gem_id_derivation_binds_each_currency_and_live_supply() {
    let derive = |issuance, reference, supply| {
        let mut params = sample_params(ALICE);
        params.issuance_currency = issuance;
        params.reference_currency = reference;
        GemContract::generate_gem_id(&params, 1, supply)
    };
    let baseline = derive(USD, USD, 0);
    assert_eq!(baseline, derive(USD, USD, 0));
    assert_ne!(baseline, derive(EUR, USD, 0));
    assert_ne!(baseline, derive(USD, EUR, 0));
    assert_ne!(baseline, derive(USD, USD, 1));
}

#[test]
fn enumerable_returns_only_owned_gems() {
    with_storage(|storage| {
        let g1 = add(storage, sample_params(ALICE));
        let mut p2 = sample_params(ALICE);
        p2.promis_load_minor = U256::from(2u64);
        let g2 = add(storage, p2);
        let _g3 = add(storage, sample_params(BOB));

        let alice = owned(storage, ALICE);
        assert_eq!(alice.len(), 2);
        assert!(alice.contains(&g1));
        assert!(alice.contains(&g2));
        assert_eq!(owned(storage, BOB).len(), 1);
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 3);
    });
}

#[test]
fn burn_requires_settled_state() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        assert!(crate::runtime::burn_settled(storage, gem_id).is_err());

        settle(storage, gem_id);
        burn_settled(storage, gem_id);

        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert!(owned(storage, ALICE).is_empty());
        assert!(item(storage, gem_id).is_none());
    });
}

#[test]
fn burn_leaves_the_owners_other_gems_listed() {
    with_storage(|storage| {
        let gems = alice_gems(storage, &[1, 2, 3]);
        settle(storage, gems[0]);
        burn_settled(storage, gems[0]);

        let remaining = owned(storage, ALICE);
        assert_eq!(remaining.len(), 2);
        assert!(remaining.contains(&gems[1]));
        assert!(remaining.contains(&gems[2]));
    });
}

/// Gems share a group when they count call days from the same first full day against
/// the same call price in the same reference currency. Nothing else is a term.
#[test]
fn a_group_holds_the_gems_of_one_first_full_day_call_price_and_currency() {
    with_storage(|storage| {
        let gems = alice_gems(storage, &[1, 2]);
        let group = group_of(storage, gems[0]);
        assert!(!group.is_zero());
        assert_eq!(group_of(storage, gems[1]), group, "the load is not a term");
        assert_eq!(
            group,
            outbe_primitives::call_breach::group_key(
                outbe_primitives::time::first_full_day(T_NOW).expect("date step"),
                view(storage, gems[0]).terms.call_price_minor,
                USD
            )
        );

        let nonce = std::cell::Cell::new(0u64);
        let apart = |edit: &dyn Fn(&mut GemAddParams)| {
            let mut params = sample_params(BOB);
            nonce.set(nonce.get() + 1);
            params.promis_load_minor = U256::from(nonce.get());
            edit(&mut params);
            group_of(storage, add(storage, params))
        };
        assert_ne!(
            apart(&|p| p.issued_at += SECONDS_PER_DAY),
            group,
            "another day"
        );
        assert_eq!(apart(&|p| p.issued_at += 1), group, "the same day");
        assert_ne!(apart(&|p| p.reference_currency = EUR), group);
        assert_ne!(apart(&|p| p.entry_price_minor += U256::from(1u64)), group);
        set_profile(storage, outbe_gem::config::PROFILE_PROD);
        assert_ne!(
            apart(&|_| {}),
            group,
            "a retuned call rate prices a new group"
        );
    });
}

/// A gem issued exactly at midnight counts that whole day. One issued later the same
/// day starts the next day, so it never borrows the midnight gem's extra day.
#[test]
fn a_midnight_gem_and_a_later_one_that_day_are_in_different_groups() {
    with_storage(|storage| {
        let midnight = (T_NOW / SECONDS_PER_DAY + 1) * SECONDS_PER_DAY;
        let mut first = sample_params(ALICE);
        first.issued_at = midnight;
        let mut later = sample_params(BOB);
        later.issued_at = midnight + 2;
        let first = add(storage, first);
        let later = add(storage, later);

        assert_ne!(group_of(storage, first), group_of(storage, later));
        let group_day = |gem_id| {
            api::get_group(storage, group_of(storage, gem_id))
                .unwrap()
                .unwrap()
                .full_day_after_issue
                .value()
        };
        assert_eq!(
            group_day(first),
            outbe_primitives::time::timestamp_to_date_key(midnight)
        );
        assert_eq!(
            group_day(later),
            outbe_primitives::time::first_full_day(midnight + 2).expect("date step")
        );
    });
}

/// A retuned call rate opens a new group, and the open one keeps the terms it sealed.
#[test]
fn profile_change_after_open_does_not_reterm_the_group() {
    with_storage(|storage| {
        set_profile(storage, outbe_gem::config::PROFILE_DEV);
        let first = add(storage, sample_params(ALICE));
        set_profile(storage, outbe_gem::config::PROFILE_PROD);
        let mut p = sample_params(BOB);
        p.issued_at += 3_600;
        let second = add(storage, p);

        assert_ne!(group_of(storage, first), group_of(storage, second));
        let terms = view(storage, first).terms;
        assert_eq!(
            terms.call_window_seconds,
            GemParams::DEV.call_window_seconds
        );
        assert_eq!(terms.call_rate, GemParams::DEV.call_rate);
        assert_eq!(
            view(storage, second).terms.call_rate,
            GemParams::PROD.call_rate
        );
    });
}

#[test]
fn leaving_a_group_moves_its_last_member_into_the_hole() {
    with_storage(|storage| {
        let gems = alice_gems(storage, &[1, 2, 3]);
        let group = group_of(storage, gems[0]);
        let gem = GemContract::new(storage.clone());

        settle(storage, gems[0]);
        assert_eq!(gem.group_gem_count.read(&group).unwrap(), 2);
        assert_eq!(
            gem.group_gems
                .read(&GemContract::group_member_key(group, 0))
                .unwrap(),
            gems[2]
        );
        assert_eq!(gem.group_gem_index.read(&gems[2]).unwrap(), 0);
        assert_eq!(gem.group_gem_index.read(&gems[0]).unwrap(), 0);
        assert_eq!(
            item(storage, gems[0]).unwrap().group_key,
            group,
            "a settled gem still names its group"
        );
    });
}

/// The group record and its sealed terms outlive the unpaid membership while a paid
/// member exists; the last removal takes everything down.
#[test]
fn group_and_terms_survive_until_unpaid_and_settled_are_zero() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        let group = group_of(storage, gem_id);
        let group_id = item(storage, gem_id).unwrap().group_key;
        let gem = GemContract::new(storage.clone());
        let bin =
            outbe_primitives::call_bins::price_to_bin(view(storage, gem_id).terms.call_price_minor)
                .unwrap();
        assert!(tree_math::contains(&outbe_gem::CallBins(&gem, USD), bin).unwrap());

        settle(storage, gem_id);
        assert_eq!(gem.group_gem_count.read(&group).unwrap(), 0);
        let body = api::get_group(storage, group_id)
            .unwrap()
            .expect("the paid member keeps the group");
        assert_eq!(body.settled_gems, 1);
        assert!(!gem.group_call_price_minor.read(&group).unwrap().is_zero());
        assert_eq!(
            view(storage, gem_id).item.entry_price_minor,
            U256::from(500_000u64)
        );
        // The group keeps its place in the trie while a paid member holds it open; the
        // scan skips a group without unpaid members.
        assert_ne!(gem.call_group_slot.read(&group).unwrap(), 0);
        let mut scan = GemContract::new(storage.clone());
        let window = breach_window(
            T_NOW,
            view(storage, gem_id).terms.call_price_minor + U256::from(1u64),
            (outbe_gem::constants::CALL_WINDOW / SECONDS_PER_DAY_U32) as usize,
        );
        assert!(!crate::called::call_if_breached(&mut scan, &window, group, T_NOW).unwrap());

        burn_settled(storage, gem_id);
        assert_eq!(
            gem.call_group_slot.read(&group).unwrap(),
            0,
            "out of the trie"
        );
        assert!(!tree_math::contains(&outbe_gem::CallBins(&gem, USD), bin).unwrap());
        assert!(api::get_group(storage, group_id).unwrap().is_none());
        assert!(gem.group_call_price_minor.read(&group).unwrap().is_zero());
        assert!(gem.group_unpaid_load.read(&group).unwrap().is_zero());
    });
}

/// Removing an unpaid gem touches no group body; removing a paid one lowers the count.
#[test]
fn unpaid_removal_preserves_group_state_settled_removal_decrements() {
    let mut provider = provider_at(T_NOW);
    StorageHandle::enter(&mut provider, |storage| {
        let gems = alice_gems(&storage, &[1, 2]);
        call_gem(&storage, gems[0], T_NOW);
        settle(&storage, gems[1]);
        let group_id = item(&storage, gems[1]).unwrap().group_key;
        assert_eq!(
            api::get_group(&storage, group_id)
                .unwrap()
                .unwrap()
                .settled_gems,
            1
        );
        // Forfeit the unpaid one: the group body stays as it is.
        begin_block_at(
            &storage,
            GemContract::day_end(GemContract::deadline_day(
                T_NOW + u64::from(GemParams::DEV.call_notice_period_seconds),
            )),
        );
        assert!(item(&storage, gems[0]).is_none());
        assert_eq!(
            api::get_group(&storage, group_id)
                .unwrap()
                .unwrap()
                .settled_gems,
            1
        );
        burn_settled(&storage, gems[1]);
        assert!(api::get_group(&storage, group_id).unwrap().is_none());
    });
}

#[test]
fn an_issued_gem_settles_once() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        assert_eq!(gem_state(storage, gem_id), GemState::Issued as u8);

        settle(storage, gem_id);
        let settled = item(storage, gem_id).unwrap();
        assert_eq!(settled.state, GemState::Settled as u8);
        assert_eq!(settled.settled_at, T_NOW);
        assert_eq!(settled.called_at, 0);
        assert!(try_settle(storage, gem_id).is_err());
    });
}

/// Settling one gem of a called group keeps its call stamp and leaves the rest called.
#[test]
fn settling_one_gem_of_a_called_group_leaves_the_rest_called() {
    with_storage(|storage| {
        let gems = alice_gems(storage, &[1, 2]);
        call_gem(storage, gems[0], T_NOW);
        assert_eq!(gem_state(storage, gems[1]), GemState::Called as u8);

        settle(storage, gems[0]);
        let settled = item(storage, gems[0]).unwrap();
        assert_eq!(settled.state, GemState::Settled as u8);
        assert_eq!(settled.called_at, T_NOW);
        assert_eq!(gem_state(storage, gems[0]), GemState::Settled as u8);
        assert_eq!(gem_state(storage, gems[1]), GemState::Called as u8);
        let entry = group_of(storage, gems[1]);
        assert_ne!(
            GemContract::new(storage.clone())
                .called_group_slot
                .read(&entry)
                .unwrap(),
            0,
            "the group still waits for its deadline"
        );
    });
}

#[test]
fn leaving_called_frees_the_expiry_slot() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let gem = GemContract::new(storage.clone());
        call_gem(storage, gem_id, T_NOW);
        let day = GemContract::deadline_day(T_NOW + 7 * SECONDS_PER_DAY);
        assert_eq!(gem.forfeit_day_live.read(&day).unwrap(), 1);

        settle(storage, gem_id);
        burn_settled(storage, gem_id);
        assert_eq!(gem.forfeit_day_live.read(&day).unwrap(), 0);
        assert_eq!(gem.first_expiry_day().unwrap(), None);
    });
}

/// A stale group capability (loaded before a sibling settled) is rejected by the
/// generic store, and the gem's own accounting stays untouched.
#[test]
fn stale_group_capability_is_rejected() {
    with_storage(|storage| {
        let gems = alice_gems(storage, &[1, 2]);
        call_gem(storage, gems[0], T_NOW);
        let (first, stale_group) = api::load_pair(storage, gems[0]).unwrap().unwrap();
        settle(storage, gems[1]);
        let error = api::remove_gem(storage, first, stale_group).unwrap_err();
        assert!(!format!("{error:?}").is_empty());
        assert!(item(storage, gems[0]).is_some(), "rolled back");
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 2);
    });
}

#[test]
fn member_count_overflow_and_underflow_roll_back_gem_mutations() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        let group = group_of(storage, gem_id);
        let gem = GemContract::new(storage.clone());
        gem.group_gem_count.write(&group, 0).unwrap();
        call_gem(storage, gem_id, T_NOW);
        assert!(try_settle(storage, gem_id).is_err());
        assert_eq!(item(storage, gem_id).unwrap().state, GemState::Issued as u8);
        assert_eq!(gem.total_supply().unwrap(), 1);
    });
}

#[test]
fn a_gem_id_is_its_raw_keccak_and_the_group_id_carries_the_day() {
    with_storage(|storage| {
        let supply = GemContract::new(storage.clone()).total_supply().unwrap();
        let gem_id = add(storage, sample_params(ALICE));
        assert_eq!(
            gem_id,
            GemContract::generate_gem_id(
                &sample_params(ALICE),
                storage.block_number().unwrap(),
                supply,
            )
        );
        let stored = item(storage, gem_id).unwrap();
        assert_eq!(WwdEntityId::from(gem_id), stored.gem_id);
        assert_eq!(
            stored.group_id().expect("date step"),
            WwdEntityId::from_day_and_digest(
                stored.full_day_after_issue().expect("date step"),
                stored.group_key
            )
        );
        assert_eq!(stored.worldwide_day().value(), timestamp_to_date_key(T_NOW));
    });
}

/// The e2e arming path re-issues the gem on another day: it leaves its group, which
/// closes when it was the only member, and opens the group of the new day.
#[cfg(feature = "e2e-test")]
#[test]
fn backdate_that_changes_the_day_moves_the_gem_and_opens_a_new_group() {
    use crate::test_arming_e2e::IGemFactoryTestArming;
    use alloy_sol_types::SolCall;

    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        let before = item(storage, gem_id).unwrap();
        let issued_at = T_NOW - 3 * SECONDS_PER_DAY;

        let data = IGemFactoryTestArming::backdateGemForTestCall {
            gemId: gem_id,
            issuedAt: issued_at,
        }
        .abi_encode();
        crate::test_arming_e2e::dispatch(storage, &data)
            .unwrap()
            .unwrap();

        let after = item(storage, gem_id).unwrap();
        assert_eq!(after.issued_at, issued_at);
        assert_ne!(after.group_key, before.group_key);
        assert_eq!(
            after.group_key,
            outbe_primitives::call_breach::group_key(
                after.full_day_after_issue().expect("date step").value(),
                view(storage, gem_id).terms.call_price_minor,
                USD
            )
        );
        assert!(
            api::get_group(storage, before.group_key).unwrap().is_none(),
            "the only member left, so the old group body is gone"
        );
        let group = api::get_group(storage, after.group_key).unwrap().unwrap();
        assert_eq!(
            group.full_day_after_issue,
            after.full_day_after_issue().expect("date step")
        );
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 1);
        assert_eq!(owned(storage, ALICE), vec![gem_id]);
    });
}

/// A throwaway build gets a paid gem out of thin air: the body reads Settled and the
/// group counts it as paid, with no unpaid member left to sweep.
#[cfg(feature = "e2e-test")]
#[test]
fn settled_for_test_issues_a_paid_gem_and_counts_it_in_the_group() {
    use crate::test_arming_e2e::IGemFactoryTestArming::issueSettledGemForTestCall;
    use alloy_sol_types::SolCall;

    with_storage(|storage| {
        let params = sample_params(ALICE);
        let data = issueSettledGemForTestCall {
            owner: params.owner,
            promisLoadMinor: params.promis_load_minor,
            currency: params.issuance_currency,
        }
        .abi_encode();
        let out = crate::test_arming_e2e::dispatch(storage, &data)
            .unwrap()
            .unwrap();
        let gem_id = issueSettledGemForTestCall::abi_decode_returns(&out).unwrap();
        let gem = item(storage, gem_id).unwrap();
        assert!(gem.is_settled());
        let group = api::get_group(storage, gem.group_key).unwrap().unwrap();
        assert_eq!(group.settled_gems, 1);
        let contract = GemContract::new(storage.clone());
        assert_eq!(contract.group_gem_count.read(&gem.group_key).unwrap(), 0);
        assert_eq!(contract.total_supply().unwrap(), 1);
    });
}
