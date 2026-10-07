use super::*;
use alloy_primitives::{address, b256, Bytes, B256};
use outbe_gemfactory::GemTypes;
use outbe_primitives::addresses::REWARDS_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::time::date_key_to_utc_timestamp;

use crate::finalized_metadata_hook::on_finalized_metadata;
use crate::runtime;

const CHAIN_ID: u64 = 1;
const GENESIS_TS: u64 = 1_704_067_200; // 2024-01-01 UTC

const VAL_X: Address = address!("0x00000000000000000000000000000000000000A1");
const VAL_Y: Address = address!("0x00000000000000000000000000000000000000B2");
const VAL_Z: Address = address!("0x00000000000000000000000000000000000000C3");

const FB_HASH_A: B256 = b256!("0x1111111111111111111111111111111111111111111111111111111111111111");
const FB_HASH_B: B256 = b256!("0x2222222222222222222222222222222222222222222222222222222222222222");

fn block_ctx(block_number: u64, timestamp: u64) -> BlockContext {
    BlockContext::new(block_number, timestamp, CHAIN_ID, Address::ZERO, Vec::new())
}

fn meta_with_hash(fb_hash: B256, fb_number: u64) -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata {
        finalized_block_number: fb_number,
        finalized_block_hash: fb_hash,
        finalized_epoch: 1,
        finalized_view: 1,
        parent_view: 0,
        ordered_committee: vec![],
        signer_bitmap: vec![],
        proof: Bytes::new(),
        committee_set_hash: B256::ZERO,
        vrf_material_version: 0,
        vrf_group_public_key_hash: B256::ZERO,
        proof_kind: outbe_primitives::consensus_metadata::ParentParticipationProof::Finalization,
        missed_proposers: vec![],
    }
}

fn bootstrap_genesis(ctx: &BlockRuntimeContext) {
    runtime::ensure_genesis_anchor(ctx).unwrap();
}

fn fund_rewards(ctx: &BlockRuntimeContext, amount: U256) {
    ctx.storage
        .increase_balance(REWARDS_ADDRESS, amount)
        .unwrap();
}

/// Seeds COEN/840 oracle pair at `rate_6`. This is necessary because
/// `deliver_oldest_reward_gem_batch` -> `issue_gem` resolves `coen_rate` for floor
/// price + entry_price at mint time.
fn seed_oracle(ctx: &BlockRuntimeContext, rate_6: U256) {
    outbe_oracle::api::register_pair(ctx.storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
        .unwrap();
    outbe_oracle::api::set_exchange_rate(
        ctx.storage.clone(),
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        rate_6,
        ctx.block.block_number,
        ctx.block.timestamp,
    )
    .unwrap();
    // Register ISO 840 (USD) so issue_gem currency-validation passes.
    let oracle = outbe_oracle::schema::OracleContract::new(ctx.storage.clone());
    oracle.reference_currencies.push(840u16).unwrap();
    let (_, index) = outbe_oracle::api::require_coen_pair(ctx.storage.clone(), 840).unwrap();
    let day = outbe_primitives::time::previous_date_key(
        outbe_primitives::time::timestamp_to_date_key(ctx.block.timestamp),
    );
    oracle.record_utc_day_vwap(day, index, rate_6).unwrap();
}

fn one_coen840() -> U256 {
    U256::from(1_000_000u64)
}

/// Collects all gem loads owned by `voter` from the gem entity store.
/// Returns empty Vec if voter holds no gems.
fn voter_promis_loads(ctx: &BlockRuntimeContext, voter: Address) -> Vec<U256> {
    let gem = outbe_gem::GemContract::new(ctx.storage.clone());
    let count = gem.balance_of(voter).unwrap();
    (0..count)
        .map(|i| {
            let gem_id = gem.token_of_owner_by_index(voter, i).unwrap();
            outbe_gem::api::get_gem(&ctx.storage, gem_id)
                .unwrap()
                .unwrap()
                .promis_load_minor
        })
        .collect()
}

#[test]
fn read_daily_fee_sum_raw_returns_zero_when_unrecorded() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);

        assert_eq!(read_daily_fee_sum_raw(&ctx, 20240101).unwrap(), U256::ZERO);
    });
}

