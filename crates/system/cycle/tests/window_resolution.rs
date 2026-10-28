//! Accounting-window resolution and Phase 2 gating tests.
//!
//! These integration tests pin these properties:
//!
//! - The period window resolves bitwise-deterministically from headers
//!   (proptest at the bottom).
//! - `last_accounted_block_number` is the single gate signal.
//! - Cycle handler decisions make no wall-clock reads.
//!
//! Each `<period>_boundary_cycle_runs_after_parent_accounted` test
//! covers a different period (hour, day, week, month). The test constructs
//! a synthetic gated `TriggerSpec`. It asserts that `accounting_gate_blocks`
//! returns `false` after the parent height is written to the accounting slot.
//!
//! `phase1_accounts_parent_before_cycle_tick` is a structural check on
//! `crates/blockchain/evm/src/executor.rs`. It confirms the V2 reorder
//! invariant that this gate depends on.

use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolError;
use outbe_cycle::{
    read_last_accounted_block_number, record_phase1_progress,
    schema::Accounting,
    state::{accounting_gate_blocks, resolve_accounting_window, AccountingWindow},
    triggers::{TriggerHandler, TriggerSpec},
    ICycle,
};
use outbe_primitives::{
    accounting_progress::AccountingProgressView,
    addresses::ACCOUNTING_PROGRESS_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    error::{PrecompileError, Result},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use proptest::prelude::*;

const CHAIN_ID: u64 = 1;
const HOUR: u64 = 3_600;
use outbe_primitives::time::SECONDS_PER_DAY as DAY;
const WEEK: u64 = 7 * DAY;
const MONTH: u64 = 30 * DAY;

fn gated_spec(period_seconds: u64) -> TriggerSpec {
    let mut spec = TriggerSpec::new(
        4242,
        "test_gated",
        period_seconds,
        0,
        TriggerHandler::IntexDrainParked,
    )
    .expect("test period is nonzero");
    spec.requires_accounting_window = true;
    spec.coalesces_backlog = false;
    spec
}

fn ungated_spec(period_seconds: u64) -> TriggerSpec {
    let mut spec = TriggerSpec::new(
        4243,
        "test_ungated",
        period_seconds,
        0,
        TriggerHandler::IntexDrainParked,
    )
    .expect("test period is nonzero");
    spec.requires_accounting_window = false;
    spec.coalesces_backlog = false;
    spec
}

fn block_ctx(block_number: u64, timestamp: u64) -> BlockContext {
    BlockContext::new(block_number, timestamp, CHAIN_ID, Address::ZERO, Vec::new())
}

/// Stub `AccountingProgressView` for tests that don't need EVM storage.
/// The proptest and gate-arithmetic checks consume this directly.
struct StubProgress(u64);

impl AccountingProgressView for StubProgress {
    fn last_accounted_block_number(&self) -> Result<u64> {
        Ok(self.0)
    }
}

// ---------------------------------------------------------------------------
// Phase 1 accounting precedes Phase 2 CycleTick.
//
// Two complementary sets of tests cover the "Phase 1 commit happens before the
// Cycle dispatcher" invariant REALLY (no source-text scanning):
//
//   * Phase ORDER - `crates/blockchain/primitives/tests/system_tx_phase1_reorder.rs`
//     (`phase1_receipt_index_0_before_cycle_tick`,
//     `phase1_reordering_preserves_body_receipt_order`) asserts that Phase 1
//     owns body_index 0 and CycleTick owns body_index 1. It uses the real
//     `expected_begin_block_kinds` / `SystemTxPhase` routing API that drives the
//     executor.
//   * Gate DEPENDENCY - the behavioral tests below prove that the Cycle gate
//     reads `last_accounted_block_number` and runs only after the parent is
//     accounted. `*_boundary_cycle_runs_after_parent_accounted` covers the
//     committed case. `cycle_job_blocks_when_window_end_not_accounted` covers
//     the stale case.
//
// Together these make the ordering observable through real APIs/behavior. Thus
// the tests need no source-text scan of the executor.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Boundary tests across hour / day / week / month periods.
// Each builds a synthetic gated trigger, seeds `last_accounted == B - 1`,
// and asserts the gate does NOT block.
// ---------------------------------------------------------------------------

fn boundary_gate_check(period_seconds: u64, block_number: u64, block_ts: u64) {
    let spec = gated_spec(period_seconds);
    let block = block_ctx(block_number, block_ts);

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block.clone(), handle);
        // Fixture height is not an exact successor. Write the slot directly.
        ctx.storage
            .contract::<Accounting<'_>>()
            .last_accounted_block_number
            .write(block_number - 1)
            .unwrap();

        struct Reader<'a, 'b>(&'a BlockRuntimeContext<'b>);
        impl AccountingProgressView for Reader<'_, '_> {
            fn last_accounted_block_number(&self) -> Result<u64> {
                read_last_accounted_block_number(self.0)
            }
        }

        let blocked = accounting_gate_blocks(&spec, &Reader(&ctx), &block).unwrap();
        assert!(
            !blocked,
            "{}s-period gate must NOT block when parent is accounted",
            period_seconds
        );

        // The resolved window's `end_inclusive` must equal `block - 1`.
        let window = resolve_accounting_window(&spec, &block).expect("gated window resolves");
        assert_eq!(window.end_inclusive, block_number - 1);
    });
}

