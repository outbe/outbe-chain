//! Cycle dispatcher tests.
//!
//! `next_fire_at` is asserted in `schedule_math_*`. Integration tests
//! exercise the dispatcher loop against the `HashMapStorageProvider`
//! so they cover the storage round-trip (`Cycle.last_executed_at`)
//! and the genesis-anchor interaction with Rewards.
//!
//! The dispatcher uses a lazy first-encounter anchor: on the very
//! first block it sees a trigger, it writes
//! `last_executed_at = block_ts` instead of firing. This anchors the
//! schedule at the chain's deployment instant so the first real fire
//! happens at the *next* slot strictly after that anchor. Without
//! this, every chain would fire its daily trigger on block 1 because
//! `block_ts >> 86_400` is always true on a real chain.

use alloy_primitives::{Address, U256};
use outbe_primitives::block::{BlockContext, BlockLifecycle, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;

use crate::lifecycle::CycleLifecycle;
use crate::runtime::dispatch_triggers;
use crate::schema::Cycle;
use crate::triggers::{next_fire_at, TriggerId};

const CHAIN_ID: u64 = 1;
/// Genesis at midnight UTC of 2024-01-01.
const GENESIS_TS: u64 = 1_704_067_200;
const SECONDS_PER_DAY: u64 = 86_400;
const EMISSION_LIMIT_1_ID: u32 = TriggerId::EmissionLimit1.as_u32();

fn block_ctx(block_number: u64, timestamp: u64) -> BlockContext {
    BlockContext::new(block_number, timestamp, CHAIN_ID, Address::ZERO, Vec::new())
}

fn anchor_genesis(ctx: &BlockRuntimeContext) {
    outbe_rewards::runtime::ensure_genesis_anchor(ctx).unwrap();
}

/// seed V2 Phase 1 accounting progress so the dispatcher's
/// new gate (`last_accounted_block_number >= block_number - 1`) is
/// satisfied for tests that fire the trigger at `block_number >= 2`.
/// Mirrors what `apply_phase1_commit_in_preexec` records in production.
fn account_parent(ctx: &BlockRuntimeContext, block_number: u64) {
    if block_number >= 2 {
        outbe_accounting::record_phase1_progress(ctx, block_number - 1).unwrap();
    }
}

// ---------------------------------------------------------------------------
// next_fire_at — pure scheduling math
// ---------------------------------------------------------------------------

#[test]
fn schedule_math_pinned_values() {
    // Daily, offset = 0 => first slot is `period_seconds`.
    assert_eq!(next_fire_at(86_400, 0, 0), 86_400);
    // Hourly @ :30 (offset = 1800), first slot at 1800.
    assert_eq!(next_fire_at(3_600, 1_800, 0), 1_800);
    // Hourly @ :30, last fired at 1800 => next at 5400.
    assert_eq!(next_fire_at(3_600, 1_800, 1_800), 5_400);
    // 5-minute, offset = 0, last fired at 299 => next at 300.
    assert_eq!(next_fire_at(300, 0, 299), 300);
    // 5-minute, offset = 0, last fired at 300 => next at 600.
    assert_eq!(next_fire_at(300, 0, 300), 600);
    // last well past first slot.
    assert_eq!(next_fire_at(86_400, 0, 86_400 * 5), 86_400 * 6);
}

#[test]
fn schedule_math_aligned_property() {
    // (next - offset) % period == 0 for arbitrary inputs.
    for &period in &[60u64, 300, 3_600, 86_400] {
        for &offset in &[0u64, 1, 7, period - 1] {
            for &last in &[0u64, 1, 100, 86_400, 86_400 * 365] {
                let next = next_fire_at(period, offset, last);
                assert!(next > last, "p={period} o={offset} l={last} n={next}");
                assert!(next >= offset);
                assert_eq!((next - offset) % period, 0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatcher: lazy first-encounter anchor + slot-based fire
// ---------------------------------------------------------------------------

#[test]
fn first_encounter_anchors_without_firing() {
    // First time the dispatcher sees the trigger, it anchors
    // `last_executed_at = block_ts` and skips firing. No event, no
    // handler invocation, no settle.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let block_ts = GENESIS_TS + 60;
        let ctx = BlockRuntimeContext::new(block_ctx(1, block_ts), handle);
        anchor_genesis(&ctx);

        dispatch_triggers(&ctx).unwrap();

        let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            block_ts,
            "first encounter anchors at block timestamp"
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&EMISSION_LIMIT_1_ID)
                .unwrap(),
            0,
            "no fire = no last_executed_block_number write"
        );
    });
}

#[test]
fn block_1_begin_block_creates_genesis_worldwide_day() {
    // Production regression: at block 1 the daily Cycle trigger only anchors
    // (it never invokes `start_metadosis`), so `CycleLifecycle::begin_block`
    // must itself create the genesis metadosis worldwide day. Before the fix
    // the active-WWD set was empty until the first block past the next UTC
    // midnight.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let block_ts = GENESIS_TS + 60;
        let ctx = BlockRuntimeContext::new(block_ctx(1, block_ts), handle);
        anchor_genesis(&ctx);

        // Sanity: no worldwide day exists before begin_block.
        let before = outbe_metadosis::schema::MetadosisContract::new(ctx.storage.clone());
        assert!(
            before.get_all_active_wwds().unwrap().is_empty(),
            "no worldwide day should exist before block-1 begin_block"
        );
        drop(before);

        CycleLifecycle::begin_block(&ctx).unwrap();

        let metadosis = outbe_metadosis::schema::MetadosisContract::new(ctx.storage.clone());
        assert!(
            !metadosis.get_all_active_wwds().unwrap().is_empty(),
            "block-1 begin_block must create the genesis worldwide day"
        );

        // The daily trigger must only have anchored — no settlement fired.
        let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            block_ts,
            "daily trigger still only anchors on block 1"
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&EMISSION_LIMIT_1_ID)
                .unwrap(),
            0,
            "daily settlement must not fire on block 1"
        );
    });
}

