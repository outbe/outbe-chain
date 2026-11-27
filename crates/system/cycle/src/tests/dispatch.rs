//! Task dispatch, emission settlement and validator top-up conservation.

use alloy_sol_types::SolEvent;
use outbe_agentreward::api::RewardPool;
use outbe_primitives::error::PrecompileError;
use outbe_system_common::addresses::CYCLE_ADDRESS;

use super::*;

#[test]
fn cycle_tick_runs_dispatcher() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let block_ts = GENESIS_TS + 60;
        let ctx = genesis_block(handle, block_ts);

        run_cycle_tick(&ctx).unwrap();

        // Same as `first_encounter_anchors_without_firing`: handle_cycle_tick
        // delegates to run_due_tasks.
        assert_eq!(last_executed_at(&ctx), block_ts);
    });
}

// ---------------------------------------------------------------------------
// advance_auctions task
// ---------------------------------------------------------------------------

#[test]
fn dispatcher_fires_advance_auctions_at_its_slot() {
    with_anchored_cycle(|handle| {
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 5;
        let ctx_fire = dispatch_at(handle, 2, fire_ts);

        let advance_auctions_id = TaskId::AdvanceAuctions.as_u32();
        let cycle: Cycle<'_> = ctx_fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&advance_auctions_id).unwrap(),
            GENESIS_TS + SECONDS_PER_DAY + PROCESS_EMISSION_DAY_START_OFFSET_SECONDS,
            "coalesces the backlog to the latest slot at or before the block"
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&advance_auctions_id)
                .unwrap(),
            2
        );
    });
}

// ---------------------------------------------------------------------------
// End-to-end: handler effects on Rewards, AgentReward, Metadosis
// ---------------------------------------------------------------------------

/// Records finalized block 10 of genesis day, with committee `early` and
/// `late`. Only `early` signed the base certificate, so `late` can still earn a
/// late reward credit.
pub(super) fn record_parent_with_late_voter(
    ctx: &BlockRuntimeContext<'_>,
    hash: B256,
    early: Address,
    late: Address,
) {
    let metadata = outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata {
        finalized_block_number: 10,
        finalized_block_hash: hash,
        finalized_epoch: 0,
        finalized_view: 10,
        parent_view: 9,
        ordered_committee: vec![early, late],
        signer_bitmap: vec![1],
        proof: Default::default(),
        committee_set_hash: B256::ZERO,
        vrf_material_version: 0,
        vrf_group_public_key_hash: B256::ZERO,
        proof_kind: outbe_primitives::consensus_metadata::ParentParticipationProof::Finalization,
        missed_proposers: vec![],
    };
    outbe_rewards::finalized_metadata_hook::record_votes_and_fees(
        ctx,
        &metadata,
        U256::ZERO,
        GENESIS_TS + SECONDS_PER_DAY - 1,
        &[early],
    )
    .unwrap();
}

