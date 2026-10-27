//! The forfeit sweep: a called group whose notice lapsed returns its unpaid load to the
//! Promis reserve in one credit, and the cleanup deletes its unpaid gems under the
//! block budget.

use alloy_primitives::{B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::{GEM_ADDRESS, GEM_FACTORY_ADDRESS, PROMIS_LIMIT_ADDRESS};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::sweep_budget::SWEEP_WRITES_PER_BLOCK;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

use super::*;
use outbe_gem::config::GemParams;
use outbe_gem::schema::{GemContract, GemState};

/// Unix time of the first block past the deadline of a group called at `T_NOW`.
fn first_due_block(storage: &StorageHandle, gem_id: U256) -> u64 {
    let notice = view(storage, gem_id).terms.call_notice_period_seconds;
    GemContract::day_end(GemContract::deadline_day(T_NOW + u64::from(notice)))
}

#[test]
fn expiry_waits_until_the_deadline_day_has_closed() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        call_gem(storage, gem_id, T_NOW);
        let day_end = GemContract::day_end(GemContract::deadline_day(T_NOW + 7 * SECONDS_PER_DAY));

        begin_block_at(storage, day_end - 1);
        assert_eq!(gem_state(storage, gem_id), GemState::Called as u8);
        assert_eq!(unallocated(storage), U256::ZERO);

        begin_block_at(storage, day_end);
        assert!(item(storage, gem_id).is_none());
        assert_eq!(unallocated(storage), U256::from(1_000_000));
    });
}

/// The gem is deleted and the load goes back to the reserve in one transition.
#[test]
fn forfeit_deletes_the_gem_and_returns_promis_load() {
    let mut provider = provider_at(T_NOW);
    let (gem_id, load) = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = mature_gem(&storage);
        let load = item(&storage, gem_id).unwrap().promis_load_minor;
        call_gem(&storage, gem_id, T_NOW);

        begin_block_at(&storage, T_NOW + 6 * SECONDS_PER_DAY);
        assert!(item(&storage, gem_id).is_some(), "inside the notice");
        assert_eq!(unallocated(&storage), U256::ZERO);

        begin_block_at(&storage, first_due_block(&storage, gem_id));
        assert!(item(&storage, gem_id).is_none());
        assert_eq!(unallocated(&storage), load);
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 0);
        (gem_id, load)
    });
    let events = provider.get_events(GEM_ADDRESS);
    let burned = events
        .iter()
        .find_map(|log| {
            IGem::Transfer::decode_log_data(log)
                .ok()
                .filter(|event| event.to.is_zero())
        })
        .expect("the burn is announced");
    assert_eq!((burned.from, burned.tokenId), (ALICE, gem_id));
    let forfeited = events
        .iter()
        .find_map(|log| IGem::GemGroupForfeited::decode_log_data(log).ok())
        .unwrap();
    assert_eq!((forfeited.gems, forfeited.promisLoadMinor), (1, load));
}

#[test]
fn forfeiting_a_gem_returns_its_load_to_the_pool() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let load = item(storage, gem_id).unwrap().promis_load_minor;
        call_gem(storage, gem_id, T_NOW);

        begin_block_at(storage, T_NOW + 6 * SECONDS_PER_DAY);
        assert_eq!(unallocated(storage), U256::ZERO);

        begin_block_at(storage, first_due_block(storage, gem_id));
        assert_eq!(unallocated(storage), load);
    });
}

/// Its owner paid the strike, so the load is theirs.
#[test]
fn a_settled_gem_is_never_forfeited() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let group = group_of(storage, gem_id);
        call_gem(storage, gem_id, T_NOW);
        settle(storage, gem_id);

        begin_block_at(storage, first_due_block(storage, gem_id));
        assert!(item(storage, gem_id).is_some());
        assert_eq!(gem_state(storage, gem_id), GemState::Settled as u8);
        assert_eq!(unallocated(storage), U256::ZERO);
        assert_eq!(
            GemContract::new(storage.clone())
                .called_group_slot
                .read(&group)
                .unwrap(),
            0
        );
    });
}

