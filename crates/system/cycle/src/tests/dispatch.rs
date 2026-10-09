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
        let rewards = ctx.storage.contract::<outbe_rewards::schema::Rewards>();
        assert_eq!(
            rewards.reward_gem_recipient_count.read(&20240101).unwrap(),
            2
        );
        let loads = rewards.reward_promis_load_at.get_nested(&20240101);
        assert_eq!(loads.read(&0).unwrap(), loads.read(&1).unwrap());
        assert!(loads.read(&1).unwrap() > U256::ZERO);
        dispatch_triggers(&ctx).unwrap();
        assert_eq!(rewards.reward_gem_queue_tail.read().unwrap(), 1);
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

        // No tributes for any AgentReward pool, so all three
        // WAA/SRA/CCA amounts are accounted for.
        // Empty WAA/SRA pools burn their backing. An empty CCA pool mints
        // nothing. All three allocations return to terminal Metadosis.
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
        assert!(
            !wallet_claimable.is_zero(),
            "the next UTC day must pay the previous day's WAA activity"
        );
        assert!(
            !sra_claimable.is_zero(),
            "the next UTC day must pay the previous day's SRA activity"
        );
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
            wallet_claimable + sra_claimable,
            "the AgentReward native balance must back every new claim"
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
        let metadosis_amount = day_zero_allocation(EmissionSinkId::Metadosis);
        let agent_terminal = day_zero_allocation_sum(&[
            EmissionSinkId::Waa,
            EmissionSinkId::Sra,
            EmissionSinkId::Cca,
        ]);
        let rewards = ctx.storage.contract::<outbe_rewards::schema::Rewards<'_>>();
        let planned = rewards
            .reward_gem_planned_load_amount
            .read(&20_240_101)
            .unwrap();
        let gem = outbe_gem::GemContract::new(ctx.storage.clone());
        for voter in voters {
            assert_eq!(
                gem.balance_of(voter).unwrap(),
                0,
                "Cycle prepares the batch but does not mint Gems"
            );
        }
        let formation = outbe_metadosis::api::day_limit_formation_receipt(
            ctx.storage.clone(),
            outbe_primitives::time::WorldwideDay::new(20_240_101),
        )
        .unwrap()
        .unwrap();
        let outbe_metadosis::DayLimitFormationReceipt::Formed(formed) = formation;
        let validator_terminal = formed
            .base_limit
            .checked_sub(metadosis_amount)
            .and_then(|amount| amount.checked_sub(agent_terminal))
            .unwrap();

        assert_eq!(
            planned.checked_add(validator_terminal).unwrap(),
            validator_amount,
            "prepared Gem liability plus terminal residue must conserve the validator allocation"
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
        assert!(rewards.daily_topup_prepared.read(&20_240_101).unwrap());
        assert!(rewards.daily_topup_settled.read(&20_240_101).unwrap());
        assert!(rewards.daily_settled.read(&20_240_101).unwrap());
        assert_eq!(rewards.reward_gem_queue_head.read().unwrap(), 0);
        assert_eq!(rewards.reward_gem_queue_tail.read().unwrap(), 0);
        let gem = outbe_gem::GemContract::new(ctx.storage.clone());
        for voter in voters {
            assert_eq!(gem.balance_of(voter).unwrap(), 0, "no Gem may be minted");
        }

        let expected_terminal = day_zero_allocation_sum(&[
            EmissionSinkId::Metadosis,
            EmissionSinkId::Validator,
            EmissionSinkId::Waa,
            EmissionSinkId::Sra,
            EmissionSinkId::Cca,
        ]);
        let receipt = outbe_metadosis::api::day_limit_formation_receipt(
            ctx.storage.clone(),
            outbe_primitives::time::WorldwideDay::new(20_240_101),
        )
        .unwrap()
        .unwrap();
        let outbe_metadosis::DayLimitFormationReceipt::Formed(formed) = receipt;
        assert_eq!(formed.base_limit, expected_terminal);
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
        assert!(!rewards.daily_topup_prepared.read(&20_240_101).unwrap());
        assert!(!rewards.daily_topup_settled.read(&20_240_101).unwrap());
        assert!(!rewards.daily_settled.read(&20_240_101).unwrap());
        assert_eq!(rewards.reward_gem_queue_head.read().unwrap(), 0);
        assert_eq!(rewards.reward_gem_queue_tail.read().unwrap(), 0);
        assert!(outbe_metadosis::api::day_limit_formation_receipt(
            fire.storage.clone(),
            outbe_primitives::time::WorldwideDay::new(20_240_101),
        )
        .unwrap()
        .is_none());
        let gem = outbe_gem::GemContract::new(fire.storage.clone());
        for voter in TOPUP_VOTERS {
            assert_eq!(gem.balance_of(voter).unwrap(), 0, "Gem mint must roll back");
        }
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
        assert!(rewards.daily_topup_prepared.read(&20_240_101).unwrap());
        assert!(!rewards.daily_topup_settled.read(&20_240_101).unwrap());
        assert!(rewards.daily_settled.read(&20_240_101).unwrap());
        assert_eq!(rewards.reward_gem_queue_head.read().unwrap(), 0);
        assert_eq!(rewards.reward_gem_queue_tail.read().unwrap(), 1);
        let receipt = outbe_metadosis::api::day_limit_formation_receipt(
            retry.storage.clone(),
            outbe_primitives::time::WorldwideDay::new(20_240_101),
        )
        .unwrap()
        .unwrap();

        let validator_amount = day_zero_allocation(EmissionSinkId::Validator);
        let expected_promis_load = validator_amount / U256::from(TOPUP_VOTERS.len());
        let distributed = expected_promis_load * U256::from(TOPUP_VOTERS.len());
        let validator_residue = validator_amount.checked_sub(distributed).unwrap();
        let cca_pool = day_zero_allocation(EmissionSinkId::Cca);
        let expected_terminal = day_zero_allocation_sum(&[
            EmissionSinkId::Metadosis,
            EmissionSinkId::Waa,
            EmissionSinkId::Sra,
        ])
        .checked_add(validator_residue)
        .unwrap();
        let outbe_metadosis::DayLimitFormationReceipt::Formed(formed) = receipt;
        assert_eq!(formed.base_limit, expected_terminal);
        assert_eq!(
            retry
                .storage
                .balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS)
                .unwrap(),
            outbe_ccaregistry::constants::BOND_REQUIREMENT,
            "CCA custody holds only the bond"
        );
        assert_eq!(
            retry
                .storage
                .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                .unwrap(),
            outbe_primitives::units::checked_protocol_to_native(cca_pool).unwrap(),
            "retry must credit CCA backing to AgentReward exactly once"
        );
        let gem = outbe_gem::GemContract::new(retry.storage.clone());
        for voter in TOPUP_VOTERS {
            assert_eq!(
                gem.balance_of(voter).unwrap(),
                0,
                "Cycle retry prepares exactly once; delivery owns Gem creation"
            );
        }
        assert_eq!(
            rewards
                .reward_gem_planned_load_amount
                .read(&20_240_101)
                .unwrap(),
            expected_promis_load * U256::from(TOPUP_VOTERS.len())
        );
        assert_eq!(last_executed_at(&retry), GENESIS_TS + SECONDS_PER_DAY);
    });
}

