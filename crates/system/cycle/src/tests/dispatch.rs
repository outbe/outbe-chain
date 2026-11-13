//! Trigger dispatch, emission settlement and validator top-up conservation.

use alloy_sol_types::SolEvent;
use outbe_emission::allocation::EmissionSinkId;
use outbe_primitives::addresses::CYCLE_ADDRESS;
use outbe_primitives::error::PrecompileError;

use super::*;

#[test]
fn cycle_tick_runs_dispatcher() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let block_ts = GENESIS_TS + 60;
        let ctx = genesis_block(handle, block_ts);

        run_cycle_tick(&ctx).unwrap();

        // Same as `first_encounter_anchors_without_firing`: handle_cycle_tick
        // delegates to dispatch_triggers.
        assert_eq!(last_executed_at(&ctx), block_ts);
    });
}

// ---------------------------------------------------------------------------
// advance_auctions trigger
// ---------------------------------------------------------------------------

#[test]
fn dispatcher_fires_advance_auctions_at_its_slot() {
    with_anchored_cycle(|handle| {
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 5;
        let ctx_fire = dispatch_at(handle, 2, fire_ts);

        let advance_auctions_id = TriggerId::AdvanceAuctions.as_u32();
        let cycle: Cycle<'_> = ctx_fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&advance_auctions_id).unwrap(),
            GENESIS_TS + SECONDS_PER_DAY + PROTOCOL_CYCLE_START_OFFSET_SECONDS,
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
fn protocol_cycle_keeps_midnight_slot_pending_until_late_reward_window_closes() {
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
    for height in 11..=13 {
        storage.set_block_number(height);
        storage.enter(|handle| {
            let ctx = runtime_context(
                block_ctx(height, GENESIS_TS + SECONDS_PER_DAY + height - 11),
                handle,
            );
            account_parent(&ctx, height);
            dispatch_triggers(&ctx).unwrap();
            let cycle = ctx.storage.contract::<Cycle>();
            assert_eq!(active_utc_day(&ctx), 20240101);
            assert_eq!(
                cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
                GENESIS_TS + 60
            );
            assert!(!is_day_settled(&ctx, 20240101));
            if height == 13 {
                // Real phase order: Cycle first, then final-slot credit and GC.
                outbe_rewards::block_fees::record_late_vote(&ctx, hash, late, 3).unwrap();
                ctx.storage
                    .with_checkpoint(|| outbe_rewards::block_fees::seal_block_votes(&ctx, 13, 3))
                    .unwrap();
            }
        });
    }
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    storage.set_block_number(14);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle, 14, GENESIS_TS + SECONDS_PER_DAY + 3);
        crate::handler::credit_reward_slice(&ctx).unwrap();
        assert!(is_day_settled(&ctx, 20240101));
        let cycle = ctx.storage.contract::<Cycle>();
        assert_eq!(active_utc_day(&ctx), 20240102);
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

        // Step 2: block past first slot. prev_day = start_date
        // (20240101). day_number_since_genesis = 0. cap = INITIAL_DAY_EMISSION.
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 60;
        let ctx_fire = dispatch_at(handle, 2, fire_ts);

        assert!(
            is_day_settled(&ctx_fire, 20_240_101),
            "Cycle handler must seal prev_day"
        );

        // Cycle's last_executed_at advanced to the slot
        // (GENESIS_TS + 86_400 + 1), not the block timestamp.
        assert_eq!(
            last_executed_at(&ctx_fire),
            GENESIS_TS + SECONDS_PER_DAY + PROTOCOL_CYCLE_START_OFFSET_SECONDS
        );

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
fn every_block_credits_a_queued_reward_slice_between_hourly_cycles() {
    use outbe_agentreward::api::{allocate_day_agent_rewards, DayAgentPools};
    use outbe_agentreward::constants::ACCRUAL_RECIPIENTS_PER_BLOCK;
    use outbe_agentreward::schema::RewardPool;

    with_anchored_cycle(|handle| {
        let day = outbe_primitives::time::WorldwideDay::new(20_231_231);
        let slice = u64::from(ACCRUAL_RECIPIENTS_PER_BLOCK);
        let recipients = slice + 4;
        let recipient = |n: u64| Address::from_word(U256::from(n + 1).into());
        let mut agent_reward = outbe_agentreward::AgentRewardContract::new(handle.clone());
        for n in 0..recipients {
            agent_reward
                .increment_waa_tribute(day, recipient(n))
                .unwrap();
        }
        let claimable = |n: u64| {
            outbe_agentreward::AgentRewardContract::new(handle.clone())
                .get_pool_claimable_reward(RewardPool::Waa, recipient(n))
                .unwrap()
        };

        let mut block = block_ctx(2, ANCHOR_TS + 10);
        block.genesis_timestamp = GENESIS_TS - SECONDS_PER_DAY;
        let ctx = runtime_context(block, handle.clone());
        outbe_rewards::api::seal_votes_and_fees(&ctx, day.value()).unwrap();
        let amount = U256::from(recipients * 1_000);
        allocate_day_agent_rewards(
            &ctx,
            day,
            DayAgentPools {
                waa: amount,
                ..Default::default()
            },
        )
        .unwrap();
        outbe_emission::api::record_day_allocation(
            handle.clone(),
            day.value(),
            outbe_emission::api::DayAllocationAmounts {
                emission_limit: amount,
                metadosis_limit: U256::ZERO,
                validators: U256::ZERO,
                waa: amount,
                sra: U256::ZERO,
                cca: U256::ZERO,
                promis_limit: U256::ZERO,
            },
        )
        .unwrap();
        run_cycle_tick(&ctx).unwrap();
        assert_eq!(claimable(slice - 1), U256::from(1_000u64));
        assert_eq!(claimable(slice), U256::ZERO);
        let (failed, returned) =
            outbe_emission::api::record_metadosis_failed(handle.clone(), day.value()).unwrap();
        assert_eq!(returned, U256::ZERO);
        assert_eq!(
            failed.state,
            outbe_emission::schema::emission_day_state::PROCESSING
        );

        let ctx = runtime_context(block_ctx(3, ANCHOR_TS + 20), handle.clone());
        run_cycle_tick(&ctx).unwrap();
        assert_eq!(claimable(recipients - 1), U256::from(1_000u64));
        let settled = outbe_emission::api::get_emission_day_record(handle.clone(), day.value())
            .unwrap()
            .unwrap();
        assert_eq!(
            settled.state,
            outbe_emission::schema::emission_day_state::COMPLETED
        );
        assert_eq!(U256::from(settled.waa), amount);
        assert_eq!(settled.promis_limit, 0);
        assert_eq!(
            agent_reward.waa_address_count.read(&day).unwrap(),
            recipients as u32
        );
        let ctx = runtime_context(block_ctx(4, ANCHOR_TS + 30), handle.clone());
        run_cycle_tick(&ctx).unwrap();
        assert_eq!(agent_reward.waa_address_count.read(&day).unwrap(), 0);
        assert_eq!(
            last_executed_at(&ctx),
            ANCHOR_TS,
            "the hourly cycle did not fire"
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
        crate::handler::credit_reward_slice(&ctx_fire).unwrap();

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

        run_emission_limit_daily(&ctx).unwrap();
        crate::handler::credit_reward_slice(&ctx).unwrap();

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
        for voter in voters {
            assert_eq!(
                outbe_gem::api::balance_of(&ctx.storage, voter).unwrap(),
                0,
                "Cycle credits claims but does not mint Gems"
            );
        }
        let expected_each = validator_amount / U256::from(voters.len());
        assert_eq!(
            planned,
            expected_each * U256::from(voters.len()),
            "credited claims must equal distributed validator pool"
        );
    });
    assert!(
        storage
            .get_events(outbe_primitives::addresses::GEM_ADDRESS)
            .is_empty(),
        "Cycle credits claims but does not mint Gems"
    );
}

#[test]
fn zero_total_validator_participation_routes_the_pool_without_halting() {
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

        assert!(is_day_settled(&ctx, 20_240_101));
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

/// Returns the slot that trigger `id` records at `TOPUP_FIRE_TS`.
/// The slot is the day boundary in production. Under `e2e-test`, a period of
/// 60 s gives a later slot.
fn topup_slot(id: TriggerId) -> u64 {
    let spec = ACTIVE_TRIGGERS
        .iter()
        .find(|spec| spec.id == id.as_u32())
        .expect("active trigger");
    assert!(
        spec.coalesces_backlog,
        "the trigger records its latest slot"
    );
    last_fire_at(
        spec.period_seconds(),
        spec.start_offset_seconds,
        TOPUP_FIRE_TS,
    )
    .unwrap()
}

/// Anchors with a bonded CCA, then runs the first settlement dispatch without
/// Metadosis mutation frames, so the terminal sink reverts. The slot is consumed.
fn failed_terminal_dispatch() -> HashMapStorageProvider {
    let (storage, dispatched) = terminal_dispatch_after(|_| {});
    dispatched.unwrap();
    storage
}

/// Runs the dispatch of [`failed_terminal_dispatch`] with `before_dispatch` run after the
/// fixture writes, and returns its result.
fn terminal_dispatch_after(
    before_dispatch: impl FnOnce(&BlockRuntimeContext<'_>),
) -> (HashMapStorageProvider, outbe_primitives::error::Result<()>) {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        anchor_with(handle, GENESIS_TS + 60, |anchor| {
            seed_reward_cca(&anchor.storage)
        });
    });

    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 0);
    storage.set_block_number(2);
    let dispatched = storage.enter(|handle| {
        let fire = runtime_context(block_ctx(2, TOPUP_FIRE_TS), handle);
        account_parent(&fire, 2);
        seed_fresh_reward_oracle(&fire);
        seed_daily_voters(&fire, 20_240_101, &TOPUP_VOTERS.map(|voter| (voter, 1)));
        before_dispatch(&fire);
        dispatch_triggers(&fire)
    });
    (storage, dispatched)
}

/// Writes an unknown state into the record of `day`, so the allocation fallback fails too.
fn corrupt_emission_day(ctx: &BlockRuntimeContext<'_>, day: u32) {
    let emission = outbe_emission::schema::EmissionContract::new(ctx.storage.clone());
    match emission.emission_days.get(day).unwrap() {
        Some(mut record) => {
            record.state = 7;
            emission.emission_days.update(&record).unwrap();
        }
        None => {
            let mut record = outbe_emission::schema::EmissionDay::new_pending(
                day,
                outbe_primitives::time::WorldwideDay::new(0),
                1,
            );
            record.state = 7;
            emission.emission_days.create(&record).unwrap();
        }
    }
}

fn cycle_failure(
    storage: &HashMapStorageProvider,
    id: u32,
) -> Option<crate::ICycle::CycleTriggerFailed> {
    storage.get_events(CYCLE_ADDRESS).iter().find_map(|log| {
        crate::ICycle::CycleTriggerFailed::decode_log_data(log)
            .ok()
            .filter(|event| event.id == id)
    })
}

fn cycle_executed(
    storage: &HashMapStorageProvider,
    id: u32,
) -> Option<crate::ICycle::CycleTriggerExecuted> {
    storage.get_events(CYCLE_ADDRESS).iter().find_map(|log| {
        crate::ICycle::CycleTriggerExecuted::decode_log_data(log)
            .ok()
            .filter(|event| event.id == id)
    })
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
fn failed_terminal_dispatch_takes_the_allocation_fallback() {
    let mut storage = failed_terminal_dispatch();
    let recorded_at = topup_slot(TriggerId::ProtocolCycle);
    for id in [
        EMISSION_LIMIT_1_ID,
        TriggerId::AdvanceAuctions.as_u32(),
        TriggerId::CompleteAuctions.as_u32(),
        TriggerId::ScheduleIntexCallPass.as_u32(),
    ] {
        assert!(cycle_failure(&storage, id).is_none());
        assert!(cycle_executed(&storage, id).is_some());
    }
    let failed = storage
        .get_events(CYCLE_ADDRESS)
        .iter()
        .find_map(|log| crate::ICycle::DayEmissionLimitAllocationFailed::decode_log_data(log).ok())
        .expect("allocation failure event");
    assert_eq!(failed.emissionDay, 20_240_101);
    storage.enter(|handle| {
        let fire = runtime_context(block_ctx(2, TOPUP_FIRE_TS), handle);
        assert!(is_day_settled(&fire, 20_240_101));
        let record = outbe_emission::api::get_emission_day_record(fire.storage.clone(), 20_240_101)
            .unwrap()
            .unwrap();
        assert_eq!(
            record.state,
            outbe_emission::schema::emission_day_state::COMPLETED
        );
        assert_eq!(
            record.metadosis_state,
            outbe_emission::schema::metadosis_state::CANCELLED
        );
        assert_eq!(U256::from(record.promis_limit), failed.emissionLimitMinor);
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
            recorded_at
        );
        for id in [
            TriggerId::AdvanceAuctions,
            TriggerId::CompleteAuctions,
            TriggerId::ScheduleIntexCallPass,
        ] {
            assert_eq!(
                cycle.last_executed_at.read(&id.as_u32()).unwrap(),
                topup_slot(id)
            );
        }
    });
}

#[test]
fn failed_terminal_dispatch_is_not_retried() {
    let mut storage = failed_terminal_dispatch();
    let settled = storage.enter(|handle| {
        outbe_emission::api::get_emission_day_record(handle, 20_240_101)
            .unwrap()
            .unwrap()
    });
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    let next_window = GENESIS_TS
        + SECONDS_PER_DAY
        + crate::triggers::PROTOCOL_CYCLE_PERIOD_SECONDS
        + PROTOCOL_CYCLE_START_OFFSET_SECONDS;
    storage.set_block_number(3);
    storage.enter(|handle| {
        let retry = runtime_context(block_ctx(3, next_window), handle);
        account_parent(&retry, 3);
        dispatch_triggers(&retry).unwrap();
        assert_eq!(
            outbe_emission::api::get_emission_day_record(retry.storage.clone(), 20_240_101)
                .unwrap()
                .unwrap(),
            settled
        );
        assert_eq!(topup_voters_claimable(&retry), U256::ZERO);
        assert_eq!(last_executed_at(&retry), next_window);
    });
    let failures = storage
        .get_events(CYCLE_ADDRESS)
        .iter()
        .filter(|log| crate::ICycle::DayEmissionLimitAllocationFailed::decode_log_data(log).is_ok())
        .count();
    assert_eq!(failures, 1);
}

#[test]
fn a_failing_allocation_fallback_fails_the_block() {
    let (_, dispatched) = terminal_dispatch_after(|fire| corrupt_emission_day(fire, 20_240_101));
    let err = dispatched.expect_err("the failed fallback must fail the block");
    assert!(matches!(err, PrecompileError::FatalWithCause(_)), "{err:?}");
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
        let err = dispatch_triggers(&fire).expect_err("invalid day must fail the block");
        assert!(matches!(err, PrecompileError::Fatal(_)));
        assert_eq!(last_executed_at(&fire), GENESIS_TS + 60);
    });
    assert!(cycle_failure(&storage, EMISSION_LIMIT_1_ID).is_none());
}

#[test]
fn storage_error_from_the_handler_does_not_consume_the_slot() {
    let mut storage = prepared_settlement_storage();
    storage.fail_mutation_at(0);
    storage.enter(|handle| {
        let fire = runtime_context(block_ctx(2, TOPUP_FIRE_TS), handle);
        let err = dispatch_triggers(&fire).expect_err("storage failure must fail the block");
        assert!(matches!(err, PrecompileError::Storage(_)));
        assert_eq!(last_executed_at(&fire), GENESIS_TS + 60);
        assert!(!is_day_settled(&fire, 20_240_101));
    });
    assert!(cycle_failure(&storage, EMISSION_LIMIT_1_ID).is_none());
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

        // First settlement of prev_day = 20240101: credits the pools + seals.
        run_emission_limit_daily(&ctx).unwrap();
        crate::handler::credit_reward_slice(&ctx).unwrap();
        assert!(
            is_day_settled(&ctx, 20_240_101),
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