/// An entry that the sweep cannot retire credits nothing and leaves its group, so
/// the gems behind it still drain. The burn and the credit share a checkpoint.
#[test]
fn an_entry_the_sweep_cannot_retire_does_not_hold_up_its_group() {
    with_storage(|storage| {
        let live = mature_gem(storage);
        let mut gem = GemContract::new(storage.clone());
        // A slot naming a group that is not there.
        let ghost = B256::repeat_byte(0xde);
        let deadline = T_NOW + 7 * SECONDS_PER_DAY;
        gem.push_called(ghost, deadline).unwrap();
        call_gem(storage, live, T_NOW);
        let day = GemContract::deadline_day(deadline);
        let load = item(storage, live).unwrap().promis_load_minor;

        begin_block_at(storage, GemContract::day_end(day));

        assert_eq!(gem.expiry_slot(day, 0).unwrap(), None, "the ghost is out");
        assert_eq!(
            unallocated(storage),
            load,
            "and the gem behind it was still forfeited"
        );
    });
}

/// Same for a due group that was never called: it burns nothing.
#[test]
fn a_due_entry_that_cannot_burn_credits_nothing() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let mut gem = GemContract::new(storage.clone());
        gem.push_called(group_of(storage, gem_id), T_NOW).unwrap();

        begin_block_at(
            storage,
            GemContract::day_end(GemContract::deadline_day(T_NOW)),
        );

        assert_eq!(unallocated(storage), U256::ZERO);
        assert!(item(storage, gem_id).is_some());
    });
}

/// Expiry reads the queue, not the tree: the reason the two stages are separate.
#[test]
fn a_gem_above_the_window_is_not_visited_but_still_expires() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let call_price = view(storage, gem_id).terms.call_price_minor;

        // Every published day sits below the gem's call price.
        let pair = seed_currency(storage, USD, Some(U256::from(600_000u64)));
        let last_closed_day = previous_date_key(timestamp_to_date_key(T_NOW)).expect("date step");
        priced_window(
            storage,
            pair,
            last_closed_day,
            call_price - U256::from(1u64),
        );

        assert_eq!(scan_and_call(&block_ctx(storage)).unwrap(), 0);
        assert_eq!(gem_state(storage, gem_id), GemState::Issued as u8);

        call_gem(storage, gem_id, T_NOW);
        begin_block_at(storage, first_due_block(storage, gem_id));
        assert!(item(storage, gem_id).is_none());
    });
}

#[test]
fn a_group_that_outlives_its_day_is_retired_rather_than_left_in_front() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        let gem = GemContract::new(storage.clone());
        call_gem(storage, gem_id, T_NOW);
        let deadline = T_NOW + 7 * SECONDS_PER_DAY;
        let day = GemContract::deadline_day(deadline);

        let entry = group_of(storage, gem_id);
        gem.called_deadline
            .write(&entry, deadline + 400 * SECONDS_PER_DAY)
            .unwrap();

        begin_block_at(storage, GemContract::day_end(day));

        let retry = GemContract::deadline_day(deadline + 400 * SECONDS_PER_DAY);
        assert_eq!(
            gem.first_expiry_day().unwrap(),
            Some(retry),
            "the group moves to its deadline's day instead of blocking every later one"
        );

        begin_block_at(storage, GemContract::day_end(retry));
        assert!(item(storage, gem_id).is_none());
    });
}

