//! Trigger dispatch, emission settlement and validator top-up conservation.

use outbe_emissionlimit::allocation::EmissionSinkId;

use super::*;

#[test]
fn cycle_lifecycle_begin_block_runs_dispatcher() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let block_ts = GENESIS_TS + 60;
        let ctx = genesis_block(handle, block_ts);

        run_cycle_lifecycle(&ctx).unwrap();

        // Same as `first_encounter_anchors_without_firing`: begin_block
        // delegates to dispatch_triggers.
        assert_eq!(last_executed_at(&ctx), block_ts);
    });
}

// ---------------------------------------------------------------------------
// auction_advance trigger
// ---------------------------------------------------------------------------

#[test]
fn dispatcher_fires_auction_advance_at_its_slot() {
    with_anchored_cycle(|handle| {
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 5;
        let ctx_fire = dispatch_at(handle, 2, fire_ts);

        let auction_advance_id = TriggerId::AuctionAdvance.as_u32();
        let cycle: Cycle<'_> = ctx_fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&auction_advance_id).unwrap(),
            GENESIS_TS + SECONDS_PER_DAY,
            "coalesces the backlog to the latest slot at or before the block"
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&auction_advance_id)
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
fn record_parent_with_late_voter(
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
    outbe_rewards::finalized_metadata_hook::on_finalized_metadata(
        ctx,
        &metadata,
        U256::ZERO,
        GENESIS_TS + SECONDS_PER_DAY - 1,
        &[early],
    )
    .unwrap();
}

#[test]
fn protocol_cycle_keeps_midnight_slot_pending_until_late_reward_window_closes() {
    let mut storage = cycle_storage();
    let hash = B256::repeat_byte(0x77);
    let early = Address::repeat_byte(0xB0);
    let late = Address::repeat_byte(0xB1);
    storage.enter(|handle| {
        anchor_with(handle.clone(), GENESIS_TS + 60, seed_fresh_reward_oracle);
        let ctx = BlockRuntimeContext::new(block_ctx(11, GENESIS_TS + SECONDS_PER_DAY), handle);
        record_parent_with_late_voter(&ctx, hash, early, late);
    });
    for height in 11..=13 {
        storage.enter(|handle| {
            let ctx = BlockRuntimeContext::new(
                block_ctx(height, GENESIS_TS + SECONDS_PER_DAY + height - 11),
                handle,
            );
            account_parent(&ctx, height);
            dispatch_triggers(&ctx).unwrap();
            let cycle = ctx.storage.contract::<Cycle>();
            assert_eq!(cycle.active_utc_day.read().unwrap(), 20240101);
            assert_eq!(
                cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
                GENESIS_TS + 60
            );
            assert!(!outbe_rewards::api::is_day_settled(&ctx, 20240101).unwrap());
            if height == 13 {
                // Real phase order: Cycle first, then final-slot credit and GC.
                outbe_rewards::late_settlement::record_late_credit(&ctx, hash, late, 3).unwrap();
                outbe_rewards::late_settlement::settle_matured(&ctx, 13, 3).unwrap();
            }
        });
    }
    storage.enter(|handle| {
        let ctx = dispatch_at(handle, 14, GENESIS_TS + SECONDS_PER_DAY + 3);
        assert!(outbe_rewards::api::is_day_settled(&ctx, 20240101).unwrap());
        let cycle = ctx.storage.contract::<Cycle>();
        assert_eq!(cycle.active_utc_day.read().unwrap(), 20240102);
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&EMISSION_LIMIT_1_ID)
                .unwrap(),
            14
        );
        let validator_claimable = |voter| {
            outbe_agentreward::AgentRewardContract::new(ctx.storage.clone())
                .get_pool_claimable_reward(outbe_agentreward::schema::RewardPool::Validator, voter)
                .unwrap()
        };
        // The late voter counts the same as the base-certificate voter.
        assert_eq!(validator_claimable(early), validator_claimable(late));
        assert!(validator_claimable(late) > U256::ZERO);
        let credited = validator_claimable(early);
        dispatch_triggers(&ctx).unwrap();
        assert_eq!(validator_claimable(early), credited);
    });
}