#[test]
fn does_not_fire_before_next_slot_after_anchor() {
    // Anchor at GENESIS_TS + 60. Next slot at 86_400 (UTC midnight
    // 1970-01-02 — already passed) → next > anchor → next slot is the
    // first multiple of 86_400 strictly greater than (GENESIS_TS + 60).
    // GENESIS_TS = 1_704_067_200 = 19723 * 86_400; +60 puts us in the
    // current slot, so next slot = 19724 * 86_400 = GENESIS_TS + 86_400.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        // Block at GENESIS_TS + 86_399 — still BEFORE next slot.
        let ctx_before = BlockRuntimeContext::new(
            block_ctx(2, GENESIS_TS + SECONDS_PER_DAY - 1),
            handle.clone(),
        );
        dispatch_triggers(&ctx_before).unwrap();

        let cycle: Cycle<'_> = ctx_before.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            anchor_ts,
            "trigger must not fire before the next slot"
        );
    });
}

#[test]
fn fires_at_first_block_past_next_slot() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        // Step 1: anchor.
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        // Step 2: block past next slot. Next slot after anchor =
        // ceil(anchor_ts / 86_400) * 86_400 = GENESIS_TS + 86_400.
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 5;
        let ctx_fire = BlockRuntimeContext::new(block_ctx(2, fire_ts), handle);
        account_parent(&ctx_fire, 2);
        dispatch_triggers(&ctx_fire).unwrap();

        let cycle: Cycle<'_> = ctx_fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            GENESIS_TS + SECONDS_PER_DAY,
            "last_executed_at must be the slot, not block.timestamp"
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&EMISSION_LIMIT_1_ID)
                .unwrap(),
            2
        );
    });
}