#[test]
fn open_day_preserves_an_already_delivered_validator_batch_without_reminting() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        genesis_block(handle.clone(), GENESIS_TS + 60);
        let ctx = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);

        let voter = Address::repeat_byte(0x41);
        seed_fresh_reward_oracle(&ctx);
        seed_daily_voters(&ctx, 20_240_101, &[(voter, 1)]);

        let validator_amount = outbe_emissionlimit::allocation::allocate_emission(
            outbe_emissionlimit::day_emission::day_emission_limit(0),
        )
        .unwrap()
        .into_iter()
        .find(|allocation| allocation.id == EmissionSinkId::Validator)
        .unwrap()
        .amount;
        let outcome = outbe_rewards::api::prepare_daily_validator_gem_batch(
            &ctx,
            20_240_101,
            validator_amount,
            &[(voter, 1)],
        )
        .unwrap();
        assert!(matches!(
            outcome,
            outbe_rewards::api::RewardGemPreparationOutcome::Prepared(_)
        ));
        outbe_rewards::api::deliver_oldest_reward_gem_batch(&ctx).unwrap();

        let gem = outbe_gem::GemContract::new(ctx.storage.clone());
        assert_eq!(gem.balance_of(voter).unwrap(), 1);
        let gem_id = gem.token_of_owner_by_index(voter, 0).unwrap();
        let load_before = outbe_gem::api::get_gem(&ctx.storage, gem_id)
            .unwrap()
            .unwrap()
            .promis_load_minor;

        let rewards = ctx.storage.contract::<outbe_rewards::schema::Rewards<'_>>();
        run_emission_limit_daily(&ctx).unwrap();

        assert_eq!(gem.balance_of(voter).unwrap(), 1, "top-up must not remint");
        assert_eq!(
            outbe_gem::api::get_gem(&ctx.storage, gem_id)
                .unwrap()
                .unwrap()
                .promis_load_minor,
            load_before,
            "the prior Gem must remain unchanged"
        );
        assert!(rewards.daily_settled.read(&20_240_101).unwrap());
        let receipt = outbe_metadosis::api::day_limit_formation_receipt(
            ctx.storage.clone(),
            outbe_primitives::time::WorldwideDay::new(20_240_101),
        )
        .unwrap()
        .unwrap();
        let expected_terminal = day_zero_allocation_sum(&[
            EmissionSinkId::Metadosis,
            EmissionSinkId::Waa,
            EmissionSinkId::Sra,
            EmissionSinkId::Cca,
        ]);
        let outbe_metadosis::DayLimitFormationReceipt::Formed(formed) = receipt;
        assert_eq!(
            formed.base_limit, expected_terminal,
            "AlreadySettled must contribute no second validator top-up to the terminal sink"
        );
    });
}