#[test]
fn read_daily_fee_sum_raw_round_trips_after_finalized_metadata() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        fund_rewards(&ctx, U256::from(300u64));

        on_finalized_metadata(
            &ctx,
            &meta_with_hash(FB_HASH_A, 1),
            U256::from(101u64),
            GENESIS_TS,
            &[VAL_X, VAL_Y],
        )
        .unwrap();
        on_finalized_metadata(
            &ctx,
            &meta_with_hash(FB_HASH_B, 2),
            U256::from(199u64),
            GENESIS_TS,
            &[VAL_X, VAL_Y],
        )
        .unwrap();

        // 101 + 199 = 300 raw.
        assert_eq!(
            read_daily_fee_sum_raw(&ctx, 20240101).unwrap(),
            U256::from(300u64)
        );
        // Untouched day stays zero.
        assert_eq!(read_daily_fee_sum_raw(&ctx, 20240102).unwrap(), U256::ZERO);
    });
}

#[test]
fn read_voters_for_day_orders_by_first_seen() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        fund_rewards(&ctx, U256::from(400u64));

        // FB_HASH_A: Y first, then X (first-seen-on-day = Y, X)
        on_finalized_metadata(
            &ctx,
            &meta_with_hash(FB_HASH_A, 1),
            U256::from(100u64),
            GENESIS_TS,
            &[VAL_Y, VAL_X],
        )
        .unwrap();
        // FB_HASH_B (same day): Z is new, X already seen
        on_finalized_metadata(
            &ctx,
            &meta_with_hash(FB_HASH_B, 2),
            U256::from(100u64),
            GENESIS_TS,
            &[VAL_X, VAL_Z],
        )
        .unwrap();

        let voters = read_voters_for_day(&ctx, 20240101).unwrap();
        // First-seen order: Y (block A), X (block A), Z (block B).
        assert_eq!(voters.len(), 3);
        assert_eq!(voters[0].0, VAL_Y);
        assert_eq!(voters[0].1, 1);
        assert_eq!(voters[1].0, VAL_X);
        assert_eq!(voters[1].1, 2); // X participated in both A and B
        assert_eq!(voters[2].0, VAL_Z);
        assert_eq!(voters[2].1, 1);
    });
}

/// Two entries for one validator would collide on the reward Gem's id.
#[test]
fn a_voter_is_listed_once_however_often_they_vote() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        fund_rewards(&ctx, U256::from(400u64));

        // Twice inside one block, then again in the next one.
        on_finalized_metadata(
            &ctx,
            &meta_with_hash(FB_HASH_A, 1),
            U256::from(100u64),
            GENESIS_TS,
            &[VAL_X, VAL_X],
        )
        .unwrap();
        on_finalized_metadata(
            &ctx,
            &meta_with_hash(FB_HASH_B, 2),
            U256::from(100u64),
            GENESIS_TS,
            &[VAL_X],
        )
        .unwrap();

        let voters = read_voters_for_day(&ctx, 20240101).unwrap();
        assert_eq!(voters.len(), 1, "one entry per voter");
        assert_eq!(voters[0].0, VAL_X);
        assert_eq!(voters[0].1, 2, "a repeat inside one block is not a vote");
    });
}

#[test]
fn read_voters_for_day_empty_when_no_metadata() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);

        let voters = read_voters_for_day(&ctx, 20240101).unwrap();
        assert!(voters.is_empty());
    });
}

#[test]
fn prepare_daily_validator_gem_batch_stores_exact_shares_without_minting() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        seed_oracle(&ctx, U256::from(2u64) * one_coen840());

        // counts 1 + 3 = 4; topup 400 -> VAL_X 100, VAL_Y 300.
        let voters = vec![(VAL_X, 1u64), (VAL_Y, 3u64)];
        let outcome =
            prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(400u64), &voters).unwrap();
        let RewardGemPreparationOutcome::Prepared(batch) = outcome else {
            panic!("fresh day must prepare one batch: {outcome:?}");
        };
        assert_eq!(batch.reward_utc_day, 20240101);
        assert_eq!(batch.planned_promis_load_amount, U256::from(400u64));
        assert_eq!(batch.recipient_count, 2);

        assert!(voter_promis_loads(&ctx, VAL_X).is_empty());
        assert!(voter_promis_loads(&ctx, VAL_Y).is_empty());

        let rewards = ctx.storage.contract::<Rewards>();
        assert!(rewards.daily_topup_prepared.read(&20240101).unwrap());
        assert!(!rewards.daily_topup_settled.read(&20240101).unwrap());
        assert_eq!(rewards.reward_gem_queue_head.read().unwrap(), 0);
        assert_eq!(rewards.reward_gem_queue_tail.read().unwrap(), 1);
        assert_eq!(
            rewards.reward_gem_utc_day_by_sequence.read(&0).unwrap(),
            20240101
        );
        assert_eq!(
            rewards.reward_gem_recipient_count.read(&20240101).unwrap(),
            2
        );
        assert_eq!(
            rewards
                .reward_gem_owner_at
                .get_nested(&20240101)
                .read(&0)
                .unwrap(),
            VAL_X
        );
        assert_eq!(
            rewards
                .reward_promis_load_at
                .get_nested(&20240101)
                .read(&0)
                .unwrap(),
            U256::from(100u64)
        );
        assert_eq!(
            rewards
                .reward_gem_owner_at
                .get_nested(&20240101)
                .read(&1)
                .unwrap(),
            VAL_Y
        );
        assert_eq!(
            rewards
                .reward_promis_load_at
                .get_nested(&20240101)
                .read(&1)
                .unwrap(),
            U256::from(300u64)
        );
    });
}