/// This helper calls a mature gem and causes its first forfeit to fail.
/// It checks that the group moves to the next day and that the gem burns there with its credit.
/// It returns the provider for access to its events.
fn forfeit_fails_once() -> (
    outbe_primitives::storage::hashmap::HashMapStorageProvider,
    U256,
) {
    let mut provider = provider_at(T_NOW);
    let (gem_id, load, retry) = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = mature_gem(&storage);
        let load = item(&storage, gem_id).unwrap().promis_load_minor;
        call_gem(&storage, gem_id, T_NOW);
        let day = GemContract::deadline_day(T_NOW + 7 * SECONDS_PER_DAY);
        // The credit overflows, so the forfeit reverts as a whole.
        let mut limit = outbe_promislimit::PromisLimitContract::new(storage.clone());
        limit.checked_take_up_to(U256::MAX).unwrap();
        limit.checked_credit(U256::MAX).unwrap();

        let now = GemContract::day_end(day);
        begin_block_at(&storage, now);

        let retry = GemContract::deadline_day(now);
        assert_eq!(gem_state(&storage, gem_id), GemState::Called as u8);
        assert_eq!(
            GemContract::new(storage.clone())
                .first_expiry_day()
                .unwrap(),
            Some(retry)
        );
        (gem_id, load, retry)
    });

    StorageHandle::enter(&mut provider, |storage| {
        outbe_promislimit::PromisLimitContract::new(storage.clone())
            .checked_take_up_to(U256::MAX)
            .unwrap();
        begin_block_at(&storage, GemContract::day_end(retry));

        assert!(item(&storage, gem_id).is_none());
        assert_eq!(unallocated(&storage), load);
    });
    (provider, gem_id)
}

/// A gem whose forfeit fails remains in its group.
/// The group moves to the next day, when the gem burns.
#[test]
fn a_group_whose_member_fails_to_burn_is_deferred_and_burned_later() {
    let (provider, _) = forfeit_fails_once();
    let deferred = provider
        .get_events(GEM_FACTORY_ADDRESS)
        .iter()
        .filter(|log| crate::precompile::IGemFactory::ExpiryDeferred::decode_log_data(log).is_ok())
        .count();
    assert_eq!(deferred, 1);
}

/// A group whose credit fails is deferred whole: none of its gems is credited or
/// deleted until the credit fits, and then all of them at once.
#[test]
fn a_group_whose_credit_fails_is_deferred_whole() {
    with_storage(|storage| {
        let gems = alice_gems(storage, &[1, 1_000]);
        call_gem(storage, gems[0], T_NOW);
        let now = first_due_block(storage, gems[0]);
        // Room for the small load's credit only, not the group's.
        let mut limit = outbe_promislimit::PromisLimitContract::new(storage.clone());
        limit.checked_take_up_to(U256::MAX).unwrap();
        limit
            .checked_credit(U256::MAX - U256::from(500u64))
            .unwrap();

        begin_block_at(storage, now);
        for gem_id in &gems {
            assert_eq!(gem_state(storage, *gem_id), GemState::Called as u8);
        }

        limit.checked_take_up_to(U256::MAX).unwrap();
        begin_block_at(
            storage,
            GemContract::day_end(GemContract::deadline_day(now)),
        );
        assert!(gems.iter().all(|gem_id| item(storage, *gem_id).is_none()));
        assert_eq!(unallocated(storage), U256::from(1_001u64));
    });
}

/// A broken group index cannot stop the forfeit, which reads only the group's totals.
/// The cleanup keeps the gem it cannot delete and tries it again later.
#[test]
fn a_broken_group_index_holds_back_only_the_cleanup() {
    let mut provider = provider_at(T_NOW);
    StorageHandle::enter(&mut provider, |storage| {
        let gems = alice_gems(&storage, &[1, 2]);
        call_gem(&storage, gems[0], T_NOW);
        GemContract::new(storage.clone())
            .group_gem_index
            .write(&gems[1], 7)
            .unwrap();
        begin_block_at(&storage, first_due_block(&storage, gems[0]));
        assert_eq!(unallocated(&storage), U256::from(3u64));
        assert!(item(&storage, gems[1]).is_some());
        assert!(GemContract::new(storage.clone())
            .first_cleanup_day()
            .unwrap()
            .is_some());
    });
    assert!(provider
        .get_events(GEM_FACTORY_ADDRESS)
        .iter()
        .all(|log| crate::precompile::IGemFactory::ExpiryDeferred::decode_log_data(log).is_err()));
}