#[test]
fn process_emission_day_keeps_midnight_slot_pending_until_late_reward_window_closes() {
    let mut storage = cycle_storage();
    let hash = B256::repeat_byte(0x77);
    let early = Address::repeat_byte(0xB0);
    let late = Address::repeat_byte(0xB1);
    storage.enter(|handle| {
        anchor_with(handle.clone(), GENESIS_TS + 60, seed_fresh_reward_oracle);
    });
    storage.set_block_number(11);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(11, GENESIS_TS + SECONDS_PER_DAY), handle);
        record_parent_with_late_voter(&ctx, hash, early, late);
    });
    let advance_auctions = TaskId::AdvanceAuctions.as_u32();
    let advance_slot = |handle: &outbe_primitives::storage::StorageHandle<'_>| {
        handle
            .contract::<Cycle<'_>>()
            .last_executed_at
            .read(&advance_auctions)
            .unwrap()
    };
    let advance_before = storage.enter(|handle| advance_slot(&handle));
    for height in 11..=13 {
        storage.set_block_number(height);
        storage.enter(|handle| {
            let ctx = runtime_context(
                block_ctx(height, GENESIS_TS + SECONDS_PER_DAY + height - 11),
                handle,
            );
            account_parent(&ctx, height);
            run_due_tasks(&ctx).unwrap();
            let cycle = ctx.storage.contract::<Cycle>();
            assert_eq!(active_utc_day(&ctx), 20240101);
            assert_eq!(
                cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
                GENESIS_TS + 60
            );
            assert!(!is_day_allocated(&ctx, 20240101));
            assert_eq!(advance_slot(&ctx.storage), advance_before);
            if height == 13 {
                // Real phase order: Cycle first, then final-slot credit and GC.
                outbe_rewards::block_fees::record_late_vote(&ctx, hash, late, 3).unwrap();
                ctx.storage
                    .with_checkpoint(|| outbe_rewards::block_fees::seal_block_votes(&ctx, 13, 3))
                    .unwrap();
            }
        });
    }
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 4);
    storage.set_block_number(14);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle, 14, GENESIS_TS + SECONDS_PER_DAY + 3);
        assert!(is_day_allocated(&ctx, 20240101));
        let cycle = ctx.storage.contract::<Cycle>();
        assert_eq!(active_utc_day(&ctx), 20240102);
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&EMISSION_LIMIT_1_ID)
                .unwrap(),
            14
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&advance_auctions)
                .unwrap(),
            14
        );
        let validator_claimable = |voter| day_reward(&ctx, RewardPool::Validator, 20240101, voter);
        // The late voter counts the same as the base-certificate voter.
        assert_eq!(validator_claimable(early), validator_claimable(late));
        assert!(validator_claimable(late) > U256::ZERO);
        let credited = validator_claimable(early);
        run_due_tasks(&ctx).unwrap();
        assert_eq!(validator_claimable(early), credited);
    });
}

#[test]
fn end_to_end_emission_dispatch_marks_day_settled_and_credits_metadosis() {
    with_anchored_cycle(|handle| {
        // Step 1: anchor at chain start.

        // Step 2: block past first slot. prev_day = start_date
        // (20240101). day_number_since_genesis = 0. cap = INITIAL_DAY_EMISSION.
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 60;
        let ctx_fire = dispatch_at(handle, 2, fire_ts);

        assert!(
            is_day_allocated(&ctx_fire, 20_240_101),
            "Cycle handler must seal prev_day"
        );

        // Cycle's last_executed_at advanced to the slot
        // (GENESIS_TS + 86_400 + 1), not the block timestamp.
        assert_eq!(
            last_executed_at(&ctx_fire),
            GENESIS_TS + SECONDS_PER_DAY + PROCESS_EMISSION_DAY_START_OFFSET_SECONDS
        );

        // No offers for any AgentReward pool: the pools with no receiver send their limit to
        // the Promis Limit, and no native COEN moves.
        let agent_reward_balance = ctx_fire
            .storage
            .balance(outbe_system_common::addresses::AGENT_REWARD_ADDRESS)
            .unwrap();
        assert_eq!(agent_reward_balance, U256::ZERO);

        // No eligible CCA: its whole limit goes to the Promis Limit.
        let cca = ctx_fire
            .storage
            .balance(outbe_system_common::addresses::CCA_REGISTRY_ADDRESS)
            .unwrap();
        assert_eq!(
            cca,
            U256::ZERO,
            "no eligible CCA weight; the pool goes to the Promis Limit"
        );
    });
}

