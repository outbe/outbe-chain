//! One `auction_advance` run moves every scheduled day that has a due step (OIP-00377).
use super::*;
use crate::constants::{ADVANCE_DAY_RESERVE_GAS, DAY_STATE_LIVE};
use crate::sol_ext::IOriginRouter::{
    sendAuctionStageCollectingBidsCall, sendAuctionStageStartCall,
};

/// At `NOW`, the commit window of this day ends.
const COMMITTING: EmissionDate = EmissionDate::new(20231112);
/// At `NOW`, the reveal stage of this day ends.
const REVEALING: EmissionDate = EmissionDate::new(20231111);
/// At `NOW`, this day waits for its bids. Nothing is due.
const WAITING: EmissionDate = EmissionDate::new(20231110);

/// Put a started day in the schedule at `stage`, with its windows from `anchor`.
fn schedule_started(
    s: &StorageHandle,
    day: EmissionDate,
    stage: AuctionStage,
    anchor: u64,
    promis_load_minor: u128,
) {
    let mut contract = s.contract::<DesisContract>();
    contract.write_stage(day, stage).unwrap();
    contract
        .auction_at
        .write(&day, u32::try_from(anchor).unwrap())
        .unwrap();
    contract
        .config_promis_load_minor
        .write(&day, U256::from(promis_load_minor))
        .unwrap();
    contract
        .pending_desis_limit_minor
        .write(&day, U256::from(10 * LOAD_MINOR))
        .unwrap();
    contract.push_sched_active(day).unwrap();
}

fn schedule_committing(s: &StorageHandle) {
    schedule_started(
        s,
        COMMITTING,
        AuctionStage::CommittingBids,
        ANCHOR - SECONDS_PER_DAY,
        PROMIS_LOAD_MINOR,
    );
}

fn schedule_revealing(s: &StorageHandle, promis_load_minor: u128) {
    schedule_started(
        s,
        REVEALING,
        AuctionStage::RevealingBids,
        ANCHOR - 2 * SECONDS_PER_DAY,
        promis_load_minor,
    );
}

fn schedule_waiting(s: &StorageHandle) {
    schedule_started(
        s,
        WAITING,
        AuctionStage::CollectingBids,
        ANCHOR - 2 * SECONDS_PER_DAY,
        PROMIS_LOAD_MINOR,
    );
}

/// Meter storage as in production, so that a trigger budget applies.
fn metered(provider: &mut HashMapStorageProvider) {
    provider.enable_production_storage_gas_metering();
    provider.set_gas_limit(32_000_000);
}

fn stage(s: &StorageHandle, day: EmissionDate) -> AuctionStage {
    s.contract::<DesisContract>().read_stage(day).unwrap()
}

fn is_scheduled(s: &StorageHandle, day: EmissionDate) -> bool {
    s.contract::<DesisContract>()
        .sched_active_slot
        .read(&day)
        .unwrap()
        != 0
}

#[test]
fn gap_ed55_advance_auctions_moves_each_due_auction() {
    let mut provider = fixture_provider(&[SRC_CHAIN]);
    StorageHandle::enter(&mut provider, |s| {
        // The worst order of the OIP example: the waiting day first, the new day last.
        schedule_waiting(&s);
        schedule_revealing(&s, PROMIS_LOAD_MINOR);
        schedule_committing(&s);
        brief(&s, true);
        assert_eq!(stage(&s, EMISSION_DAY), AuctionStage::Briefed);
    });
    metered(&mut provider);
    let calls_before = provider.sub_call_inputs().len();
    StorageHandle::enter(&mut provider, |s| {
        s.with_trigger_budget(ADVANCE_BUDGET, || runtime::schedule_tick(&s, NOW))
            .unwrap();
        assert_eq!(stage(&s, EMISSION_DAY), AuctionStage::CommittingBids);
        assert_eq!(stage(&s, COMMITTING), AuctionStage::RevealingBids);
        assert_eq!(stage(&s, REVEALING), AuctionStage::CollectingBids);
        assert_eq!(stage(&s, WAITING), AuctionStage::CollectingBids);
        let contract = s.contract::<DesisContract>();
        assert_ne!(contract.gate_active_slot.read(&REVEALING).unwrap(), 0);
        assert_eq!(contract.gate_active_slot.read(&WAITING).unwrap(), 0);
        assert_eq!(contract.sched_active_count.read().unwrap(), 4);
        assert_eq!(contract.sched_poll_cursor.read().unwrap(), 0);
    });
    let sent = &provider.sub_call_inputs()[calls_before..];
    let starts: Vec<_> = sent
        .iter()
        .filter(|input| input.target == ORIGIN_ROUTER_ADDRESS)
        .filter_map(|input| sendAuctionStageStartCall::abi_decode(&input.calldata).ok())
        .map(|call| (call.params.emissionDay, call.params.dayState))
        .collect();
    assert_eq!(starts, vec![(EMISSION_DAY.value(), DAY_STATE_LIVE)]);
    let collecting: Vec<_> = sent
        .iter()
        .filter(|input| input.target == ORIGIN_ROUTER_ADDRESS)
        .filter_map(|input| sendAuctionStageCollectingBidsCall::abi_decode(&input.calldata).ok())
        .map(|call| (call.emissionDay, call.dstChainId))
        .collect();
    assert_eq!(collecting, vec![(REVEALING.value(), SRC_CHAIN)]);
}

