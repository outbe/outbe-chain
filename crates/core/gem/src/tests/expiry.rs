//! The forfeit sweep: called buckets whose notice lapsed burn their unpaid gems and
//! return the loads to the Promis reserve, one body at a time under the block budget.

use alloy_primitives::{B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::{GEM_ADDRESS, PROMIS_LIMIT_ADDRESS};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::sweep_budget::{SWEEP_BODY_WRITES_PER_BLOCK, SWEEP_WRITES_PER_BLOCK};
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

use super::*;
use crate::config::GemParams;
use crate::schema::{GemContract, GemState};

/// Unix time of the first block past the deadline of a bucket called at `T_NOW`.
fn first_due_block(storage: &StorageHandle, world: &World, gem_id: U256) -> u64 {
    let notice = view(storage, world, gem_id)
        .terms
        .call_notice_period_seconds;
    GemContract::hour_end(GemContract::deadline_hour(T_NOW + u64::from(notice)))
}

#[test]
fn expiry_waits_until_the_deadline_hour_has_closed() {
    with_storage(|storage, world| {
        let gem_id = mature_gem(storage, world);
        call_gem(storage, world, gem_id, T_NOW);
        let hour_end = GemContract::hour_end(GemContract::deadline_hour(T_NOW + 7 * 86_400));

        begin_block_at(storage, world, hour_end - 1);
        assert_eq!(gem_state(storage, world, gem_id), GemState::Called as u8);
        assert_eq!(unallocated(storage), U256::ZERO);

        begin_block_at(storage, world, hour_end);
        assert!(item(storage, world, gem_id).is_none());
        assert_eq!(unallocated(storage), U256::from(1_000_000));
    });
}

/// The body is deleted and the load goes back to the reserve in one transition.
#[test]
fn forfeit_deletes_the_gem_body_and_returns_promis_load() {
    let (mut provider, world) = provider_at(T_NOW);
    let (gem_id, load) = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = mature_gem(&storage, &world);
        let load = item(&storage, &world, gem_id).unwrap().promis_load_minor;
        call_gem(&storage, &world, gem_id, T_NOW);

        begin_block_at(&storage, &world, T_NOW + 6 * 86_400);
        assert!(
            item(&storage, &world, gem_id).is_some(),
            "inside the notice"
        );
        assert_eq!(unallocated(&storage), U256::ZERO);

        begin_block_at(&storage, &world, first_due_block(&storage, &world, gem_id));
        assert!(item(&storage, &world, gem_id).is_none());
        assert_eq!(unallocated(&storage), load);
        assert_eq!(GemContract::new(storage.clone()).total_supply().unwrap(), 0);
        (gem_id, load)
    });
    let events = provider.get_events(GEM_ADDRESS);
    let deleted = events
        .iter()
        .find_map(|log| IGem::GemBodyDeleted::decode_log_data(log).ok())
        .expect("the body deletion is announced");
    assert_eq!(deleted.gemId, gem_id);
    let expired = events
        .iter()
        .find_map(|log| IGem::GemExpired::decode_log_data(log).ok())
        .unwrap();
    assert_eq!(
        (expired.gemId, expired.owner, expired.promisLoadMinor),
        (gem_id, ALICE, load)
    );
    assert!(events
        .iter()
        .any(|log| IGem::GemBucketBodyDeleted::decode_log_data(log).is_ok()));
}

#[test]
fn forfeiting_a_gem_returns_its_load_to_the_pool() {
    with_storage(|storage, world| {
        let gem_id = mature_gem(storage, world);
        let load = item(storage, world, gem_id).unwrap().promis_load_minor;
        call_gem(storage, world, gem_id, T_NOW);

        begin_block_at(storage, world, T_NOW + 6 * 86_400);
        assert_eq!(unallocated(storage), U256::ZERO);

        begin_block_at(storage, world, first_due_block(storage, world, gem_id));
        assert_eq!(unallocated(storage), load);
    });
}