#[test]
fn one_rate_pays_every_receiver_of_a_pool_with_no_reward_run() {
    with_anchored_cycle(|handle| {
        let day = 20_240_101;
        let recipients = 260u64;
        let recipient = |n: u64| Address::from_word(U256::from(n + 1).into());
        let mut agent_reward = outbe_agentreward::AgentRewardContract::new(handle.clone());
        for n in 0..recipients {
            agent_reward.increment_waa_offer(day, recipient(n)).unwrap();
        }
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        run_emission_limit_daily(&ctx).unwrap();

        let pool = day_zero_shares().waa;
        let each = pool / U256::from(recipients);
        for n in [0, recipients / 2, recipients - 1] {
            assert_eq!(day_reward(&ctx, RewardPool::Waa, day, recipient(n)), each);
        }
        let record = outbe_emission::api::get_emission_day(ctx.storage.clone(), day)
            .unwrap()
            .unwrap();
        assert_eq!(U256::from(record.waa), each * U256::from(recipients));
        let (_, _, unclaimed) = agent_reward
            .agent_reward_rate(RewardPool::Waa, day)
            .unwrap();
        assert_eq!(unclaimed, U256::from(record.waa));
        assert_eq!(
            agent_reward.waa_address_count.read(&day).unwrap(),
            recipients as u32
        );
    });
}

#[test]
fn the_offering_end_pays_the_previous_emission_day_agent_activity() {
    const EMISSION_DAY: u32 = 20_240_101;

    with_anchored_cycle(|handle| {
        let wallet = Address::repeat_byte(0x71);
        let sra = Address::repeat_byte(0x72);
        let mut agent_reward = outbe_agentreward::AgentRewardContract::new(handle.clone());
        agent_reward
            .increment_waa_offer(EMISSION_DAY, wallet)
            .unwrap();
        agent_reward.increment_sra_offer(EMISSION_DAY, sra).unwrap();

        let ctx = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        run_emission_limit_daily(&ctx).unwrap();

        let shares = day_zero_shares();
        assert_eq!(
            day_reward(&ctx, RewardPool::Waa, EMISSION_DAY, wallet),
            shares.waa
        );
        assert_eq!(
            day_reward(&ctx, RewardPool::Sra, EMISSION_DAY, sra),
            shares.sra
        );
        assert_eq!(
            agent_reward
                .weight(RewardPool::Waa, EMISSION_DAY, wallet)
                .unwrap(),
            U256::from(1),
            "the weight stays until the claim"
        );
        assert_eq!(
            ctx.storage
                .balance(outbe_system_common::addresses::AGENT_REWARD_ADDRESS)
                .unwrap(),
            U256::ZERO,
            "AgentReward holds no native COEN"
        );
    });
}

#[test]
fn validators_get_the_full_share_by_their_votes() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        genesis_block(handle.clone(), GENESIS_TS + 60);
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);

        seed_fresh_reward_oracle(&ctx);

        let voters = [
            Address::repeat_byte(0x31),
            Address::repeat_byte(0x32),
            Address::repeat_byte(0x33),
        ];
        seed_daily_voters(&ctx, 20_240_101, &voters.map(|voter| (voter, 1)));
        ctx.storage
            .contract::<outbe_rewards::schema::Rewards<'_>>()
            .day_fee_sum_native
            .write(&20_240_101, U256::MAX)
            .unwrap();

        run_emission_limit_daily(&ctx).unwrap();

        let each = day_zero_shares().validators / U256::from(voters.len());
        for voter in voters {
            assert_eq!(
                day_reward(&ctx, RewardPool::Validator, 20_240_101, voter),
                each,
                "the fees of the day are not subtracted from the share"
            );
            assert_eq!(outbe_gem::api::balance_of(&ctx.storage, voter).unwrap(), 0);
        }
    });
    assert!(
        storage
            .get_events(outbe_system_common::addresses::GEM_ADDRESS)
            .is_empty(),
        "setting the rate mints no Gem"
    );
}

#[test]
fn a_pool_with_no_weight_sends_its_limit_to_the_promis_limit() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        genesis_block(handle.clone(), GENESIS_TS + 60);
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);

        let voters = [Address::repeat_byte(0x51), Address::repeat_byte(0x52)];
        seed_daily_voters(&ctx, 20_240_101, &voters.map(|voter| (voter, 0)));

        run_emission_limit_daily(&ctx).unwrap();

        assert!(is_day_allocated(&ctx, 20_240_101));
        let record = outbe_emission::api::get_emission_day(ctx.storage.clone(), 20_240_101)
            .unwrap()
            .unwrap();
        assert_eq!(
            (record.validators, record.waa, record.sra, record.cca),
            (0, 0, 0, 0)
        );
        let shares = day_zero_shares();
        assert_eq!(U256::from(record.metadosis_limit), shares.metadosis);
        assert_eq!(
            U256::from(record.promis_limit as u128),
            shares.validators + shares.waa + shares.sra + shares.cca
        );
        for voter in voters {
            assert_eq!(
                day_reward(&ctx, RewardPool::Validator, 20_240_101, voter),
                U256::ZERO
            );
        }
    });
}