#[test]
fn hour_boundary_cycle_runs_after_parent_accounted() {
    // Block 100 at a timestamp inside the second hour-slot.
    boundary_gate_check(HOUR, 100, HOUR * 2 + 5);
}

#[test]
fn day_boundary_cycle_runs_after_parent_accounted() {
    boundary_gate_check(DAY, 100, DAY * 7 + 12);
}

#[test]
fn week_boundary_cycle_runs_after_parent_accounted() {
    boundary_gate_check(WEEK, 100, WEEK * 3 + 17);
}

#[test]
fn month_boundary_cycle_runs_after_parent_accounted() {
    boundary_gate_check(MONTH, 100, MONTH * 2 + 33);
}

// ---------------------------------------------------------------------------
// A gated trigger must NOT fire when `last_accounted <
// window.end_inclusive`.
// ---------------------------------------------------------------------------

#[test]
fn cycle_job_blocks_when_window_end_not_accounted() {
    let spec = gated_spec(DAY);
    let block = block_ctx(100, DAY * 7);
    // `last_accounted = 50`, but the gate requires `>= 99` (= 100 - 1).
    let progress = StubProgress(50);
    let blocked = accounting_gate_blocks(&spec, &progress, &block).unwrap();
    assert!(
        blocked,
        "gate must BLOCK when last_accounted < window.end_inclusive"
    );

    // Just-below-threshold edge: last_accounted = end_inclusive - 1.
    let progress_almost = StubProgress(98);
    let blocked_almost = accounting_gate_blocks(&spec, &progress_almost, &block).unwrap();
    assert!(
        blocked_almost,
        "gate must BLOCK at the just-below-threshold edge (98 < 99)"
    );

    // Exact-threshold: last_accounted = end_inclusive. Gate passes.
    let progress_exact = StubProgress(99);
    let blocked_exact = accounting_gate_blocks(&spec, &progress_exact, &block).unwrap();
    assert!(
        !blocked_exact,
        "gate must PASS when last_accounted == window.end_inclusive"
    );
}

// ---------------------------------------------------------------------------
// (Scope 4): ungated trigger fires regardless of accounting.
// ---------------------------------------------------------------------------

#[test]
fn cycle_job_without_accounting_window_runs_on_schedule() {
    let spec = ungated_spec(DAY);
    let block = block_ctx(100, DAY * 7);

    // Resolver returns `None` for ungated specs even on a deeply
    // accounted chain. The gate never consults the progress view.
    let window = resolve_accounting_window(&spec, &block);
    assert!(window.is_none(), "ungated spec must resolve to no window");

    // The gate must return `false` (i.e., "do not block") for ungated
    // triggers regardless of progress. This includes a deliberately-stale
    // `last_accounted = 0` reader that would block any gated trigger.
    let progress = StubProgress(0);
    let blocked = accounting_gate_blocks(&spec, &progress, &block).unwrap();
    assert!(
        !blocked,
        "ungated trigger must never be blocked by the accounting gate"
    );
}

// ---------------------------------------------------------------------------
// Bootstrap edge: blocks 0 and 1 (no V2 Phase 1) must resolve to `None`
// so the dispatcher fires without consulting accounting.
// ---------------------------------------------------------------------------