/// Its owner paid the strike, so the load is theirs.
#[test]
fn a_settled_gem_is_never_forfeited() {
    with_storage(|storage, world| {
        let gem_id = mature_gem(storage, world);
        let bucket = bucket_of(storage, world, gem_id);
        call_gem(storage, world, gem_id, T_NOW);
        settle(storage, world, gem_id);

        begin_block_at(storage, world, first_due_block(storage, world, gem_id));
        assert!(item(storage, world, gem_id).is_some());
        assert_eq!(gem_state(storage, world, gem_id), GemState::Settled as u8);
        assert_eq!(unallocated(storage), U256::ZERO);
        assert_eq!(
            GemContract::new(storage.clone())
                .called_bucket_slot
                .read(&bucket)
                .unwrap(),
            0
        );
    });
}

/// An entry that the sweep cannot retire credits nothing and leaves its bucket, so
/// the gems behind it still drain. The burn and the credit share a checkpoint.
#[test]
fn an_entry_the_sweep_cannot_retire_does_not_hold_up_its_bucket() {
    with_storage(|storage, world| {
        let live = mature_gem(storage, world);
        let mut gem = GemContract::new(storage.clone());
        // A slot naming a bucket that is not there.
        let ghost = B256::repeat_byte(0xde);
        let deadline = T_NOW + 7 * 86_400;
        gem.push_called(ghost, deadline).unwrap();
        call_gem(storage, world, live, T_NOW);
        let day = GemContract::deadline_hour(deadline);
        let load = item(storage, world, live).unwrap().promis_load_minor;

        begin_block_at(storage, world, GemContract::hour_end(day));

        assert_eq!(gem.expiry_slot(day, 0).unwrap(), None, "the ghost is out");
        assert_eq!(
            unallocated(storage),
            load,
            "and the gem behind it was still forfeited"
        );
    });
}

/// Same for a due bucket that was never called: it burns nothing.
#[test]
fn a_due_entry_that_cannot_burn_credits_nothing() {
    with_storage(|storage, world| {
        let gem_id = mature_gem(storage, world);
        let mut gem = GemContract::new(storage.clone());
        gem.push_called(bucket_of(storage, world, gem_id), T_NOW)
            .unwrap();

        begin_block_at(
            storage,
            world,
            GemContract::hour_end(GemContract::deadline_hour(T_NOW)),
        );

        assert_eq!(unallocated(storage), U256::ZERO);
        assert!(item(storage, world, gem_id).is_some());
    });
}

/// Expiry reads the queue, not the tree: the reason the two stages are separate.
#[test]
fn a_gem_above_the_window_is_not_visited_but_still_expires() {
    with_storage(|storage, world| {
        let gem_id = mature_gem(storage, world);
        let call_price = view(storage, world, gem_id).terms.call_price_minor;

        // Every published day sits below the gem's call price.
        let pair = seed_currency(storage, USD, Some(U256::from(600_000u64)));
        let last_closed_day = previous_date_key(timestamp_to_date_key(T_NOW));
        priced_window(
            storage,
            pair,
            last_closed_day,
            call_price - U256::from(1u64),
        );

        assert_eq!(
            crate::called::scan_and_call(&block_ctx(storage)).unwrap(),
            0
        );
        assert_eq!(gem_state(storage, world, gem_id), GemState::Issued as u8);

        call_gem(storage, world, gem_id, T_NOW);
        begin_block_at(storage, world, first_due_block(storage, world, gem_id));
        assert!(item(storage, world, gem_id).is_none());
    });
}