/// Day-1 voters of the dispatch scenario.
const TOPUP_VOTERS: [Address; 3] = [
    Address::repeat_byte(0x61),
    Address::repeat_byte(0x62),
    Address::repeat_byte(0x63),
];

/// Block time of the first settlement dispatch on day 2.
const TOPUP_FIRE_TS: u64 = GENESIS_TS + SECONDS_PER_DAY + 60;

/// Anchors with a bonded CCA and runs the first settlement dispatch, with `before_dispatch` run
/// after the fixture writes, and returns its result.
fn terminal_dispatch_after(
    before_dispatch: impl FnOnce(&BlockRuntimeContext<'_>),
) -> (HashMapStorageProvider, outbe_primitives::error::Result<()>) {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        anchor_with(handle, GENESIS_TS + 60, |anchor| {
            seed_reward_cca(&anchor.storage)
        });
    });

    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 0);
    storage.set_block_number(2);
    let dispatched = storage.enter(|handle| {
        let fire = runtime_context(block_ctx(2, TOPUP_FIRE_TS), handle);
        account_parent(&fire, 2);
        seed_fresh_reward_oracle(&fire);
        seed_daily_voters(&fire, 20_240_101, &TOPUP_VOTERS.map(|voter| (voter, 1)));
        before_dispatch(&fire);
        run_due_tasks(&fire)
    });
    (storage, dispatched)
}

/// Writes an unknown state into the record of `day`.
fn corrupt_emission_day(ctx: &BlockRuntimeContext<'_>, day: u32) {
    let mut record = outbe_emission::api::get_emission_day(ctx.storage.clone(), day)
        .unwrap()
        .unwrap_or_else(|| {
            outbe_emission::schema::EmissionDay::new(
                day,
                outbe_system_common::time::WorldwideDay::new(0),
                1,
                0,
                [0; 5],
            )
        });
    record.state = 7;
    outbe_emission::api::put_emission_day(ctx.storage.clone(), &record).unwrap();
}

fn cycle_completed(storage: &HashMapStorageProvider, id: u32) -> bool {
    storage.get_events(CYCLE_ADDRESS).iter().any(|log| {
        crate::ICycle::CycleTaskCompleted::decode_log_data(log)
            .is_ok_and(|event| u32::from(event.task as u8) == id)
    })
}

#[test]
fn a_corrupt_day_record_fails_the_block() {
    let (_, dispatched) = terminal_dispatch_after(|fire| corrupt_emission_day(fire, 20_240_101));
    let err = dispatched.expect_err("a corrupt record must fail the block");
    assert!(
        matches!(
            err,
            PrecompileError::Fatal(_) | PrecompileError::FatalWithCause(_)
        ),
        "{err:?}"
    );
}

#[test]
fn invalid_active_utc_day_does_not_consume_the_slot_or_emit_failure() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        anchor_at(handle.clone(), GENESIS_TS + 60);
        let fire = runtime_context(
            outbe_primitives::block::BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 2,
                timestamp: TOPUP_FIRE_TS,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 0,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            handle,
        );
        account_parent(&fire, 2);
        let err = run_due_tasks(&fire).expect_err("invalid day must fail the block");
        assert!(matches!(err, PrecompileError::Fatal(_)));
        assert_eq!(last_executed_at(&fire), GENESIS_TS + 60);
    });
    assert!(!cycle_completed(&storage, EMISSION_LIMIT_1_ID));
}