#[test]
fn genesis_bootstrap_resolves_to_no_window_for_gated_trigger() {
    let spec = gated_spec(DAY);
    for block_number in [0u64, 1] {
        let block = block_ctx(block_number, DAY);
        let window = resolve_accounting_window(&spec, &block);
        assert!(
            window.is_none(),
            "genesis bootstrap (block {block_number}) must not require accounting"
        );

        // Gate returns `false` even with `last_accounted = 0`.
        let blocked = accounting_gate_blocks(&spec, &StubProgress(0), &block).unwrap();
        assert!(
            !blocked,
            "genesis bootstrap gate must never block trigger (block {block_number})"
        );
    }
}

#[test]
fn accounting_progress_slot0_roundtrips_last_accounted_block_number() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle: StorageHandle| {
        let block = BlockContext::empty_for_tests(0, 0, CHAIN_ID);
        let ctx = BlockRuntimeContext::new(block, handle);

        let accounting: Accounting<'_> = ctx.storage.contract::<Accounting<'_>>();
        accounting
            .last_accounted_block_number
            .write(42)
            .expect("seed progress");
        let read = read_last_accounted_block_number(&ctx).expect("read progress");
        assert_eq!(read, 42);

        accounting
            .last_accounted_block_number
            .write(1_000_000)
            .expect("overwrite progress");
        assert_eq!(
            read_last_accounted_block_number(&ctx).expect("read overwritten progress"),
            1_000_000,
        );

        let raw = accounting
            .last_accounted_block_number
            .read()
            .expect("raw slot read");
        assert_eq!(raw, 1_000_000);

        let view = StubProgress(raw);
        assert_eq!(
            view.last_accounted_block_number().expect("view read"),
            1_000_000
        );
    });
}

fn with_progress_ctx(
    storage: &mut HashMapStorageProvider,
    body: impl FnOnce(&BlockRuntimeContext),
) {
    storage.enter(|handle: StorageHandle| {
        let block = BlockContext::empty_for_tests(2, 0, CHAIN_ID);
        let ctx = BlockRuntimeContext::new(block, handle);
        body(&ctx);
    });
}

fn revert_bytes(error: &PrecompileError) -> &[u8] {
    match error {
        PrecompileError::RevertBytes(bytes) => bytes,
        other => panic!("expected typed revert bytes, got {other:?}"),
    }
}

