//! One `advance_auctions` run moves every scheduled day that has a due step (OIP-00377).
use super::*;
use crate::constants::{ADVANCE_DAY_BASE_GAS, MAX_TARGET_CHAINS};

/// At `NOW`, the commit window of this day ends.
const COMMITTING: EmissionDate = EmissionDate::new(20231112);
/// At `NOW`, the reveal stage of this day ends.
const REVEALING: EmissionDate = EmissionDate::new(20231111);
/// At `NOW`, this day waits for its bids, and its last clearing time is ahead. Nothing is due.
const WAITING: EmissionDate = EmissionDate::new(20231114);

/// Put a started day in the schedule at `stage`; its windows come from the day.
fn schedule_started(
    s: &StorageHandle,
    day: EmissionDate,
    stage: AuctionStage,
    promis_load_minor: u128,
) {
    crate::tests::record_started_day(s, day);
    let mut contract = s.contract::<DesisContract>();
    contract.write_stage(day, stage).unwrap();
    contract
        .config_promis_load_minor
        .write(&day, U256::from(promis_load_minor))
        .unwrap();
    contract
        .pending_desis_limit_minor
        .write(&day, U256::from(10 * LOAD_MINOR))
        .unwrap();
    contract.has_desis_limit.write(&day, 1).unwrap();
    contract.desis_limit_set.write(&day, 1).unwrap();
    contract.push_sched_active(day).unwrap();
}

fn schedule_committing(s: &StorageHandle) {
    schedule_started(
        s,
        COMMITTING,
        AuctionStage::CommittingBids,
        PROMIS_LOAD_MINOR,
    );
}

fn schedule_revealing(s: &StorageHandle, promis_load_minor: u128) {
    schedule_started(s, REVEALING, AuctionStage::RevealingBids, promis_load_minor);
}