#[test]
fn end_to_end_emission_dispatch_marks_day_settled_and_credits_metadosis() {
    with_anchored_cycle(|handle| {
        // Step 1: anchor at chain start.

        // Step 2: block past first slot. prev_day = genesis_utc_day
        // (20240101). day_number_since_genesis = 0. cap = INITIAL_DAY_EMISSION.
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 60;
        let ctx_fire = dispatch_at(handle, 2, fire_ts);

        // Rewards.daily_settled[20240101] = true (sealed against late
        // finalized metadata for the previous UTC day).
        let rewards = ctx_fire
            .storage
            .contract::<outbe_rewards::schema::Rewards<'_>>();
        assert!(
            rewards.daily_settled.read(&20_240_101).unwrap(),
            "Cycle handler must seal prev_day"
        );

        // Cycle's last_executed_at advanced to the slot
        // (GENESIS_TS + 86_400), not the block timestamp.
        assert_eq!(last_executed_at(&ctx_fire), GENESIS_TS + SECONDS_PER_DAY);

        // No tributes for any AgentReward pool: all three WAA/SRA/CCA
        // allocations return to terminal Metadosis and no native COEN moves.
        let agent_reward_balance = ctx_fire
            .storage
            .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
            .unwrap();
        assert_eq!(agent_reward_balance, U256::ZERO);

        // No eligible CCA: its full allocation goes to terminal Metadosis.
        let cca = ctx_fire
            .storage
            .balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS)
            .unwrap();
        assert_eq!(
            cca,
            U256::ZERO,
            "no eligible CCA weight; pool goes to Metadosis"
        );
    });
}

#[test]
fn next_day_cycle_settlement_pays_previous_utc_day_agent_activity() {
    const REWARD_UTC_DAY: u32 = 20_240_101;

    with_anchored_cycle(|handle| {
        let wallet = Address::repeat_byte(0x71);
        let sra = Address::repeat_byte(0x72);
        let reward_day = outbe_primitives::time::WorldwideDay::new(REWARD_UTC_DAY);
        let mut agent_reward = outbe_agentreward::AgentRewardContract::new(handle.clone());
        agent_reward
            .increment_waa_tribute(reward_day, wallet)
            .unwrap();
        agent_reward.increment_sra_tribute(reward_day, sra).unwrap();

        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 60;
        let ctx_fire = dispatch_at(handle, 2, fire_ts);

        let agent_reward = outbe_agentreward::AgentRewardContract::new(ctx_fire.storage.clone());
        let wallet_claimable = agent_reward.get_claimable_reward(wallet).unwrap();
        let sra_claimable = agent_reward.get_claimable_reward(sra).unwrap();
        let waa_pool = day_zero_allocation(EmissionSinkId::Waa);
        let sra_pool = day_zero_allocation(EmissionSinkId::Sra);
        assert_eq!(wallet_claimable, waa_pool);
        assert_eq!(sra_claimable, sra_pool);
        assert!(
            agent_reward
                .get_all_waa_counts(reward_day)
                .unwrap()
                .is_empty(),
            "settled WAA counters must be cleared"
        );
        assert!(
            agent_reward
                .get_all_sra_counts(reward_day)
                .unwrap()
                .is_empty(),
            "settled SRA counters must be cleared"
        );
        assert_eq!(
            ctx_fire
                .storage
                .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                .unwrap(),
            U256::ZERO,
            "AgentReward holds no native COEN"
        );
    });
}

#[test]
fn prepared_validator_topup_and_terminal_residue_conserve_the_allocation() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        genesis_block(handle.clone(), GENESIS_TS + 60);
        let ctx = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);

        seed_fresh_reward_oracle(&ctx);

        let voters = [
            Address::repeat_byte(0x31),
            Address::repeat_byte(0x32),
            Address::repeat_byte(0x33),
        ];
        seed_daily_voters(&ctx, 20_240_101, &voters.map(|voter| (voter, 1)));

        run_emission_limit_daily(&ctx).unwrap();

        let validator_amount = day_zero_allocation(EmissionSinkId::Validator);
        let agent_reward = outbe_agentreward::AgentRewardContract::new(ctx.storage.clone());
        let planned = voters.iter().fold(U256::ZERO, |total, voter| {
            total
                + agent_reward
                    .get_pool_claimable_reward(
                        outbe_agentreward::schema::RewardPool::Validator,
                        *voter,
                    )
                    .unwrap()
        });
        let gem = outbe_gem::GemContract::new(ctx.storage.clone());
        assert_eq!(
            gem.total_supply().unwrap(),
            0,
            "Cycle credits claims but does not mint Gems"
        );
        let expected_each = validator_amount / U256::from(voters.len());
        assert_eq!(
            planned,
            expected_each * U256::from(voters.len()),
            "credited claims must equal distributed validator pool"
        );
    });
}