#[test]
fn phase1_recorder_rejects_zero_and_non_successors_and_replays_positive_progress() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);

    storage.enable_production_storage_gas_metering();
    with_progress_ctx(&mut storage, |ctx| {
        let error = record_phase1_progress(ctx, 0).expect_err("zero on a fresh slot");
        assert_eq!(
            &revert_bytes(&error)[..4],
            ICycle::ZeroAccountingProgress::SELECTOR
        );
        assert_eq!(read_last_accounted_block_number(ctx).unwrap(), 0);
    });
    assert_eq!(storage.metered_storage_operations().1, 0);
    assert!(storage.get_ordered_events().is_empty());

    storage.enable_production_storage_gas_metering();
    with_progress_ctx(&mut storage, |ctx| {
        let error = record_phase1_progress(ctx, 42).expect_err("gap 0 -> 42");
        let decoded = ICycle::AccountingProgressGap::abi_decode(revert_bytes(&error)).unwrap();
        assert_eq!(decoded.current, 0);
        assert_eq!(decoded.attempted, 42);
        assert_eq!(read_last_accounted_block_number(ctx).unwrap(), 0);
    });
    assert_eq!(storage.metered_storage_operations().1, 0);

    storage.enable_production_storage_gas_metering();
    with_progress_ctx(&mut storage, |ctx| {
        record_phase1_progress(ctx, 1).expect("first commit 0 -> 1");
        assert_eq!(read_last_accounted_block_number(ctx).unwrap(), 1);
    });
    assert_eq!(storage.metered_storage_operations().1, 1);

    storage.enable_production_storage_gas_metering();
    with_progress_ctx(&mut storage, |ctx| {
        record_phase1_progress(ctx, 1).expect("replay 1");
        assert_eq!(read_last_accounted_block_number(ctx).unwrap(), 1);
    });
    assert_eq!(storage.metered_storage_operations().1, 0);
    assert!(storage.get_ordered_events().is_empty());

    storage.enable_production_storage_gas_metering();
    with_progress_ctx(&mut storage, |ctx| {
        let error = record_phase1_progress(ctx, 3).expect_err("gap 1 -> 3");
        let decoded = ICycle::AccountingProgressGap::abi_decode(revert_bytes(&error)).unwrap();
        assert_eq!(decoded.current, 1);
        assert_eq!(decoded.attempted, 3);
        assert_eq!(read_last_accounted_block_number(ctx).unwrap(), 1);
    });
    assert_eq!(storage.metered_storage_operations().1, 0);

    storage.enable_production_storage_gas_metering();
    with_progress_ctx(&mut storage, |ctx| {
        record_phase1_progress(ctx, 2).expect("successor 1 -> 2");
        assert_eq!(read_last_accounted_block_number(ctx).unwrap(), 2);
    });
    assert_eq!(storage.metered_storage_operations().1, 1);

    storage.enable_production_storage_gas_metering();
    with_progress_ctx(&mut storage, |ctx| {
        let error = record_phase1_progress(ctx, 1).expect_err("regression 2 -> 1");
        let decoded = ICycle::BlockNumberRegression::abi_decode(revert_bytes(&error)).unwrap();
        assert_eq!(decoded.current, 2);
        assert_eq!(decoded.attempted, 1);
        assert_eq!(read_last_accounted_block_number(ctx).unwrap(), 2);
    });
    assert_eq!(storage.metered_storage_operations().1, 0);

    storage.enable_production_storage_gas_metering();
    with_progress_ctx(&mut storage, |ctx| {
        let error = record_phase1_progress(ctx, 0).expect_err("zero after progress");
        assert_eq!(
            &revert_bytes(&error)[..4],
            ICycle::ZeroAccountingProgress::SELECTOR
        );
        assert_eq!(read_last_accounted_block_number(ctx).unwrap(), 2);
    });
    assert_eq!(storage.metered_storage_operations().1, 0);

    with_progress_ctx(&mut storage, |ctx| {
        ctx.storage
            .contract::<Accounting<'_>>()
            .last_accounted_block_number
            .write(u64::MAX)
            .expect("seed max");
    });
    storage.enable_production_storage_gas_metering();
    with_progress_ctx(&mut storage, |ctx| {
        record_phase1_progress(ctx, u64::MAX).expect("replay max");
        assert_eq!(read_last_accounted_block_number(ctx).unwrap(), u64::MAX);
    });
    assert_eq!(storage.metered_storage_operations().1, 0);

    storage.enable_production_storage_gas_metering();
    with_progress_ctx(&mut storage, |ctx| {
        let error = record_phase1_progress(ctx, 1).expect_err("lower height at max");
        let decoded = ICycle::BlockNumberRegression::abi_decode(revert_bytes(&error)).unwrap();
        assert_eq!(decoded.current, u64::MAX);
        assert_eq!(decoded.attempted, 1);
        assert_eq!(read_last_accounted_block_number(ctx).unwrap(), u64::MAX);
    });
    assert_eq!(storage.metered_storage_operations().1, 0);
    assert!(storage.get_ordered_events().is_empty());
}

#[test]
fn genesis_progress_reads_zero_before_first_phase1_write() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle: StorageHandle| {
        let block = BlockContext::empty_for_tests(0, 0, CHAIN_ID);
        let ctx = BlockRuntimeContext::new(block, handle);

        let initial = read_last_accounted_block_number(&ctx).expect("read genesis slot 0");
        assert_eq!(
            initial, 0,
            "genesis slot 0 must read as zero before any Phase 1 commit",
        );

        for slot_index in 1u64..=15 {
            let slot = ctx
                .storage
                .sload(ACCOUNTING_PROGRESS_ADDRESS, U256::from(slot_index))
                .expect("read reserved slot");
            assert_eq!(
                slot,
                U256::ZERO,
                "INV2: reserved slot {slot_index} must be zero in genesis V2",
            );
        }
    });
}