/// a second `run_emission_limit_daily` invocation for an already-settled
/// `prev_day` is a no-op. The handler does NOT mint the CCA agent pool (and
/// terminal Metadosis) twice. Guards the per-day idempotency added on top of the
/// C-01 timestamp drift band.
#[test]
fn emission_dispatch_is_idempotent_per_prev_day() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        anchor_with(handle.clone(), GENESIS_TS + 60, |anchor| {
            seed_reward_cca(&anchor.storage)
        });

        let ctx = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        account_parent(&ctx, 2);

        // First settlement of prev_day = 20240101: mints the pools + seals.
        run_emission_limit_daily(&ctx).unwrap();
        let rewards = ctx.storage.contract::<outbe_rewards::schema::Rewards<'_>>();
        assert!(
            rewards.daily_settled.read(&20_240_101).unwrap(),
            "first fire must seal prev_day"
        );
        let cca_after_first = ctx
            .storage
            .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
            .unwrap();
        let metadosis_after_first = ctx
            .storage
            .balance(outbe_primitives::addresses::METADOSIS_ADDRESS)
            .unwrap();
        assert!(!cca_after_first.is_zero(), "first fire credited CCA");

        // Second invocation for the SAME prev_day: the idempotency guard sees
        // `daily_settled[20240101] == true` and returns early. No double-mint occurs.
        run_emission_limit_daily(&ctx).unwrap();
        assert_eq!(
            ctx.storage
                .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
                .unwrap(),
            cca_after_first,
            "CCA pool must not be minted twice for the same prev_day"
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
        assert!(matches!(
            outbe_metadosis::api::day_limit_formation_receipt(
                fire.storage.clone(),
                outbe_primitives::time::WorldwideDay::new(20_240_101),
            )
            .unwrap(),
            Some(outbe_metadosis::DayLimitFormationReceipt::Formed(_))
        ));
    });

    assert_storage_unchanged(&mut storage, |handle| {
        let replay =
            BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        run_emission_limit_daily(&replay).unwrap();
    });
}

#[test]
fn settled_cycle_marker_without_metadosis_semantic_receipt_is_fatal() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        outbe_rewards::api::mark_day_settled(&ctx, 20_240_101).unwrap();
        assert!(matches!(
            run_emission_limit_daily(&ctx),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
    });
}

#[test]
fn metadosis_semantic_receipt_without_settled_cycle_marker_is_fatal_before_effects() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        outbe_metadosis::commands::apply_cycle_day_limit(&ctx, U256::from(17_u8)).unwrap();
    });

    assert_storage_unchanged(&mut storage, |handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        let parent_storage: StorageReaderHandle = Arc::new(MemoryStorage::new());
        let parent = TributeRepositoryReader::new(parent_storage);
        let scope = ExecutionScope::default();
        assert!(matches!(
            crate::handler::run_emission_limit_daily(&ctx, &scope, &parent),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
    });
}