#[test]
fn storage_error_from_the_handler_does_not_consume_the_slot() {
    let mut storage = prepared_settlement_storage();
    storage.fail_mutation_at(0);
    storage.enter(|handle| {
        let fire = runtime_context(block_ctx(2, TOPUP_FIRE_TS), handle);
        let err = run_due_tasks(&fire).expect_err("storage failure must fail the block");
        assert!(matches!(err, PrecompileError::Storage(_)));
        assert_eq!(last_executed_at(&fire), GENESIS_TS + 60);
        assert!(!is_day_allocated(&fire, 20_240_101));
    });
    assert!(!cycle_completed(&storage, EMISSION_LIMIT_1_ID));
}

fn prepared_settlement_storage() -> HashMapStorageProvider {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        anchor_with(handle.clone(), GENESIS_TS + 60, |anchor| {
            seed_reward_cca(&anchor.storage)
        });
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        let fire = runtime_context(block_ctx(2, TOPUP_FIRE_TS), handle);
        account_parent(&fire, 2);
        seed_fresh_reward_oracle(&fire);
        seed_daily_voters(&fire, 20_240_101, &TOPUP_VOTERS.map(|voter| (voter, 1)));
    });
    storage
}

/// A second `run_emission_limit_daily` call for an already-settled `prev_day`
/// is a no-op. The handler does not credit the CCA agent pool or terminal
/// Metadosis twice. This test checks the per-day idempotency guard on top of
/// the C-01 timestamp drift band.
#[test]
fn emission_dispatch_is_idempotent_per_prev_day() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        anchor_with(handle.clone(), GENESIS_TS + 60, |anchor| {
            seed_reward_cca(&anchor.storage)
        });
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        account_parent(&ctx, 2);

        // First settlement of prev_day = 20240101: sets the rates and starts the day.
        run_emission_limit_daily(&ctx).unwrap();
        assert!(
            is_day_allocated(&ctx, 20_240_101),
            "first fire must seal prev_day"
        );
        let cca_claimable = || {
            day_reward(
                &ctx,
                RewardPool::Cca,
                20_240_101,
                Address::repeat_byte(0xc1),
            )
        };
        let cca_after_first = cca_claimable();
        let metadosis_after_first = ctx
            .storage
            .balance(outbe_system_common::addresses::METADOSIS_ADDRESS)
            .unwrap();
        assert!(!cca_after_first.is_zero(), "first fire credited CCA");

        // Second invocation for the SAME prev_day: the day already started, so no rate is set
        // again.
        run_emission_limit_daily(&ctx).unwrap();
        assert_eq!(
            cca_claimable(),
            cca_after_first,
            "CCA pool must not be credited twice for the same prev_day"
        );
        assert_eq!(
            ctx.storage
                .balance(outbe_system_common::addresses::METADOSIS_ADDRESS)
                .unwrap(),
            metadosis_after_first,
            "terminal Metadosis must not be re-dispatched for the same prev_day"
        );
    });
}

#[test]
fn repeated_settled_cycle_slot_replays_without_any_storage_or_event_write() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        anchor_at(handle.clone(), GENESIS_TS + 60);
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        let fire = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        account_parent(&fire, 2);
        run_emission_limit_daily(&fire).unwrap();
    });

    assert_storage_unchanged(&mut storage, |handle| {
        let replay = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        run_emission_limit_daily(&replay).unwrap();
    });
}

#[test]
fn desis_takes_the_day_after_it_started() {
    let day = outbe_system_common::time::EmissionDate::new(20_240_101);
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        genesis_block(handle.clone(), GENESIS_TS + 60);
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        let desis = outbe_desis::DesisContract::new(ctx.storage.clone());
        outbe_desis::advance_intex_auctions(&ctx).unwrap();
        assert_eq!(desis.sched_active_slot.read(&day).unwrap(), 0);
        run_emission_limit_daily(&ctx).unwrap();
        outbe_desis::advance_intex_auctions(&ctx).unwrap();
        assert_eq!(desis.next_auction_day.read().unwrap(), 20_240_102);
    });
}