#[test]
fn a_reverting_day_does_not_stop_the_other_days() {
    with_storage(|s| {
        // With no PROMIS load, the clearing of this day cannot arm, and the day reverts.
        schedule_revealing(&s, 0);
        schedule_committing(&s);
        brief(&s, true);
        runtime::schedule_tick(&s, NOW).unwrap();
        assert_eq!(stage(&s, REVEALING), AuctionStage::RevealingBids);
        assert!(is_scheduled(&s, REVEALING));
        assert_eq!(
            s.contract::<DesisContract>()
                .gate_active_slot
                .read(&REVEALING)
                .unwrap(),
            0
        );
        assert_eq!(stage(&s, COMMITTING), AuctionStage::RevealingBids);
        assert_eq!(stage(&s, EMISSION_DAY), AuctionStage::CommittingBids);
    });
}

#[test]
fn a_small_budget_stops_before_overrun_and_the_next_run_moves_the_rest() {
    let mut provider = fixture_provider(&[SRC_CHAIN]);
    StorageHandle::enter(&mut provider, |s| {
        schedule_committing(&s);
        schedule_revealing(&s, PROMIS_LOAD_MINOR);
        brief(&s, true);
    });
    metered(&mut provider);
    StorageHandle::enter(&mut provider, |s| {
        // Below the reserve from the start: the run moves no day and spends no write.
        let before = s.gas_used().unwrap();
        s.with_trigger_budget(ADVANCE_DAY_RESERVE_GAS - 1, || {
            runtime::schedule_tick(&s, NOW)
        })
        .unwrap();
        assert!(s.gas_used().unwrap() - before < ADVANCE_DAY_RESERVE_GAS);
        assert_eq!(stage(&s, COMMITTING), AuctionStage::CommittingBids);
        assert_eq!(
            s.contract::<DesisContract>()
                .sched_poll_cursor
                .read()
                .unwrap(),
            0
        );

        // Above the reserve by the two reads before the first day and less than the write of
        // that day. So the check passes before the first day and fails before the second.
        let small = ADVANCE_DAY_RESERVE_GAS + 1_000;
        let before = s.gas_used().unwrap();
        s.with_trigger_budget(small, || runtime::schedule_tick(&s, NOW))
            .unwrap();
        assert!(s.gas_used().unwrap() - before <= small);
        assert_eq!(stage(&s, COMMITTING), AuctionStage::RevealingBids);
        assert_eq!(stage(&s, REVEALING), AuctionStage::RevealingBids);
        assert_eq!(stage(&s, EMISSION_DAY), AuctionStage::Briefed);
        // The next run starts with the first day that this run did not reach.
        assert_eq!(
            s.contract::<DesisContract>()
                .sched_poll_cursor
                .read()
                .unwrap(),
            1
        );

        s.with_trigger_budget(ADVANCE_BUDGET, || runtime::schedule_tick(&s, NOW + 3_600))
            .unwrap();
        assert_eq!(stage(&s, REVEALING), AuctionStage::CollectingBids);
        assert_eq!(stage(&s, EMISSION_DAY), AuctionStage::CommittingBids);
        assert_eq!(stage(&s, COMMITTING), AuctionStage::RevealingBids);
        assert_eq!(
            s.contract::<DesisContract>()
                .sched_poll_cursor
                .read()
                .unwrap(),
            0
        );
    });
}

#[test]
fn one_run_retires_every_backlogged_day_from_the_cursor_on() {
    with_storage(|s| {
        // The cursor points at the second day. Each removal moves the tail day into its slot.
        let late = EmissionDate::new(20231109);
        let later = EmissionDate::new(20231108);
        for (day, anchor) in [
            (late, ANCHOR - 3 * SECONDS_PER_DAY),
            (WAITING, ANCHOR - 2 * SECONDS_PER_DAY),
            (later, ANCHOR - 3 * SECONDS_PER_DAY),
        ] {
            schedule_started(
                &s,
                day,
                AuctionStage::CollectingBids,
                anchor,
                PROMIS_LOAD_MINOR,
            );
        }
        let contract = s.contract::<DesisContract>();
        contract.sched_poll_cursor.write(1).unwrap();
        runtime::schedule_tick(&s, NOW).unwrap();
        assert_eq!(stage(&s, late), AuctionStage::Failed);
        assert_eq!(stage(&s, later), AuctionStage::Failed);
        assert_eq!(stage(&s, WAITING), AuctionStage::CollectingBids);
        assert_eq!(contract.sched_active_count.read().unwrap(), 1);
        assert!(is_scheduled(&s, WAITING));
        assert_eq!(contract.sched_poll_cursor.read().unwrap(), 0);
    });
}