fn schedule_waiting(s: &StorageHandle) {
    schedule_started(s, WAITING, AuctionStage::CollectingBids, PROMIS_LOAD_MINOR);
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
fn advance_auctions_moves_each_due_auction() {
    let mut provider = fixture_provider(&[SRC_CHAIN]);
    StorageHandle::enter(&mut provider, |s| {
        // The worst order of the OIP example: the waiting day first, the new day last.
        schedule_waiting(&s);
        schedule_revealing(&s, PROMIS_LOAD_MINOR);
        schedule_committing(&s);
        brief(&s, true);
        assert_eq!(stage(&s, EMISSION_DAY), AuctionStage::None);
    });
    metered(&mut provider);
    StorageHandle::enter(&mut provider, |s| {
        s.with_trigger_budget(ADVANCE_BUDGET, || {
            runtime::advance_intex_auctions_at(&s, NOW)
        })
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
    let starts: Vec<_> = outbox_of_type(
        &mut provider,
        outbe_intexfactory::transport::wire::MSG_INTEX_AUCTION_START,
    )
    .iter()
    .map(|message| message.label.emission_day)
    .collect();
    assert_eq!(starts, vec![EMISSION_DAY.value()]);
    let collecting: Vec<_> = outbox_of_type(
        &mut provider,
        outbe_intexfactory::transport::wire::MSG_INTEX_AUCTION_CLEARING_START,
    )
    .iter()
    .map(|message| (message.label.emission_day, message.chain_id))
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
        runtime::advance_intex_auctions_at(&s, NOW).unwrap();
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
fn a_day_above_the_target_cap_reverts_and_the_other_days_move() {
    let chains: Vec<u32> = (1..=MAX_TARGET_CHAINS as u32 + 1).collect();
    with_targets(&chains, |s| {
        schedule_revealing(&s, PROMIS_LOAD_MINOR);
        schedule_committing(&s);
        brief(&s, true);
        runtime::advance_intex_auctions_at(&s, NOW).unwrap();
        assert_eq!(stage(&s, REVEALING), AuctionStage::RevealingBids);
        assert!(is_scheduled(&s, REVEALING));
        assert_eq!(stage(&s, COMMITTING), AuctionStage::RevealingBids);
        assert_eq!(stage(&s, EMISSION_DAY), AuctionStage::CommittingBids);
    });
}

#[test]
fn the_own_chain_asks_for_the_same_round_gas_as_another_chain() {
    const OTHER_CHAIN: u32 = 2;
    let own_chain = u32::try_from(CHAIN_ID).unwrap();
    let mut provider = fixture_provider(&[own_chain, OTHER_CHAIN]);
    StorageHandle::enter(&mut provider, |s| {
        schedule_waiting(&s);
        let contract = s.contract::<DesisContract>();
        for chain in [own_chain, OTHER_CHAIN] {
            let key = DesisContract::chain_key(WAITING, chain);
            contract.chain_bid_count.write(&key, 640).unwrap();
        }
        schedule_revealing(&s, PROMIS_LOAD_MINOR);
    });
    StorageHandle::enter(&mut provider, |s| {
        runtime::advance_intex_auctions_at(&s, NOW).unwrap()
    });
    let asks: Vec<_> = outbox_of_type(
        &mut provider,
        outbe_intexfactory::transport::wire::MSG_INTEX_AUCTION_CLEARING_START,
    )
    .iter()
    .map(|message| (message.chain_id, message.message_gas))
    .collect();
    assert_eq!(asks.len(), 2);
    assert_eq!(asks[0].0, own_chain);
    assert_eq!(asks[1].0, OTHER_CHAIN);
    assert_eq!(asks[0].1, asks[1].1);
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
    // The first day is due a commit end: one base step for the day and one for the flip.
    let commit_end = 2 * ADVANCE_DAY_BASE_GAS;
    StorageHandle::enter(&mut provider, |s| {
        // Below the bound of the first day: the run moves no day and spends no write.
        let before = s.gas_used().unwrap();
        s.with_trigger_budget(commit_end - 1, || {
            runtime::advance_intex_auctions_at(&s, NOW)
        })
        .unwrap();
        assert!(s.gas_used().unwrap() - before < commit_end);
        assert_eq!(stage(&s, COMMITTING), AuctionStage::CommittingBids);
        assert_eq!(
            s.contract::<DesisContract>()
                .sched_poll_cursor
                .read()
                .unwrap(),
            0
        );

        // Pays for the commit end, not for the reveal end of the second day.
        let small = commit_end + 1_000;
        let before = s.gas_used().unwrap();
        s.with_trigger_budget(small, || runtime::advance_intex_auctions_at(&s, NOW))
            .unwrap();
        assert!(s.gas_used().unwrap() - before <= small);
        assert_eq!(stage(&s, COMMITTING), AuctionStage::RevealingBids);
        assert_eq!(stage(&s, REVEALING), AuctionStage::RevealingBids);
        assert_eq!(stage(&s, EMISSION_DAY), AuctionStage::None);
        // The next run starts with the first day that this run did not reach.
        assert_eq!(
            s.contract::<DesisContract>()
                .sched_poll_cursor
                .read()
                .unwrap(),
            1
        );

        s.with_trigger_budget(ADVANCE_BUDGET, || {
            runtime::advance_intex_auctions_at(&s, NOW + 3_600)
        })
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

fn promis_limit(s: &StorageHandle) -> U256 {
    outbe_promislimit::PromisLimitContract::new(s.clone())
        .promis_limit()
        .unwrap()
}

#[test]
fn a_brief_cancel_of_a_started_day_is_budgeted_as_a_cancel() {
    let mut provider = fixture_provider(&[SRC_CHAIN]);
    StorageHandle::enter(&mut provider, |s| {
        schedule_committing(&s);
        s.contract::<DesisContract>()
            .has_desis_limit
            .write(&COMMITTING, 0)
            .unwrap();
    });
    metered(&mut provider);
    let commit_end = 2 * ADVANCE_DAY_BASE_GAS;
    StorageHandle::enter(&mut provider, |s| {
        // Enough for a commit end, not for the cancel the brief asks for.
        let small = commit_end + 1_000;
        let before = s.gas_used().unwrap();
        s.with_trigger_budget(small, || runtime::advance_intex_auctions_at(&s, NOW))
            .unwrap();
        assert!(s.gas_used().unwrap() - before <= small);
        assert_eq!(stage(&s, COMMITTING), AuctionStage::CommittingBids);

        s.with_trigger_budget(ADVANCE_BUDGET, || {
            runtime::advance_intex_auctions_at(&s, NOW)
        })
        .unwrap();
        assert_eq!(stage(&s, COMMITTING), AuctionStage::Canceled);
    });
}

#[test]
fn a_canceled_started_day_queues_its_bid_intake_cleanup() {
    with_storage(|s| {
        schedule_revealing(&s, PROMIS_LOAD_MINOR);
        s.contract::<DesisContract>()
            .cancel_reason
            .write(&REVEALING, crate::constants::CANCELED_NO_LIMIT)
            .unwrap();
        runtime::advance_intex_auctions_at(&s, NOW).unwrap();
        assert_eq!(stage(&s, REVEALING), AuctionStage::Canceled);
        assert_eq!(
            s.contract::<DesisContract>()
                .cleanup_pending
                .read(&REVEALING)
                .unwrap(),
            1
        );
    });
}

#[test]
fn a_brief_above_the_auction_domain_is_returned_once_and_cancels_the_day() {
    let mut provider = fixture_provider(&[SRC_CHAIN]);
    StorageHandle::enter(&mut provider, |s| {
        schedule_committing(&s);
        let contract = s.contract::<DesisContract>();
        contract.desis_limit_set.write(&COMMITTING, 0).unwrap();
        contract.has_desis_limit.write(&COMMITTING, 0).unwrap();
        contract
            .pending_desis_limit_minor
            .write(&COMMITTING, U256::ZERO)
            .unwrap();
        let limit_before = promis_limit(&s);
        let receipt = crate::api::start_desis(
            s.clone(),
            crate::api::DesisLimit {
                emission_day: COMMITTING,
                desis_limit_minor: U256::from(u128::MAX) + U256::ONE,
                has_limit: true,
            },
        )
        .unwrap();
        assert!(matches!(
            receipt,
            DesisLimitReceipt::ReturnedToPromisLimit { .. }
        ));
        runtime::advance_intex_auctions_at(&s, NOW).unwrap();
        assert_eq!(stage(&s, COMMITTING), AuctionStage::Canceled);
        assert_eq!(promis_limit(&s), limit_before);
    });
}