#[test]
fn prepared_reward_gem_batch_freezes_reward_utc_day_type_for_delivery() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        seed_oracle(&ctx, U256::from(2u64) * one_coen840());
        // The bootstrap closes as the first rewarded day does, so that day is
        // still Genesis and the next one is not.
        outbe_metadosis::test_support::seed_bootstrap_end_time(
            ctx.storage.clone(),
            date_key_to_utc_timestamp(20240102),
        )
        .unwrap();

        prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(50u64), &[(VAL_X, 1)])
            .unwrap();
        prepare_daily_validator_gem_batch(&ctx, 20240201, U256::from(70u64), &[(VAL_Y, 1)])
            .unwrap();

        let first = deliver_oldest_reward_gem_batch(&ctx).unwrap();
        assert!(matches!(
            first,
            RewardGemDeliveryOutcome::Delivered {
                reward_utc_day: 20240101,
                ..
            }
        ));
        let second = deliver_oldest_reward_gem_batch(&ctx).unwrap();
        assert!(matches!(
            second,
            RewardGemDeliveryOutcome::Delivered {
                reward_utc_day: 20240201,
                ..
            }
        ));

        let gem = outbe_gem::GemContract::new(ctx.storage.clone());

        assert_eq!(gem.balance_of(VAL_X).unwrap(), 1);
        let x_gem_id = gem.token_of_owner_by_index(VAL_X, 0).unwrap();
        let x_item = outbe_gem::api::get_gem(&ctx.storage, x_gem_id)
            .unwrap()
            .unwrap();
        assert_eq!(x_item.gem_type, GemTypes::Genesis as u8);
        assert!(
            x_item.floor_price_minor.is_zero(),
            "Genesis gem carries no floor"
        );

        assert_eq!(gem.balance_of(VAL_Y).unwrap(), 1);
        let y_gem_id = gem.token_of_owner_by_index(VAL_Y, 0).unwrap();
        let y_item = outbe_gem::api::get_gem(&ctx.storage, y_gem_id)
            .unwrap()
            .unwrap();
        assert_eq!(y_item.gem_type, GemTypes::Validator as u8);
        assert_eq!(
            y_item.state,
            outbe_gem::GemState::Issued as u8,
            "Post-genesis Validator gem is born Issued"
        );
    });
}

#[test]
fn identical_preparation_replay_does_not_append_a_second_batch() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        seed_oracle(&ctx, one_coen840());
        let voters = [(VAL_X, 1), (VAL_Y, 2)];

        let first =
            prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(300u64), &voters).unwrap();
        let replay =
            prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(300u64), &voters).unwrap();
        let rewards = ctx.storage.contract::<Rewards>();

        assert!(matches!(first, RewardGemPreparationOutcome::Prepared(_)));
        assert!(matches!(
            replay,
            RewardGemPreparationOutcome::AlreadyPrepared(_)
        ));
        assert_eq!(rewards.reward_gem_queue_head.read().unwrap(), 0);
        assert_eq!(rewards.reward_gem_queue_tail.read().unwrap(), 1);
    });
}

#[test]
fn delivered_preparation_replay_ignores_dead_recipient_rows() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        seed_oracle(&ctx, one_coen840());
        let voters = [(VAL_X, 1)];

        prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(100u64), &voters).unwrap();
        assert!(matches!(
            deliver_oldest_reward_gem_batch(&ctx).unwrap(),
            RewardGemDeliveryOutcome::Delivered {
                reward_utc_day: 20240101,
                ..
            }
        ));

        // A delivered batch is authoritative from its settled marker and
        // lack of live FIFO linkage. Dead recipient rows are not replay
        // inputs and therefore cannot halt Cycle or mint a second Gem.
        let rewards = ctx.storage.contract::<Rewards>();
        rewards
            .reward_gem_owner_at
            .get_nested(&20240101)
            .write(&0, VAL_Y)
            .unwrap();
        rewards
            .reward_promis_load_at
            .get_nested(&20240101)
            .write(&0, U256::from(999u64))
            .unwrap();

        let replay =
            prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(100u64), &voters).unwrap();
        assert!(matches!(
            replay,
            RewardGemPreparationOutcome::AlreadyPrepared(_)
        ));
        assert_eq!(rewards.reward_gem_queue_head.read().unwrap(), 1);
        assert_eq!(rewards.reward_gem_queue_tail.read().unwrap(), 1);
        assert_eq!(rewards.reward_gem_pending_batch_count.read().unwrap(), 0);
        assert_eq!(voter_promis_loads(&ctx, VAL_X).len(), 1);
    });
}