#[test]
fn zero_total_validator_participation_routes_the_pool_without_halting() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        genesis_block(handle.clone(), GENESIS_TS + 60);
        let ctx = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);

        let voters = [Address::repeat_byte(0x51), Address::repeat_byte(0x52)];
        seed_daily_voters(&ctx, 20_240_101, &voters.map(|voter| (voter, 0)));

        run_emission_limit_daily(&ctx).unwrap();

        let rewards = ctx.storage.contract::<outbe_rewards::schema::Rewards<'_>>();
        assert!(rewards.daily_settled.read(&20_240_101).unwrap());
        let agent_reward = outbe_agentreward::AgentRewardContract::new(ctx.storage.clone());
        for voter in voters {
            assert_eq!(
                agent_reward
                    .get_pool_claimable_reward(
                        outbe_agentreward::schema::RewardPool::Validator,
                        voter
                    )
                    .unwrap(),
                U256::ZERO
            );
        }
    });
}

/// Day-1 voters of the failed-terminal-dispatch scenario.
const TOPUP_VOTERS: [Address; 3] = [
    Address::repeat_byte(0x61),
    Address::repeat_byte(0x62),
    Address::repeat_byte(0x63),
];

/// Block time of the first settlement dispatch on day 2.
const TOPUP_FIRE_TS: u64 = GENESIS_TS + SECONDS_PER_DAY + 60;

/// Anchors with a bonded CCA, then runs the first settlement dispatch without
/// Metadosis mutation frames, so the terminal sink fails. Returns the storage
/// and the dispatch error.
fn failed_terminal_dispatch() -> (
    HashMapStorageProvider,
    outbe_primitives::error::PrecompileError,
) {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        anchor_with(handle, GENESIS_TS + 60, |anchor| {
            seed_reward_cca(&anchor.storage)
        });
    });

    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 0);
    let error = storage.enter(|handle| {
        let fire = BlockRuntimeContext::new(block_ctx(2, TOPUP_FIRE_TS), handle);
        account_parent(&fire, 2);
        seed_fresh_reward_oracle(&fire);
        seed_daily_voters(&fire, 20_240_101, &TOPUP_VOTERS.map(|voter| (voter, 1)));
        dispatch_triggers(&fire).unwrap_err()
    });
    (storage, error)
}

fn topup_voters_claimable(ctx: &BlockRuntimeContext<'_>) -> U256 {
    let agent_reward = outbe_agentreward::AgentRewardContract::new(ctx.storage.clone());
    TOPUP_VOTERS.iter().fold(U256::ZERO, |total, voter| {
        total
            + agent_reward
                .get_pool_claimable_reward(outbe_agentreward::schema::RewardPool::Validator, *voter)
                .unwrap()
    })
}

#[test]
fn failed_terminal_dispatch_rolls_back_validator_topup() {
    let (mut storage, error) = failed_terminal_dispatch();
    assert!(
        error
            .to_string()
            .contains("no matching Metadosis mutation lease"),
        "the injected downstream failure must reach the terminal sink: {error}"
    );
    storage.enter(|handle| {
        let fire = BlockRuntimeContext::new(block_ctx(2, TOPUP_FIRE_TS), handle);
        let rewards = fire
            .storage
            .contract::<outbe_rewards::schema::Rewards<'_>>();
        assert!(!rewards.daily_settled.read(&20_240_101).unwrap());
        assert_eq!(
            topup_voters_claimable(&fire),
            U256::ZERO,
            "validator credit must roll back"
        );
        assert_eq!(
            fire.storage
                .balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS)
                .unwrap(),
            outbe_ccaregistry::constants::BOND_REQUIREMENT,
            "CCA reward credit must roll back, preserving the bond"
        );
        assert_eq!(
            fire.storage
                .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                .unwrap(),
            U256::ZERO
        );
        let cycle: Cycle<'_> = fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            GENESIS_TS + 60,
            "the failed trigger must remain due for retry"
        );
        assert_eq!(cycle.active_utc_day.read().unwrap(), 20_240_101);
    });
}