#[test]
fn a_bucket_that_outlives_its_hour_is_retired_rather_than_left_in_front() {
    with_storage(|storage, world| {
        let gem_id = mature_gem(storage, world);
        let gem = GemContract::new(storage.clone());
        call_gem(storage, world, gem_id, T_NOW);
        let deadline = T_NOW + 7 * 86_400;
        let hour = GemContract::deadline_hour(deadline);

        let entry = bucket_of(storage, world, gem_id);
        gem.called_deadline
            .write(&entry, deadline + 400 * 86_400)
            .unwrap();

        begin_block_at(storage, world, GemContract::hour_end(hour));

        let retry = GemContract::deadline_hour(deadline + 400 * 86_400);
        assert_eq!(
            gem.first_expiry_day().unwrap(),
            Some(retry),
            "the bucket moves to its deadline's hour instead of blocking every later one"
        );

        begin_block_at(storage, world, GemContract::hour_end(retry));
        assert!(item(storage, world, gem_id).is_none());
    });
}

/// Calls a mature gem, fails its first forfeit, and checks its bucket moved to the
/// next hour and burned there with its credit. Returns the provider for its events.
fn forfeit_fails_once() -> (
    outbe_primitives::storage::hashmap::HashMapStorageProvider,
    U256,
) {
    let (mut provider, world) = provider_at(T_NOW);
    let (gem_id, load, retry) = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = mature_gem(&storage, &world);
        let load = item(&storage, &world, gem_id).unwrap().promis_load_minor;
        call_gem(&storage, &world, gem_id, T_NOW);
        let hour = GemContract::deadline_hour(T_NOW + 7 * 86_400);
        // The credit overflows, so the forfeit reverts as a whole.
        let mut limit = outbe_promislimit::PromisLimitContract::new(storage.clone());
        limit.checked_take_carry_over_up_to(U256::MAX).unwrap();
        limit.checked_add_carry_over(U256::MAX).unwrap();

        let now = GemContract::hour_end(hour);
        begin_block_at(&storage, &world, now);

        let retry = GemContract::deadline_hour(now) + 1;
        assert_eq!(gem_state(&storage, &world, gem_id), GemState::Called as u8);
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
            .checked_take_carry_over_up_to(U256::MAX)
            .unwrap();
        begin_block_at(&storage, &world, GemContract::hour_end(retry));

        assert!(item(&storage, &world, gem_id).is_none());
        assert_eq!(unallocated(&storage), load);
    });
    (provider, gem_id)
}

/// A gem whose forfeit fails is not lost: its bucket moves to the next hour with it,
/// and it burns there.
#[test]
fn a_bucket_whose_member_fails_to_burn_is_deferred_and_burned_later() {
    let (provider, _) = forfeit_fails_once();
    let deferred = provider
        .get_events(GEM_ADDRESS)
        .iter()
        .filter(|log| IGem::ExpiryDeferred::decode_log_data(log).is_ok())
        .count();
    assert_eq!(deferred, 1);
}

/// One member that cannot burn stays in its bucket, deferred. The others burn on time.
#[test]
fn a_member_that_cannot_burn_does_not_hold_back_its_bucket() {
    with_storage(|storage, world| {
        let gems = alice_gems(storage, world, &[1, 1_000]);
        call_gem(storage, world, gems[0], T_NOW);
        let now = first_due_block(storage, world, gems[0]);
        // Room for the small load's credit only.
        let mut limit = outbe_promislimit::PromisLimitContract::new(storage.clone());
        limit.checked_take_carry_over_up_to(U256::MAX).unwrap();
        limit
            .checked_add_carry_over(U256::MAX - U256::from(500u64))
            .unwrap();

        // The failing member sits on top, where the sweep starts.
        begin_block_at(storage, world, now);
        assert!(item(storage, world, gems[0]).is_none(), "burned");
        assert_eq!(gem_state(storage, world, gems[1]), GemState::Called as u8);
        assert_eq!(
            GemContract::new(storage.clone())
                .bucket_gem_count
                .read(&bucket_of(storage, world, gems[1]))
                .unwrap(),
            1,
            "it stays in its bucket"
        );

        limit.checked_take_carry_over_up_to(U256::MAX).unwrap();
        begin_block_at(
            storage,
            world,
            GemContract::hour_end(GemContract::deadline_hour(now) + 1),
        );
        assert!(item(storage, world, gems[1]).is_none());
        assert_eq!(unallocated(storage), U256::from(1_000u64));
    });
}