#[test]
fn prepared_batch_with_lost_fifo_linkage_is_retryable_without_writes() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        seed_oracle(&ctx, one_coen840());
        let voters = [(VAL_X, 1)];
        prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(100u64), &voters).unwrap();

        let rewards = ctx.storage.contract::<Rewards>();
        rewards.reward_gem_queue_tail.write(0).unwrap();

        let replay_error =
            prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(100u64), &voters)
                .unwrap_err();
        assert!(
            matches!(replay_error, PrecompileError::Revert(_)),
            "prepared obligation without a live FIFO link must fail closed: {replay_error:?}"
        );

        let delivery_error = deliver_oldest_reward_gem_batch(&ctx).unwrap_err();
        assert!(
            matches!(delivery_error, PrecompileError::Revert(_)),
            "an empty queue cannot hide a prepared unsettled obligation: {delivery_error:?}"
        );
        assert!(voter_promis_loads(&ctx, VAL_X).is_empty());
    });
}

#[test]
fn contradictory_preparation_replay_is_retryable_without_writes() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        seed_oracle(&ctx, one_coen840());
        prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(100u64), &[(VAL_X, 1)])
            .unwrap();
        let rewards = ctx.storage.contract::<Rewards>();
        let before_tail = rewards.reward_gem_queue_tail.read().unwrap();
        let before_digest = rewards.reward_gem_batch_digest.read(&20240101).unwrap();

        let err = ctx
            .with_checkpoint(|| {
                prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(101u64), &[(VAL_X, 1)])
            })
            .unwrap_err();
        assert!(matches!(err, PrecompileError::Revert(_)), "{err:?}");
        assert_eq!(rewards.reward_gem_queue_tail.read().unwrap(), before_tail);
        assert_eq!(
            rewards.reward_gem_batch_digest.read(&20240101).unwrap(),
            before_digest
        );
    });
}

#[test]
fn unpriced_delivery_preserves_fifo_head_without_minting() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        seed_oracle(&ctx, one_coen840());
        prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(100u64), &[(VAL_X, 1)])
            .unwrap();
        let (_, pair_index) =
            outbe_oracle::api::require_coen_pair(ctx.storage.clone(), 840).unwrap();
        let day = outbe_primitives::time::previous_date_key(
            outbe_primitives::time::timestamp_to_date_key(ctx.block.timestamp),
        );
        outbe_oracle::schema::OracleContract::new(ctx.storage.clone())
            .record_utc_day_vwap(day, pair_index, U256::ZERO)
            .unwrap();

        assert_eq!(
            deliver_oldest_reward_gem_batch(&ctx).unwrap(),
            RewardGemDeliveryOutcome::PendingRate {
                reward_utc_day: 20240101
            }
        );
        assert_eq!(
            deliver_oldest_reward_gem_batch(&ctx).unwrap(),
            RewardGemDeliveryOutcome::PendingRate {
                reward_utc_day: 20240101
            }
        );
        let rewards = ctx.storage.contract::<Rewards>();
        assert_eq!(rewards.reward_gem_queue_head.read().unwrap(), 0);
        assert_eq!(rewards.reward_gem_queue_tail.read().unwrap(), 1);
        assert!(voter_promis_loads(&ctx, VAL_X).is_empty());
    });
}

#[test]
fn fresh_delivery_mints_the_head_exactly_once() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        seed_oracle(&ctx, one_coen840());
        prepare_daily_validator_gem_batch(
            &ctx,
            20240101,
            U256::from(400u64),
            &[(VAL_X, 1), (VAL_Y, 3)],
        )
        .unwrap();

        assert_eq!(
            deliver_oldest_reward_gem_batch(&ctx).unwrap(),
            RewardGemDeliveryOutcome::Delivered {
                reward_utc_day: 20240101,
                recipient_count: 2,
                delivered_promis_load_amount: U256::from(400u64),
            }
        );
        assert_eq!(voter_promis_loads(&ctx, VAL_X), vec![U256::from(100u64)]);
        assert_eq!(voter_promis_loads(&ctx, VAL_Y), vec![U256::from(300u64)]);
        assert_eq!(
            deliver_oldest_reward_gem_batch(&ctx).unwrap(),
            RewardGemDeliveryOutcome::Empty
        );
        assert_eq!(voter_promis_loads(&ctx, VAL_X), vec![U256::from(100u64)]);
        assert_eq!(voter_promis_loads(&ctx, VAL_Y), vec![U256::from(300u64)]);
    });
}