#[test]
fn phase1_reorg_requires_branch_parent_state_for_fresh_progress() {
    const VALIDATOR: Address = address!("0x1111111111111111111111111111111111111111");
    const CHILD_BLOCK: u64 = 27;
    const PARENT_BLOCK: u64 = CHILD_BLOCK - 1;
    const REQUIRED_PREVIOUS: u64 = PARENT_BLOCK - 1;

    let provider = |block_number: u64| -> HashMapStorageProvider {
        let mut p = HashMapStorageProvider::new(2026);
        p.set_block_number(block_number);
        p.set_timestamp(U256::from(1_700_000_000u64));
        p.set_beneficiary(VALIDATOR);
        p
    };
    let read_progress = |p: &mut HashMapStorageProvider| -> u64 {
        p.enter(|storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::new(
                    storage.block_number().expect("block number"),
                    storage.now_secs().expect("timestamp"),
                    storage.chain_id().expect("chain id"),
                    VALIDATOR,
                    vec![VALIDATOR],
                ),
                storage,
            );
            read_last_accounted_block_number(&ctx).expect("read progress")
        })
    };
    let seed_progress = |p: &mut HashMapStorageProvider, block_number: u64| {
        p.enter(|storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::new(
                    storage.block_number().expect("block number"),
                    storage.now_secs().expect("timestamp"),
                    storage.chain_id().expect("chain id"),
                    VALIDATOR,
                    vec![VALIDATOR],
                ),
                storage,
            );
            ctx.storage
                .contract::<Accounting<'_>>()
                .last_accounted_block_number
                .write(block_number)
                .expect("seed progress");
        });
    };
    let record_progress = |p: &mut HashMapStorageProvider, block_number: u64| {
        p.enter(|storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::new(
                    storage.block_number().expect("block number"),
                    storage.now_secs().expect("timestamp"),
                    storage.chain_id().expect("chain id"),
                    VALIDATOR,
                    vec![VALIDATOR],
                ),
                storage,
            );
            record_phase1_progress(&ctx, block_number).expect("record progress");
        });
    };

    let mut parent = provider(CHILD_BLOCK);
    seed_progress(&mut parent, REQUIRED_PREVIOUS);
    let parent_state = parent.storage.clone();

    let mut branch_a = provider(CHILD_BLOCK);
    branch_a.storage = parent_state.clone();
    assert_eq!(read_progress(&mut branch_a), REQUIRED_PREVIOUS);
    record_progress(&mut branch_a, PARENT_BLOCK);
    assert_eq!(read_progress(&mut branch_a), PARENT_BLOCK);

    let mut branch_b_from_parent = provider(CHILD_BLOCK);
    branch_b_from_parent.storage = parent_state;
    assert_eq!(
        read_progress(&mut branch_b_from_parent),
        REQUIRED_PREVIOUS,
        "same-height reorg branch must begin from its own parent state"
    );
    record_progress(&mut branch_b_from_parent, PARENT_BLOCK);
    assert_eq!(read_progress(&mut branch_b_from_parent), PARENT_BLOCK);

    let mut branch_b_from_abandoned_post_state = provider(CHILD_BLOCK);
    branch_b_from_abandoned_post_state.storage = branch_a.storage.clone();
    assert_eq!(
        read_progress(&mut branch_b_from_abandoned_post_state),
        PARENT_BLOCK,
        "using abandoned sibling post-state would make the Fresh Phase 1 guard reject"
    );
}

// ---------------------------------------------------------------------------
// Determinism property: the same inputs give the same
// `AccountingWindow`. The resolver must be a pure function over
// `(period, offset, block_number, timestamp)` with no wall-clock or
// RNG dependency.
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn cycle_window_resolution_deterministic_from_headers(
        period in 60u64..=MONTH,
        offset_raw in 0u64..=DAY,
        block_number in 2u64..1_000_000,
        block_ts in 86_400u64..2_000_000_000,
    ) {
        let offset = offset_raw % period;
        let mut spec = TriggerSpec::new(
            99,
            "proptest",
            period,
            offset,
            TriggerHandler::IntexDrainParked,
        )
        .expect("period is at least 60");
        spec.requires_accounting_window = true;
        spec.coalesces_backlog = false;
        let block = block_ctx(block_number, block_ts);

        let a = resolve_accounting_window(&spec, &block);
        let b = resolve_accounting_window(&spec, &block);
        prop_assert_eq!(
            a, b,
            "resolver must be a pure function over (period, offset, block, ts)"
        );

        let window = a.expect("gated + block_number >= 2 always resolves Some");
        prop_assert_eq!(
            window.end_inclusive,
            block_number - 1,
            "end_inclusive must equal parent block number"
        );
        prop_assert_eq!(
            window,
            AccountingWindow {
                start_block: window.start_block,
                end_inclusive: block_number - 1,
            },
            "window equality round-trips for the same inputs"
        );
    }
}