#[test]
fn does_not_refire_within_same_slot() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 60;
        let ctx_fire = BlockRuntimeContext::new(block_ctx(2, fire_ts), handle.clone());
        account_parent(&ctx_fire, 2);
        dispatch_triggers(&ctx_fire).unwrap();
        let after_first_fire = ctx_fire
            .storage
            .contract::<Cycle<'_>>()
            .last_executed_at
            .read(&EMISSION_LIMIT_1_ID)
            .unwrap();

        // Second block within the same slot.
        let ctx_again = BlockRuntimeContext::new(block_ctx(3, fire_ts + 30), handle);
        account_parent(&ctx_again, 3);
        dispatch_triggers(&ctx_again).unwrap();
        let cycle: Cycle<'_> = ctx_again.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            after_first_fire,
            "trigger must not refire within the same slot"
        );
    });
}

#[test]
fn multi_slot_gap_fires_only_for_latest_slot_after_anchor() {
    // Anchor, then jump 3 slots ahead in one block.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        // Block at GENESIS_TS + 4 days — 3 slots crossed. next_fire_at
        // from anchor is the FIRST slot > anchor = GENESIS_TS + 86_400,
        // even though the block is 3 days later.
        let ctx_fire =
            BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + 4 * SECONDS_PER_DAY), handle);
        account_parent(&ctx_fire, 2);
        dispatch_triggers(&ctx_fire).unwrap();

        let cycle: Cycle<'_> = ctx_fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            GENESIS_TS + SECONDS_PER_DAY,
            "multi-slot gap fires once for the first slot strictly after anchor"
        );
    });
}

#[test]
fn cycle_lifecycle_begin_block_runs_dispatcher() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let block_ts = GENESIS_TS + 60;
        let ctx = BlockRuntimeContext::new(block_ctx(1, block_ts), handle);
        anchor_genesis(&ctx);

        <CycleLifecycle as BlockLifecycle>::begin_block(&ctx).unwrap();

        // Same as `first_encounter_anchors_without_firing`: begin_block
        // delegates to dispatch_triggers.
        let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            block_ts
        );
    });
}

// ---------------------------------------------------------------------------
// End-to-end: handler effects on Rewards, AgentReward, Metadosis
// ---------------------------------------------------------------------------

#[test]
fn end_to_end_emission_dispatch_marks_day_settled_and_credits_metadosis() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        // Step 1: anchor at chain start.
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        // Step 2: block past first slot. prev_day = genesis_utc_day
        // (20240101); day_number_since_genesis = 0; cap = INITIAL_DAY_EMISSION.
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 60;
        let ctx_fire = BlockRuntimeContext::new(block_ctx(2, fire_ts), handle);
        account_parent(&ctx_fire, 2);
        dispatch_triggers(&ctx_fire).unwrap();

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
        let cycle: Cycle<'_> = ctx_fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            GENESIS_TS + SECONDS_PER_DAY
        );

        // No tributes for any AgentReward pool, so all four
        // WAA/SRA/CCA/Merchant amounts are accounted for.
        // burn parity: WAA + SRA pools are pre-funded then burned in
        // their no-tribute branch; CCA + Merchant land on their own
        // accumulator addresses. AGENT_REWARD balance is therefore
        // zero (no claimable was credited).
        let agent_reward_balance = ctx_fire
            .storage
            .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
            .unwrap();
        assert_eq!(agent_reward_balance, U256::ZERO);

        // CCA / Merchant accumulators received their 4 % each. The
        // exact amount comes from `day_emission_limit(0) * 4 / 100`
        // which is fully covered by emissionlimit pinned tests; here
        // we only assert that they are non-zero and equal (both
        // pools share the same percentage).
        let cca = ctx_fire
            .storage
            .balance(outbe_primitives::addresses::CCA_ADDRESS)
            .unwrap();
        let merchant = ctx_fire
            .storage
            .balance(outbe_primitives::addresses::MERCHANT_ADDRESS)
            .unwrap();
        assert!(!cca.is_zero(), "CCA accumulator received its 4 %");
        assert_eq!(
            cca, merchant,
            "CCA and Merchant pools have equal percentage"
        );
    });
}