/// A broken bucket index reverts, so the sweep defers the bucket instead of failing
/// every block.
#[test]
fn a_broken_bucket_index_defers_the_bucket_and_keeps_blocks_going() {
    let (mut provider, world) = provider_at(T_NOW);
    let bucket = StorageHandle::enter(&mut provider, |storage| {
        let gems = alice_gems(&storage, &world, &[1, 2]);
        call_gem(&storage, &world, gems[0], T_NOW);
        let bucket = bucket_of(&storage, &world, gems[0]);
        GemContract::new(storage.clone())
            .bucket_gem_index
            .write(&gems[1], 7)
            .unwrap();
        begin_block_at(&storage, &world, first_due_block(&storage, &world, gems[0]));
        assert_eq!(gem_state(&storage, &world, gems[1]), GemState::Called as u8);
        bucket
    });
    let deferred: Vec<_> = provider
        .get_events(GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::ExpiryDeferred::decode_log_data(log).ok())
        .map(|event| event.bucketKey)
        .collect();
    assert_eq!(deferred, vec![bucket]);
}

/// A called bucket wider than one block's budget burns over several blocks, then
/// leaves the queue.
#[test]
fn a_called_bucket_wider_than_the_budget_burns_over_several_blocks() {
    with_storage(|storage, world| {
        let loads: Vec<u64> = (0..=u64::from(SWEEP_WRITES_PER_BLOCK))
            .map(|n| 1_000 + n)
            .collect();
        let gems = alice_gems(storage, world, &loads);
        call_gem(storage, world, gems[0], T_NOW);
        let now = first_due_block(storage, world, gems[0]);
        let live = || {
            gems.iter()
                .filter(|id| item(storage, world, **id).is_some())
                .count()
        };

        begin_block_at(storage, world, now);
        assert_eq!(
            live(),
            gems.len() - SWEEP_BODY_WRITES_PER_BLOCK as usize,
            "one block burns its body budget"
        );
        let mut blocks = 1;
        while live() > 0 {
            assert!(
                blocks < 16,
                "the queue drains within a bounded number of blocks"
            );
            begin_block_at(storage, world, now + blocks);
            blocks += 1;
        }
        assert_eq!(live(), 0);
        assert_eq!(
            GemContract::new(storage.clone())
                .first_expiry_day()
                .unwrap(),
            None
        );
        assert_eq!(unallocated(storage), U256::from(loads.iter().sum::<u64>()));
    });
}

/// A storage fault is this node's own: it fails the block instead of moving the gem.
#[test]
fn a_storage_fault_in_a_forfeit_fails_the_block() {
    let (mut provider, world) = provider_at(T_NOW);
    let (gem_id, now) = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = mature_gem(&storage, &world);
        call_gem(&storage, &world, gem_id, T_NOW);
        let now = GemContract::hour_end(GemContract::deadline_hour(T_NOW + 7 * 86_400));
        (gem_id, now)
    });

    provider.fail_mutation_at_address(PROMIS_LIMIT_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = block_ctx_at(&storage, now);
        let error = continue_sweeps(&ctx, &world).unwrap_err();
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::Storage(_)
        ));
        assert_eq!(gem_state(&storage, &world, gem_id), GemState::Called as u8);
    });
}

/// The DEV notice is the one a DEV-profile bucket forfeits on.
#[test]
fn config_dev_profile_terms_a_new_bucket() {
    with_storage(|storage, world| {
        set_profile(storage, crate::config::PROFILE_DEV);
        let gem_id = add(storage, world, sample_params(ALICE));
        let terms = view(storage, world, gem_id).terms;
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