/// A called group wider than one block's budget is credited at once, and its gems are
/// deleted over the next blocks.
#[test]
fn a_called_group_wider_than_the_budget_is_cleaned_up_over_several_blocks() {
    with_storage(|storage| {
        let loads: Vec<u64> = (0..=u64::from(SWEEP_WRITES_PER_BLOCK))
            .map(|n| 1_000 + n)
            .collect();
        let gems = alice_gems(storage, &loads);
        call_gem(storage, gems[0], T_NOW);
        let now = first_due_block(storage, gems[0]);
        let live = || {
            gems.iter()
                .filter(|id| item(storage, **id).is_some())
                .count()
        };

        begin_block_at(storage, now);
        assert_eq!(unallocated(storage), U256::from(loads.iter().sum::<u64>()));
        assert_eq!(
            live(),
            gems.len() - SWEEP_WRITES_PER_BLOCK as usize,
            "one block deletes its write budget"
        );
        begin_block_at(storage, now);
        assert_eq!(
            live(),
            gems.len() - SWEEP_WRITES_PER_BLOCK as usize,
            "same-height cleanup cannot replenish its independent allowance"
        );
        begin_block_at(storage, now + 1);
        assert_eq!(live(), 0);
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.first_expiry_day().unwrap(), None);
        assert_eq!(gem.first_cleanup_day().unwrap(), None);
        assert_eq!(unallocated(storage), U256::from(loads.iter().sum::<u64>()));
    });
}

/// A storage fault is this node's own: it fails the block instead of moving the gem.
#[test]
fn a_storage_fault_in_a_forfeit_fails_the_block() {
    let mut provider = provider_at(T_NOW);
    let (gem_id, now) = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = mature_gem(&storage);
        call_gem(&storage, gem_id, T_NOW);
        let now = GemContract::day_end(GemContract::deadline_day(T_NOW + 7 * SECONDS_PER_DAY));
        (gem_id, now)
    });

    provider.fail_mutation_at_address(PROMIS_LIMIT_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = block_ctx_at(&storage, now);
        let error = continue_sweeps(&ctx).unwrap_err();
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::Storage(_)
        ));
        assert_eq!(gem_state(&storage, gem_id), GemState::Called as u8);
    });
}

/// The DEV notice is the one a DEV-profile group forfeits on.
#[test]
fn config_dev_profile_terms_a_new_group() {
    with_storage(|storage| {
        set_profile(storage, outbe_gem::config::PROFILE_DEV);
        let gem_id = add(storage, sample_params(ALICE));
        let terms = view(storage, gem_id).terms;
        assert_eq!(
            terms.call_window_seconds,
            GemParams::DEV.call_window_seconds
        );
        assert_eq!(
            terms.call_threshold_seconds,
            GemParams::DEV.call_threshold_seconds
        );
        assert_eq!(
            terms.call_notice_period_seconds,
            GemParams::DEV.call_notice_period_seconds
        );
        assert!(terms.call_window_seconds < GemParams::PROD.call_window_seconds);
    });
}

/// A corrupt gem record read is this node's store: the forfeit sweep fails the block.
#[test]
fn a_corrupt_record_read_fails_the_forfeit_sweep() {
    use outbe_primitives::error::{PrecompileError, SweepFailure};
    use outbe_primitives::deadline_queue::DeadlineHandler;
    with_storage(|storage| {
        let ctx = block_ctx(storage);
        let expiry = crate::expired::GemExpiry::new(&ctx);
        assert_eq!(
            expiry.classify(&PrecompileError::BodyReadCorruption("x".into())),
            SweepFailure::Propagate
        );
    });
}
