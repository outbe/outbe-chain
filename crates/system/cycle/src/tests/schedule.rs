//! Dispatcher schedule math and first-encounter anchoring.

use super::*;

// ---------------------------------------------------------------------------
// next_fire_at - pure scheduling math
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
    let mut storage = cycle_storage();
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
    // Production regression: at block 1 the daily Cycle trigger only anchors.
    // It never invokes `start_metadosis`. So `CycleLifecycle::begin_block`
    // must itself create the genesis metadosis worldwide day. Before the fix
    // the active-WWD set was empty until the first block past the next UTC
    // midnight.
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let block_ts = GENESIS_TS + 60;
        let ctx = BlockRuntimeContext::new(block_ctx(1, block_ts), handle);
        anchor_genesis(&ctx);

        // Sanity: no worldwide day exists before begin_block.
        assert!(outbe_metadosis::api::has_active_ocomp_profile(ctx.storage.clone()).unwrap());
        assert!(
            outbe_metadosis::api::worldwide_days(ctx.storage.clone())
                .unwrap()
                .is_empty(),
            "no worldwide day should exist before block-1 begin_block"
        );

        run_cycle_lifecycle(&ctx).unwrap();

        assert!(
            !outbe_metadosis::api::worldwide_days(ctx.storage.clone())
                .unwrap()
                .is_empty(),
            "block-1 begin_block must create the genesis worldwide day"
        );

        // The daily trigger must only have anchored - no settlement fired.
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
fn block_1_begin_block_rejects_missing_genesis_ocomp_profile_without_partial_state() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    storage.enter(|handle| {
        handle
            .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
            .unwrap();
        handle
            .sstore(
                COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_slice(
                    outbe_compressed_entities::sealed_root(B256::ZERO)
                        .unwrap()
                        .as_slice(),
                ),
            )
            .unwrap();
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        anchor_genesis(&ctx);
    });
    let storage_before = storage.storage.clone();
    let events_before = storage.events.clone();

    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        assert!(run_cycle_lifecycle(&ctx).is_err());
    });

    assert_eq!(storage.storage, storage_before);
    assert_eq!(storage.events, events_before);
}

#[test]
fn frozen_final_profile_initializes_metadosis_at_its_existing_activation_height() {
    let mut storage = cycle_storage();

    storage.enter(|handle| {
        let block_1 = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle.clone());
        anchor_genesis(&block_1);
        run_cycle_lifecycle_at_activation(&block_1, 32).unwrap();

        assert!(
            outbe_metadosis::api::worldwide_days(block_1.storage.clone())
                .unwrap()
                .is_empty(),
            "the frozen Final/32 evidence profile must not initialize Metadosis at block 1"
        );

        let activation =
            BlockRuntimeContext::new(block_ctx(32, GENESIS_TS + 31 * 60), handle.clone());
        run_cycle_lifecycle_at_activation(&activation, 32).unwrap();

        assert!(
            !outbe_metadosis::api::worldwide_days(activation.storage.clone())
                .unwrap()
                .is_empty(),
            "the existing Final/32 evidence profile must initialize Metadosis after its fork install"
        );
    });
}

#[test]
fn does_not_fire_before_next_slot_after_anchor() {
    // Anchor at 00:01 UTC. The first aligned hourly slot is 01:00 UTC.
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        // Block at 00:59:59 UTC - still before the next slot.
        let ctx_before =
            BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + 3_600 - 1), handle.clone());
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
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        // Step 1: anchor.
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        // Step 2: first block past the next aligned hour.
        let fire_ts = GENESIS_TS + 3_600 + 5;
        let ctx_fire = BlockRuntimeContext::new(block_ctx(2, fire_ts), handle);
        account_parent(&ctx_fire, 2);
        dispatch_triggers(&ctx_fire).unwrap();

        let cycle: Cycle<'_> = ctx_fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            GENESIS_TS + 3_600,
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
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        let fire_ts = GENESIS_TS + 3_600 + 60;
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
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 10);
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        // Missed hourly scheduler slots collapse to one execution at the latest
        // due boundary. Calendar policy then forfeits this multi-day gap.
        let ctx_fire =
            BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + 4 * SECONDS_PER_DAY), handle);
        account_parent(&ctx_fire, 2);
        dispatch_triggers(&ctx_fire).unwrap();

        let cycle: Cycle<'_> = ctx_fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            GENESIS_TS + 4 * SECONDS_PER_DAY,
            "multi-slot gap fires once at the latest due hourly slot"
        );
    });
}