#[test]
fn failed_delivery_rolls_back_every_gem_and_retries_the_same_batch() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        seed_oracle(&ctx, one_coen840());
        prepare_daily_validator_gem_batch(
            &ctx,
            20240101,
            U256::from(200u64),
            &[(VAL_X, 1), (VAL_Y, 1)],
        )
        .unwrap();
        let factory = outbe_gemfactory::schema::GemFactoryContract::new(ctx.storage.clone());
        factory
            .total_gems_issued
            .write(U256::MAX - U256::ONE)
            .unwrap();

        let err = ctx
            .with_checkpoint(|| deliver_oldest_reward_gem_batch(&ctx))
            .unwrap_err();
        assert!(matches!(err, PrecompileError::Revert(_)), "{err:?}");
        assert!(voter_promis_loads(&ctx, VAL_X).is_empty());
        assert!(voter_promis_loads(&ctx, VAL_Y).is_empty());
        let rewards = ctx.storage.contract::<Rewards>();
        assert_eq!(rewards.reward_gem_queue_head.read().unwrap(), 0);

        factory.total_gems_issued.write(U256::ZERO).unwrap();
        assert!(matches!(
            deliver_oldest_reward_gem_batch(&ctx).unwrap(),
            RewardGemDeliveryOutcome::Delivered {
                reward_utc_day: 20240101,
                ..
            }
        ));
        assert_eq!(voter_promis_loads(&ctx, VAL_X), vec![U256::from(100u64)]);
        assert_eq!(voter_promis_loads(&ctx, VAL_Y), vec![U256::from(100u64)]);
    });
}

#[test]
fn two_reward_utc_days_deliver_fifo_one_batch_per_call() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        seed_oracle(&ctx, one_coen840());
        prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(10u64), &[(VAL_X, 1)])
            .unwrap();
        prepare_daily_validator_gem_batch(&ctx, 20240102, U256::from(20u64), &[(VAL_Y, 1)])
            .unwrap();

        let first = deliver_oldest_reward_gem_batch(&ctx).unwrap();
        assert!(matches!(
            first,
            RewardGemDeliveryOutcome::Delivered {
                reward_utc_day: 20240101,
                ..
            }
        ));
        assert_eq!(voter_promis_loads(&ctx, VAL_X), vec![U256::from(10u64)]);
        assert!(voter_promis_loads(&ctx, VAL_Y).is_empty());

        let second = deliver_oldest_reward_gem_batch(&ctx).unwrap();
        assert!(matches!(
            second,
            RewardGemDeliveryOutcome::Delivered {
                reward_utc_day: 20240102,
                ..
            }
        ));
        assert_eq!(voter_promis_loads(&ctx, VAL_Y), vec![U256::from(20u64)]);
    });
}

#[test]
fn pending_reward_gem_batch_survives_storage_reopen() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        bootstrap_genesis(&ctx);
        seed_oracle(&ctx, one_coen840());
        prepare_daily_validator_gem_batch(&ctx, 20240101, U256::from(90u64), &[(VAL_Z, 1)])
            .unwrap();
    });

    storage.enter(|handle| {
        let reopened = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + 120), handle);
        let rewards = reopened.storage.contract::<Rewards>();
        assert!(rewards.daily_topup_prepared.read(&20240101).unwrap());
        assert_eq!(rewards.reward_gem_queue_head.read().unwrap(), 0);
        assert_eq!(rewards.reward_gem_queue_tail.read().unwrap(), 1);
        assert!(matches!(
            deliver_oldest_reward_gem_batch(&reopened).unwrap(),
            RewardGemDeliveryOutcome::Delivered {
                reward_utc_day: 20240101,
                delivered_promis_load_amount,
                ..
            } if delivered_promis_load_amount == U256::from(90u64)
        ));
        assert_eq!(
            voter_promis_loads(&reopened, VAL_Z),
            vec![U256::from(90u64)]
        );
        assert!(rewards.daily_topup_settled.read(&20240101).unwrap());
    });
}