/// a second `run_emission_limit_daily` invocation for an already-settled
/// `prev_day` is a no-op — the CCA/Merchant agent pools (and terminal Metadosis)
/// are NOT minted twice. Guards the per-day idempotency added on top of the
/// C-01 timestamp drift band.
#[test]
fn emission_dispatch_is_idempotent_per_prev_day() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        let ctx = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        account_parent(&ctx, 2);

        // First settlement of prev_day = 20240101: mints the pools + seals.
        crate::handler::run_emission_limit_daily(&ctx).unwrap();
        let rewards = ctx.storage.contract::<outbe_rewards::schema::Rewards<'_>>();
        assert!(
            rewards.daily_settled.read(&20_240_101).unwrap(),
            "first fire must seal prev_day"
        );
        let cca_after_first = ctx
            .storage
            .balance(outbe_primitives::addresses::CCA_ADDRESS)
            .unwrap();
        let merchant_after_first = ctx
            .storage
            .balance(outbe_primitives::addresses::MERCHANT_ADDRESS)
            .unwrap();
        let metadosis_after_first = ctx
            .storage
            .balance(outbe_primitives::addresses::METADOSIS_ADDRESS)
            .unwrap();
        assert!(!cca_after_first.is_zero(), "first fire credited CCA");

        // Second invocation for the SAME prev_day: the idempotency guard sees
        // `daily_settled[20240101] == true` and returns early — no double-mint.
        crate::handler::run_emission_limit_daily(&ctx).unwrap();
        assert_eq!(
            ctx.storage
                .balance(outbe_primitives::addresses::CCA_ADDRESS)
                .unwrap(),
            cca_after_first,
            "CCA pool must not be minted twice for the same prev_day"
        );
        assert_eq!(
            ctx.storage
                .balance(outbe_primitives::addresses::MERCHANT_ADDRESS)
                .unwrap(),
            merchant_after_first,
            "Merchant pool must not be minted twice for the same prev_day"
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
fn genesis_midday_first_cycle_at_next_midnight_settles_genesis_day() {
    // Genesis at 10:00 UTC on day D. First CycleTick fires at 00:00:01 UTC
    // on day D+1. prev_day = D = genesis_utc_day → day_number = 0 → Ok.
    // This is the production scenario that was broken when genesis_utc_day
    // was derived from block 0 timestamp at 10:00 instead of genesisTime.
    const DAY_D_MIDNIGHT: u64 = GENESIS_TS; // 2024-01-01 00:00:00
    const DAY_D_10AM: u64 = DAY_D_MIDNIGHT + 10 * 3600; // 10:00 UTC

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        // Block 1 at 10:00 — genesis anchor records genesis_utc_day = day D.
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, DAY_D_10AM), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        // Block at 00:00:01 UTC day D+1 — CycleTick fires.
        // prev_day = D = genesis_utc_day → day_number_since_genesis = 0.
        let fire_ts = DAY_D_MIDNIGHT + SECONDS_PER_DAY + 1;
        let ctx_fire = BlockRuntimeContext::new(block_ctx(2, fire_ts), handle);
        account_parent(&ctx_fire, 2);
        dispatch_triggers(&ctx_fire).unwrap();

        let rewards = ctx_fire
            .storage
            .contract::<outbe_rewards::schema::Rewards<'_>>();
        let genesis_day = rewards.genesis_utc_day.read().unwrap();
        assert_eq!(genesis_day, 20_240_101, "genesis_utc_day = day D");
        assert!(
            rewards.daily_settled.read(&genesis_day).unwrap(),
            "CycleTick must settle genesis day (day_number=0)"
        );
    });
}