#[test]
fn retry_after_failed_terminal_dispatch_settles_once() {
    let (mut storage, _) = failed_terminal_dispatch();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    storage.enter(|handle| {
        let retry = BlockRuntimeContext::new(block_ctx(2, TOPUP_FIRE_TS), handle);
        dispatch_triggers(&retry).unwrap();

        let rewards = retry
            .storage
            .contract::<outbe_rewards::schema::Rewards<'_>>();
        assert!(rewards.daily_settled.read(&20_240_101).unwrap());

        let validator_amount = day_zero_allocation(EmissionSinkId::Validator);
        let expected_promis_load = validator_amount / U256::from(TOPUP_VOTERS.len());
        let cca_pool = day_zero_allocation(EmissionSinkId::Cca);
        assert_eq!(
            retry
                .storage
                .balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS)
                .unwrap(),
            outbe_ccaregistry::constants::BOND_REQUIREMENT,
            "CCA custody holds only the bond"
        );
        assert_eq!(
            outbe_agentreward::AgentRewardContract::new(retry.storage.clone())
                .get_pool_claimable_reward(
                    outbe_agentreward::schema::RewardPool::Cca,
                    Address::repeat_byte(0xc1)
                )
                .unwrap(),
            cca_pool,
            "retry must credit the CCA pool exactly once"
        );
        let gem = outbe_gem::GemContract::new(retry.storage.clone());
        assert_eq!(
            gem.total_supply().unwrap(),
            0,
            "Cycle credits claims; validators mint their own Gems"
        );
        assert_eq!(
            topup_voters_claimable(&retry),
            expected_promis_load * U256::from(TOPUP_VOTERS.len()),
            "retry must credit the validator pool exactly once"
        );
        assert_eq!(last_executed_at(&retry), GENESIS_TS + SECONDS_PER_DAY);
    });
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

        let ctx = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        account_parent(&ctx, 2);

        // First settlement of prev_day = 20240101: credits the pools + seals.
        run_emission_limit_daily(&ctx).unwrap();
        let rewards = ctx.storage.contract::<outbe_rewards::schema::Rewards<'_>>();
        assert!(
            rewards.daily_settled.read(&20_240_101).unwrap(),
            "first fire must seal prev_day"
        );
        let cca_claimable = || {
            outbe_agentreward::AgentRewardContract::new(ctx.storage.clone())
                .get_pool_claimable_reward(
                    outbe_agentreward::schema::RewardPool::Cca,
                    Address::repeat_byte(0xc1),
                )
                .unwrap()
        };
        let cca_after_first = cca_claimable();
        let metadosis_after_first = ctx
            .storage
            .balance(outbe_primitives::addresses::METADOSIS_ADDRESS)
            .unwrap();
        assert!(!cca_after_first.is_zero(), "first fire credited CCA");

        // Second invocation for the SAME prev_day: the idempotency guard sees
        // `daily_settled[20240101] == true` and returns early. No double credit occurs.
        run_emission_limit_daily(&ctx).unwrap();
        assert_eq!(
            cca_claimable(),
            cca_after_first,
            "CCA pool must not be credited twice for the same prev_day"
        );
        assert_eq!(
            ctx.storage
                .balance(outbe_primitives::addresses::METADOSIS_ADDRESS)
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

        let fire =
            BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        account_parent(&fire, 2);
        run_emission_limit_daily(&fire).unwrap();
    });

    assert_storage_unchanged(&mut storage, |handle| {
        let replay =
            BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        run_emission_limit_daily(&replay).unwrap();
    });
}
