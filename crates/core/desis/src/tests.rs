#![cfg(test)]

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api::{AuctionBriefReceipt, AuctionBriefRejectionReason};
use crate::constants::ORIGIN_ROUTER_ADDRESS;
use crate::runtime;
use crate::schema::{AuctionConfig, AuctionStage, BidData, DesisContract};
use outbe_intexfactory::constants::QUALIFIER_REFERENCE_ISO;

const CHAIN_ID: u64 = 1;
const WORLDWIDE_DAY: WorldwideDay = WorldwideDay::new(20260101);
const NEXT_WORLDWIDE_DAY: WorldwideDay = WorldwideDay::new(20260102);
const PROMIS_LOAD_MINOR: u128 = 1_000_000_000_000_000_000; // 1e18
/// The single default target chain the auction fans in from (matches `src_chain_id` in the calls).
const SRC_CHAIN: u32 = 1;
/// Block timestamp the tests brief at: just after a midnight, so the brief
/// anchors to that same midnight (the normal on-time case).
const NOW: u64 = 1_699_920_000 + 5;
const ANCHOR: u64 = NOW - NOW % 86_400;
const LOAD_MINOR: u128 = crate::constants::PROMIS_LOAD * PROMIS_LOAD_MINOR;
const ENTRY_PRICE: u128 = 2_000_000_000_000_000; // 2e15 (entry feeds floor/call; escrow basis = promis_load)

fn bidder(n: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[19] = n;
    Address::from(bytes)
}

/// ABI-encoded `targetsOf` return, so the OriginRouter staticcall in clearing sees this target set.
fn targets_stub(chains: &[u32]) -> Bytes {
    Bytes::from(crate::sol_ext::IOriginRouter::targetsOfCall::abi_encode_returns(&chains.to_vec()))
}

fn with_targets<R>(chains: &[u32], f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(NOW));
    // Stub OriginRouter: `targetsOf` returns the snapshot; send* returns are ignored by the runtime.
    storage.stub_sub_call_at(ORIGIN_ROUTER_ADDRESS, targets_stub(chains));
    // Stub IntexNFT1155: createSeries/settle/burnSettled are void; balanceOf returns 0 (32 bytes).
    storage.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, f)
}

fn with_storage<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    with_targets(&[SRC_CHAIN], f)
}

fn brief_at(s: &StorageHandle, worldwide_day: WorldwideDay, supply_promis: u128, green: bool) {
    assert_eq!(
        crate::api::dispatch_auction_brief(
            s.clone(),
            worldwide_day,
            U256::from(supply_promis),
            entry_price_rows(),
            green,
            NOW,
        )
        .unwrap(),
        AuctionBriefReceipt::Accepted
    );
}

fn brief(s: &StorageHandle, green: bool) {
    brief_at(s, WORLDWIDE_DAY, 10 * LOAD_MINOR, green);
}

/// Brief and drive the schedule to `Revealing` (bid intake open, gate not armed).
fn open_revealing(s: &StorageHandle) {
    brief(s, true);
    runtime::schedule_tick(s, NOW).unwrap();
    runtime::schedule_tick(s, ANCHOR + 86_400).unwrap();
}

/// The reveal-end tick: arms the clearing gate from the brief supply.
fn arm_clearing(s: &StorageHandle) {
    runtime::schedule_tick(s, ANCHOR + 2 * 86_400).unwrap();
}

/// Brief `units` whole Intex units and drive the schedule until the gate is armed.
fn open_clearing(s: &StorageHandle, units: u128) {
    brief_at(s, WORLDWIDE_DAY, units * LOAD_MINOR, true);
    runtime::schedule_tick(s, NOW).unwrap();
    runtime::schedule_tick(s, ANCHOR + 86_400).unwrap();
    arm_clearing(s);
}

/// Send the chain's BIDS_DONE marker so its intake finalizes and the clearing gate opens.
fn mark_done(s: &StorageHandle, chain: u32, gen: u32, total_batches: u16, total_bids: u32) {
    runtime::process_bids_done(
        s.clone(),
        ORIGIN_ROUTER_ADDRESS,
        WORLDWIDE_DAY,
        chain,
        gen,
        total_batches,
        total_bids,
    )
    .unwrap();
}

/// Run the begin-block gate clearing for the day (every snapshot chain finalized).
fn clear(s: &StorageHandle) -> crate::schema::ClearingResult {
    runtime::force_clear(s.clone(), WORLDWIDE_DAY, NOW)
        .unwrap()
        .unwrap()
}

fn bids(n: u8, rate: u32) -> Vec<BidData> {
    (0..n)
        .map(|i| BidData {
            bidder_address: bidder(i),
            intex_bid_rate: rate,
            timestamp: i as u32,
            intex_quantity: 1,
            issuance_currency: QUALIFIER_REFERENCE_ISO,
            reference_currency: QUALIFIER_REFERENCE_ISO,
        })
        .collect()
}

// --- Auction brief ---

/// The frozen price table an OCOMP request brings: the same single row the
/// in-process fixtures use, in the wire shape the receipt commits.
fn frozen_entry_prices() -> Vec<outbe_ocomp_protocol::intent::ReferenceEntryPriceV1> {
    vec![outbe_ocomp_protocol::intent::ReferenceEntryPriceV1 {
        reference_currency: QUALIFIER_REFERENCE_ISO,
        entry_price_minor: U256::from(ENTRY_PRICE),
        source: outbe_ocomp_protocol::intent::AuctionEntryPriceSource::LastClosedDayVwap,
        source_day: WORLDWIDE_DAY.value(),
    }]
}

/// The single priced reference currency the fixtures brief with.
fn entry_price_rows() -> Vec<crate::schema::ReferencePrice> {
    vec![crate::schema::ReferencePrice {
        iso_code: QUALIFIER_REFERENCE_ISO,
        entry_price_minor: U256::from(ENTRY_PRICE),
    }]
}

#[test]
fn dispatch_auction_brief_records_the_brief() {
    with_storage(|s| {
        let receipt = crate::api::dispatch_auction_brief(
            s.clone(),
            WORLDWIDE_DAY,
            U256::from(10 * PROMIS_LOAD_MINOR),
            entry_price_rows(),
            true,
            NOW,
        )
        .unwrap();
        assert_eq!(receipt, AuctionBriefReceipt::Accepted);
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Briefed
        );
        assert_eq!(
            contract.pending_supply_promis.read(&WORLDWIDE_DAY).unwrap(),
            U256::from(10 * PROMIS_LOAD_MINOR)
        );
        assert_eq!(contract.brief_green.read(&WORLDWIDE_DAY).unwrap(), 1);
        assert_eq!(
            u64::from(contract.auction_at.read(&WORLDWIDE_DAY).unwrap()),
            NOW - NOW % 86_400
        );
        assert_eq!(contract.sched_active_count.read().unwrap(), 1);
        assert_eq!(
            contract.sched_active_at.read(&0).unwrap(),
            WORLDWIDE_DAY.value()
        );
        let cfg = contract.read_auction_config(WORLDWIDE_DAY).unwrap();
        assert_eq!(cfg.entry_price_minor, U256::from(ENTRY_PRICE));
    });
}

#[test]
fn dispatch_auction_brief_records_a_red_day() {
    with_storage(|s| {
        let receipt = crate::api::dispatch_auction_brief(
            s.clone(),
            WORLDWIDE_DAY,
            U256::from(PROMIS_LOAD_MINOR),
            entry_price_rows(),
            false,
            NOW,
        )
        .unwrap();
        assert_eq!(receipt, AuctionBriefReceipt::Accepted);
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Briefed
        );
        assert_eq!(contract.brief_green.read(&WORLDWIDE_DAY).unwrap(), 0);
    });
}

#[test]
fn strict_request_auction_base_commits_the_exact_green_brief() {
    with_storage(|s| {
        let digest = crate::ocomp_budget::apply_request_auction_base(
            s.clone(),
            B256::repeat_byte(0x41),
            WORLDWIDE_DAY,
            U256::from(7 * PROMIS_LOAD_MINOR),
            &frozen_entry_prices(),
            NOW,
        )
        .expect("strict request brief");

        assert_ne!(digest, B256::ZERO);
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Briefed
        );
        assert_eq!(
            contract.pending_supply_promis.read(&WORLDWIDE_DAY).unwrap(),
            U256::from(7 * PROMIS_LOAD_MINOR)
        );
        assert_eq!(contract.brief_green.read(&WORLDWIDE_DAY).unwrap(), 1);
        assert_eq!(
            contract
                .read_auction_config(WORLDWIDE_DAY)
                .unwrap()
                .entry_price_minor,
            U256::from(ENTRY_PRICE)
        );
    });
}

#[test]
fn strict_request_auction_base_propagates_duplicate_refusal_without_overwrite() {
    with_storage(|s| {
        crate::ocomp_budget::apply_request_auction_base(
            s.clone(),
            B256::repeat_byte(0x41),
            WORLDWIDE_DAY,
            U256::from(7 * PROMIS_LOAD_MINOR),
            &frozen_entry_prices(),
            NOW,
        )
        .unwrap();

        assert!(crate::ocomp_budget::apply_request_auction_base(
            s.clone(),
            B256::repeat_byte(0x41),
            WORLDWIDE_DAY,
            U256::from(9 * PROMIS_LOAD_MINOR),
            &frozen_entry_prices(),
            NOW,
        )
        .is_err());

        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.pending_supply_promis.read(&WORLDWIDE_DAY).unwrap(),
            U256::from(7 * PROMIS_LOAD_MINOR)
        );
        assert_eq!(contract.sched_active_count.read().unwrap(), 1);
    });
}

#[test]
fn strict_request_auction_base_rejects_oversized_supply_without_state() {
    with_storage(|s| {
        assert!(crate::ocomp_budget::apply_request_auction_base(
            s.clone(),
            B256::repeat_byte(0x41),
            WORLDWIDE_DAY,
            U256::MAX,
            &frozen_entry_prices(),
            NOW,
        )
        .is_err());

        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::None
        );
        assert_eq!(contract.sched_active_count.read().unwrap(), 0);
        assert_no_request_brief_state(&s);
    });
}

fn assert_no_request_brief_state(storage: &StorageHandle<'_>) {
    let contract = storage.contract::<DesisContract>();
    assert_eq!(
        contract.read_stage(WORLDWIDE_DAY).unwrap(),
        AuctionStage::None
    );
    assert_eq!(
        contract.read_auction_config(WORLDWIDE_DAY).unwrap(),
        AuctionConfig {
            issuance_currency: 0,
            reference_currency: 0,
            promis_load_minor: 0,
            call_trigger: Default::default(),
            min_intex_bid_rate: 0,
            min_intex_bid_quantity: 0,
            commit_bond_minor: 0,
            entry_price_minor: U256::ZERO,
            reference_prices: vec![],
        }
    );
    assert_eq!(
        contract.pending_supply_promis.read(&WORLDWIDE_DAY).unwrap(),
        U256::ZERO
    );
    assert_eq!(contract.brief_green.read(&WORLDWIDE_DAY).unwrap(), 0);
    assert_eq!(contract.auction_at.read(&WORLDWIDE_DAY).unwrap(), 0);
    assert_eq!(contract.sched_active_count.read().unwrap(), 0);
    assert_eq!(contract.sched_active_at.read(&0).unwrap(), 0u32);
    assert_eq!(contract.sched_active_slot.read(&WORLDWIDE_DAY).unwrap(), 0);
}

#[test]
fn strict_request_auction_base_rolls_back_every_partial_write_boundary() {
    let mutation_count = {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let result = StorageHandle::enter(&mut provider, |storage| {
            crate::ocomp_budget::apply_request_auction_base(
                storage,
                B256::repeat_byte(0x41),
                WORLDWIDE_DAY,
                U256::from(7 * PROMIS_LOAD_MINOR),
                &frozen_entry_prices(),
                NOW,
            )
        });
        assert!(result.is_ok());
        provider.clear_mutation_failure()
    };
    assert!(
        mutation_count > 1,
        "fixture must cross partial-write boundaries"
    );

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.fail_after_mutation_at(operation);
        let result = StorageHandle::enter(&mut provider, |storage| {
            crate::ocomp_budget::apply_request_auction_base(
                storage,
                B256::repeat_byte(0x41),
                WORLDWIDE_DAY,
                U256::from(7 * PROMIS_LOAD_MINOR),
                &frozen_entry_prices(),
                NOW,
            )
        });
        assert!(
            result.is_err(),
            "fault after mutation {operation} must propagate"
        );
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        StorageHandle::enter(&mut provider, |storage| {
            assert_no_request_brief_state(&storage)
        });
        assert!(provider.get_ordered_events().is_empty());
    }
}

#[test]
fn strict_request_auction_base_never_tops_up_a_live_auction() {
    with_storage(|storage| {
        crate::ocomp_budget::apply_request_auction_base(
            storage.clone(),
            B256::repeat_byte(0x41),
            WORLDWIDE_DAY,
            U256::from(7 * PROMIS_LOAD_MINOR),
            &frozen_entry_prices(),
            NOW,
        )
        .unwrap();
        runtime::schedule_tick(&storage, NOW).unwrap();

        let before = storage.contract::<DesisContract>();
        assert_eq!(
            before.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Started
        );
        let config = before.read_auction_config(WORLDWIDE_DAY).unwrap();
        let anchor = before.auction_at.read(&WORLDWIDE_DAY).unwrap();

        assert!(crate::ocomp_budget::apply_request_auction_base(
            storage.clone(),
            B256::repeat_byte(0x41),
            WORLDWIDE_DAY,
            U256::from(9 * PROMIS_LOAD_MINOR),
            &frozen_entry_prices(),
            NOW,
        )
        .is_err());

        let after = storage.contract::<DesisContract>();
        assert_eq!(
            after.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Started
        );
        assert_eq!(
            after.pending_supply_promis.read(&WORLDWIDE_DAY).unwrap(),
            U256::from(7 * PROMIS_LOAD_MINOR)
        );
        assert_eq!(after.read_auction_config(WORLDWIDE_DAY).unwrap(), config);
        assert_eq!(after.auction_at.read(&WORLDWIDE_DAY).unwrap(), anchor);
        assert_eq!(after.sched_active_count.read().unwrap(), 1);
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn dispatch_auction_brief_duplicate_propagates_without_committed_failure_event() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(NOW));
    storage.stub_sub_call_at(ORIGIN_ROUTER_ADDRESS, targets_stub(&[SRC_CHAIN]));
    storage.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, |s| {
        assert_eq!(
            crate::api::dispatch_auction_brief(
                s.clone(),
                WORLDWIDE_DAY,
                U256::from(10 * PROMIS_LOAD_MINOR),
                entry_price_rows(),
                true,
                NOW,
            )
            .unwrap(),
            AuctionBriefReceipt::Accepted
        );
        assert!(crate::api::dispatch_auction_brief(
            s.clone(),
            WORLDWIDE_DAY,
            U256::from(7 * PROMIS_LOAD_MINOR),
            entry_price_rows(),
            true,
            NOW,
        )
        .is_err());
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.pending_supply_promis.read(&WORLDWIDE_DAY).unwrap(),
            U256::from(10 * PROMIS_LOAD_MINOR),
            "the first brief stays intact"
        );
        assert_eq!(contract.sched_active_count.read().unwrap(), 1);
    });

    assert!(storage
        .get_events(outbe_primitives::addresses::DESIS_ADDRESS)
        .is_empty());
}

#[test]
fn dispatch_auction_brief_oversized_supply_returns_typed_full_carry_over() {
    use crate::precompile::IDesis;
    use alloy_sol_types::SolEvent;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |s| {
        assert_eq!(
            crate::api::dispatch_auction_brief(
                s.clone(),
                WORLDWIDE_DAY,
                U256::MAX,
                entry_price_rows(),
                true,
                NOW,
            )
            .unwrap(),
            AuctionBriefReceipt::RejectedToCarryOver {
                reason: AuctionBriefRejectionReason::SupplyExceedsAuctionDomain,
                supply: U256::MAX,
                max_accepted: U256::from(u128::MAX),
            }
        );
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::None
        );
        assert_eq!(contract.sched_active_count.read().unwrap(), 0);
        assert_no_request_brief_state(&s);
    });

    let logs = storage.get_events(outbe_primitives::addresses::DESIS_ADDRESS);
    assert_eq!(logs.len(), 1);
    let event = IDesis::AuctionBriefRejectedToCarryOver::decode_log_data(&logs[0]).unwrap();
    assert_eq!(event.worldwideDay, WORLDWIDE_DAY.value());
    assert_eq!(event.supply, U256::MAX);
    assert_eq!(event.maxAccepted, U256::from(u128::MAX));
    assert_eq!(
        event.reasonCode,
        AuctionBriefRejectionReason::SupplyExceedsAuctionDomain.code()
    );
}

#[test]
fn auction_domain_boundary_accepts_u128_max_and_rejects_the_next_value() {
    with_storage(|storage| {
        assert_eq!(
            crate::api::dispatch_auction_brief(
                storage.clone(),
                WORLDWIDE_DAY,
                U256::from(u128::MAX),
                entry_price_rows(),
                true,
                NOW,
            )
            .unwrap(),
            AuctionBriefReceipt::Accepted
        );
    });

    let supply = U256::from(u128::MAX).checked_add(U256::from(1_u8)).unwrap();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            crate::api::dispatch_auction_brief(
                storage.clone(),
                WORLDWIDE_DAY,
                supply,
                entry_price_rows(),
                true,
                NOW,
            )
            .unwrap(),
            AuctionBriefReceipt::RejectedToCarryOver {
                reason: AuctionBriefRejectionReason::SupplyExceedsAuctionDomain,
                supply,
                max_accepted: U256::from(u128::MAX),
            }
        );
        assert_no_request_brief_state(&storage);
    });
    use alloy_sol_types::SolEvent;
    let logs = provider.get_events(outbe_primitives::addresses::DESIS_ADDRESS);
    assert_eq!(logs.len(), 1);
    let event =
        crate::precompile::IDesis::AuctionBriefRejectedToCarryOver::decode_log_data(&logs[0])
            .unwrap();
    assert_eq!(event.worldwideDay, WORLDWIDE_DAY.value());
    assert_eq!(event.supply, supply);
    assert_eq!(event.maxAccepted, U256::from(u128::MAX));
    assert_eq!(
        event.reasonCode,
        AuctionBriefRejectionReason::SupplyExceedsAuctionDomain.code()
    );
}

#[test]
fn invalid_day_duplicate_and_anchor_overflow_are_errors_without_business_events() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        assert!(crate::api::dispatch_auction_brief(
            storage.clone(),
            WorldwideDay::new(0),
            U256::MAX,
            entry_price_rows(),
            true,
            NOW,
        )
        .is_err());

        brief_at(&storage, WORLDWIDE_DAY, 1, true);
        assert!(crate::api::dispatch_auction_brief(
            storage.clone(),
            WORLDWIDE_DAY,
            U256::MAX,
            entry_price_rows(),
            true,
            NOW,
        )
        .is_err());

        let midnight = u64::MAX - u64::MAX % outbe_primitives::time::SECONDS_PER_DAY;
        let late = midnight
            + (crate::constants::COMMIT_WINDOW_SECONDS
                - crate::constants::MIN_COMMIT_WINDOW_SECONDS)
            + 1;
        assert!(crate::api::dispatch_auction_brief(
            storage,
            NEXT_WORLDWIDE_DAY,
            U256::MAX,
            entry_price_rows(),
            true,
            late,
        )
        .is_err());
    });
    assert!(provider
        .get_events(outbe_primitives::addresses::DESIS_ADDRESS)
        .is_empty());
}

#[test]
fn auction_brief_rolls_back_every_partial_write_and_event_fault() {
    let mutation_count = {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let result = StorageHandle::enter(&mut provider, |storage| {
            crate::api::dispatch_auction_brief(
                storage,
                WORLDWIDE_DAY,
                U256::from(7 * PROMIS_LOAD_MINOR),
                entry_price_rows(),
                true,
                NOW,
            )
        });
        assert_eq!(result.unwrap(), AuctionBriefReceipt::Accepted);
        provider.clear_mutation_failure()
    };
    assert!(mutation_count > 1);

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.fail_after_mutation_at(operation);
        let result = StorageHandle::enter(&mut provider, |storage| {
            crate::api::dispatch_auction_brief(
                storage,
                WORLDWIDE_DAY,
                U256::from(7 * PROMIS_LOAD_MINOR),
                entry_price_rows(),
                true,
                NOW,
            )
        });
        assert!(result.is_err(), "mutation {operation} must propagate");
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        StorageHandle::enter(&mut provider, |storage| {
            assert_no_request_brief_state(&storage)
        });
        assert!(provider.get_ordered_events().is_empty());
    }

    let mut rejection_provider = HashMapStorageProvider::new(CHAIN_ID);
    rejection_provider.fail_after_mutation_at(0);
    let result = StorageHandle::enter(&mut rejection_provider, |storage| {
        crate::api::dispatch_auction_brief(
            storage,
            WORLDWIDE_DAY,
            U256::MAX,
            entry_price_rows(),
            true,
            NOW,
        )
    });
    assert!(result.is_err());
    assert_eq!(rejection_provider.clear_mutation_failure(), 1);
    assert!(rejection_provider.get_ordered_events().is_empty());
    StorageHandle::enter(&mut rejection_provider, |storage| {
        assert_no_request_brief_state(&storage)
    });
}

// --- Late-brief deferral ---

fn brief_anchor_at(now: u64) -> u64 {
    with_storage(|s| {
        assert_eq!(
            crate::api::dispatch_auction_brief(
                s.clone(),
                WORLDWIDE_DAY,
                U256::from(LOAD_MINOR),
                entry_price_rows(),
                true,
                now,
            )
            .unwrap(),
            AuctionBriefReceipt::Accepted
        );
        u64::from(
            s.contract::<DesisContract>()
                .auction_at
                .read(&WORLDWIDE_DAY)
                .unwrap(),
        )
    })
}

#[test]
fn brief_anchors_to_this_midnight_within_grace() {
    assert_eq!(brief_anchor_at(ANCHOR + 4 * 3600), ANCHOR);
    assert_eq!(brief_anchor_at(ANCHOR + 6 * 3600), ANCHOR);
}

#[test]
fn brief_defers_past_grace_to_next_midnight() {
    assert_eq!(brief_anchor_at(ANCHOR + 6 * 3600 + 1), ANCHOR + 86_400);
    assert_eq!(brief_anchor_at(ANCHOR + 12 * 3600), ANCHOR + 86_400);
}

#[test]
fn schedule_starts_a_deferred_brief_at_the_next_midnight() {
    with_storage(|s| {
        let noon = ANCHOR + 12 * 3600;
        assert_eq!(
            crate::api::dispatch_auction_brief(
                s.clone(),
                WORLDWIDE_DAY,
                U256::from(10 * LOAD_MINOR),
                entry_price_rows(),
                true,
                noon,
            )
            .unwrap(),
            AuctionBriefReceipt::Accepted
        );
        assert_eq!(
            u64::from(
                s.contract::<DesisContract>()
                    .auction_at
                    .read(&WORLDWIDE_DAY)
                    .unwrap()
            ),
            ANCHOR + 86_400
        );
        runtime::schedule_tick(&s, noon).unwrap();
        assert_eq!(
            s.contract::<DesisContract>()
                .read_stage(WORLDWIDE_DAY)
                .unwrap(),
            AuctionStage::Briefed
        );
        runtime::schedule_tick(&s, ANCHOR + 86_400).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Started
        );
    });
}

// --- Schedule tick ---

#[test]
fn schedule_starts_a_green_brief() {
    with_storage(|s| {
        brief(&s, true);
        runtime::schedule_tick(&s, NOW).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Started
        );
        let cfg = contract.read_auction_config(WORLDWIDE_DAY).unwrap();
        assert!(cfg.commit_bond_minor > 0, "profile folded at start");
    });
}

#[test]
fn schedule_flips_to_revealing_at_commit_end() {
    with_storage(|s| {
        brief(&s, true);
        runtime::schedule_tick(&s, NOW).unwrap();
        runtime::schedule_tick(&s, ANCHOR + 86_400).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Revealing
        );
        assert_eq!(contract.clearing_initiated.read(&WORLDWIDE_DAY).unwrap(), 0);
    });
}

#[test]
fn schedule_arms_the_clearing_gate_at_reveal_end() {
    with_storage(|s| {
        brief(&s, true);
        runtime::schedule_tick(&s, NOW).unwrap();
        runtime::schedule_tick(&s, ANCHOR + 86_400).unwrap();
        runtime::schedule_tick(&s, ANCHOR + 2 * 86_400).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Clearing
        );
        assert_eq!(contract.clearing_initiated.read(&WORLDWIDE_DAY).unwrap(), 1);
        assert_eq!(
            contract.pending_supply_intex.read(&WORLDWIDE_DAY).unwrap(),
            10
        );
        assert_eq!(contract.gate_active_count.read().unwrap(), 1);
        assert_eq!(
            contract.clearing_deadline.read(&WORLDWIDE_DAY).unwrap(),
            ANCHOR + 2 * 86_400 + crate::constants::BIDS_FANIN_TIMEOUT_SECS
        );
    });
}

#[test]
fn schedule_catches_up_over_missed_ticks() {
    with_storage(|s| {
        brief(&s, true);
        runtime::schedule_tick(&s, NOW).unwrap();
        runtime::schedule_tick(&s, ANCHOR + 2 * 86_400 + 3600).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Clearing
        );
        assert_eq!(contract.clearing_initiated.read(&WORLDWIDE_DAY).unwrap(), 1);
    });
}

#[test]
fn schedule_cancels_a_red_brief() {
    with_storage(|s| {
        brief(&s, false);
        runtime::schedule_tick(&s, NOW).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Cancelled
        );
        assert_eq!(contract.sched_active_count.read().unwrap(), 0);
    });
}

#[test]
fn schedule_cancels_a_missed_start() {
    use outbe_promislimit::PromisLimitContract;
    with_storage(|s| {
        brief(&s, true);
        runtime::schedule_tick(&s, ANCHOR + 86_400 + 3600).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Cancelled
        );
        assert_eq!(contract.sched_active_count.read().unwrap(), 0);
        assert_eq!(contract.gate_active_count.read().unwrap(), 0);
        assert_eq!(
            PromisLimitContract::new(s.clone())
                .get_total_unallocated()
                .unwrap(),
            U256::from(10 * LOAD_MINOR)
        );
    });
}

#[test]
fn schedule_retires_an_overdue_day() {
    use crate::precompile::IDesis;
    use alloy_sol_types::SolEvent;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(NOW));
    storage.stub_sub_call_at(ORIGIN_ROUTER_ADDRESS, targets_stub(&[SRC_CHAIN]));
    storage.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, |s| {
        use outbe_promislimit::PromisLimitContract;
        brief(&s, true);
        runtime::schedule_tick(&s, NOW).unwrap();
        runtime::schedule_tick(&s, ANCHOR + 3 * 86_400).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(contract.sched_active_count.read().unwrap(), 0);
        assert_eq!(contract.gate_active_count.read().unwrap(), 0);
        assert_eq!(
            PromisLimitContract::new(s.clone())
                .get_total_unallocated()
                .unwrap(),
            U256::from(10 * LOAD_MINOR)
        );
    });
    let overdue_sig = IDesis::AuctionOverdue::SIGNATURE_HASH;
    let found = storage
        .get_events(outbe_primitives::addresses::DESIS_ADDRESS)
        .iter()
        .any(|log| log.topics().first() == Some(&overdue_sig));
    assert!(found, "expected AuctionOverdue event");
}

#[test]
fn schedule_derives_min_bid_qty_from_prior_clearing() {
    with_storage(|s| {
        open_clearing(&s, 100);
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            (0..100u8)
                .map(|i| BidData {
                    bidder_address: bidder(i),
                    intex_bid_rate: 200,
                    timestamp: i as u32,
                    intex_quantity: 1,
                    issuance_currency: QUALIFIER_REFERENCE_ISO,
                    reference_currency: QUALIFIER_REFERENCE_ISO,
                })
                .collect(),
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 100);
        clear(&s);

        brief_at(&s, NEXT_WORLDWIDE_DAY, 10 * LOAD_MINOR, true);
        runtime::schedule_tick(&s, NOW).unwrap();
        let contract = s.contract::<DesisContract>();
        let min_qty = contract
            .config_min_bid_quantity
            .read(&(NEXT_WORLDWIDE_DAY))
            .unwrap();
        assert_eq!(min_qty, 4);
    });
}

// --- Origin gate (OriginRouter-only entries) ---

#[test]
fn process_bids_in_non_revealing_stage_fails() {
    with_storage(|s| {
        brief(&s, true);
        runtime::schedule_tick(&s, NOW).unwrap();
        // Stage is Started, not Revealing — must be rejected.
        assert!(runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(2, 200)
        )
        .is_err());
    });
}

#[test]
fn process_bids_rejects_non_origin_caller() {
    with_storage(|s| {
        open_revealing(&s);
        // Series is in Revealing, so the only admission gate left is the caller check.
        let attacker = bidder(99);
        assert!(runtime::process_bids_batch(
            s.clone(),
            attacker,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(3, 200)
        )
        .is_err());
        let contract = s.contract::<DesisContract>();
        assert_eq!(contract.day_bid_count.read(&WORLDWIDE_DAY).unwrap(), 0);
    });
}

// --- Bid ingestion ---

#[test]
fn process_bids_accumulate_across_batches() {
    with_storage(|s| {
        open_revealing(&s);

        // Two batches of generation 1 (total_batches=2) accumulate for the chain. Intake stays
        // Revealing — nothing auto-transitions; the chain finalizes only on its BIDS_DONE marker.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            2,
            bids(3, 200),
        )
        .unwrap();
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            1,
            2,
            bids(2, 150),
        )
        .unwrap();

        let contract = s.contract::<DesisContract>();
        assert_eq!(contract.day_bid_count.read(&WORLDWIDE_DAY).unwrap(), 5);
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Revealing
        );
        // No marker yet, so the chain is not done.
        assert!(
            contract
                .chain_done
                .read(&DesisContract::chain_key(WORLDWIDE_DAY, SRC_CHAIN))
                .unwrap()
                == 0
        );
    });
}

#[test]
fn marker_finalizes_chain_once_batches_and_totals_match() {
    with_storage(|s| {
        open_revealing(&s);
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(4, 200),
        )
        .unwrap();
        // Marker with matching totals opens the gate for this chain.
        mark_done(&s, SRC_CHAIN, 1, 1, 4);
        let contract = s.contract::<DesisContract>();
        assert!(
            contract
                .chain_done
                .read(&DesisContract::chain_key(WORLDWIDE_DAY, SRC_CHAIN))
                .unwrap()
                == 1
        );
    });
}

#[test]
fn marker_arriving_before_batches_still_finalizes() {
    with_storage(|s| {
        open_revealing(&s);
        // Marker races ahead of the batches over the unordered bridge: it can't finalize yet
        // (generation not seen), so it reverts and the transport redelivers it after the batches.
        assert!(runtime::process_bids_done(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            1,
            2
        )
        .is_err());
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(2, 200),
        )
        .unwrap();
        // Redelivered marker now matches and finalizes.
        mark_done(&s, SRC_CHAIN, 1, 1, 2);
        let contract = s.contract::<DesisContract>();
        assert!(
            contract
                .chain_done
                .read(&DesisContract::chain_key(WORLDWIDE_DAY, SRC_CHAIN))
                .unwrap()
                == 1
        );
    });
}

#[test]
fn marker_total_mismatch_keeps_chain_not_done() {
    with_storage(|s| {
        open_revealing(&s);
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(3, 200),
        )
        .unwrap();
        // Marker claims 5 bids but only 3 arrived: the integrity check keeps the chain not-done.
        mark_done(&s, SRC_CHAIN, 1, 1, 5);
        let contract = s.contract::<DesisContract>();
        assert!(
            contract
                .chain_done
                .read(&DesisContract::chain_key(WORLDWIDE_DAY, SRC_CHAIN))
                .unwrap()
                == 0
        );
    });
}

#[test]
fn higher_generation_replaces_bids() {
    with_storage(|s| {
        open_revealing(&s);

        // Gen 1 arrives incomplete (batch 0 of 2), so it never finalizes.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            2,
            bids(5, 200),
        )
        .unwrap();
        // Gen 2 supersedes with its own single completing batch.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            2,
            0,
            1,
            bids(2, 150),
        )
        .unwrap();

        let contract = s.contract::<DesisContract>();
        assert_eq!(contract.day_bid_count.read(&WORLDWIDE_DAY).unwrap(), 2);
    });
}

#[test]
fn superseding_generation_resets_done_flag() {
    with_storage(|s| {
        open_revealing(&s);
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(2, 200),
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 2);
        let key = DesisContract::chain_key(WORLDWIDE_DAY, SRC_CHAIN);
        assert!(s.contract::<DesisContract>().chain_done.read(&key).unwrap() == 1);

        // A fresh generation re-opens the chain: done is cleared until the new marker lands.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            2,
            0,
            1,
            bids(3, 150),
        )
        .unwrap();
        let contract = s.contract::<DesisContract>();
        assert!(contract.chain_done.read(&key).unwrap() == 0);
        assert_eq!(contract.day_bid_count.read(&WORLDWIDE_DAY).unwrap(), 3);
    });
}

#[test]
fn stale_generation_is_rejected() {
    with_storage(|s| {
        open_revealing(&s);

        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            2,
            0,
            2,
            bids(1, 200),
        )
        .unwrap();
        assert!(runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(1, 200)
        )
        .is_err());
    });
}

#[test]
fn no_bids_clears_as_no_sale() {
    with_storage(|s| {
        open_clearing(&s, 10);
        // Lysis recorded creator rewards for the day before the auction concluded.
        outbe_intex::api::record_contributors(
            &s,
            WORLDWIDE_DAY.value(),
            &[(bidder(9), U256::from(100u64))],
        )
        .unwrap();
        // A single empty batch (batch 0 of 1) plus a zero-bid marker finalizes the chain.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            vec![],
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 0);

        // Clearing a zero-bid auction is a no-sale: Cleared with 0 issued and no winners (the
        // AuctionResult(0,0,0) lets the target chain finalize to Completed instead of stalling).
        let result = clear(&s);
        assert_eq!(result.issued_intex_count, 0);
        assert!(result.winners.is_empty());
        assert_eq!(
            s.contract::<DesisContract>()
                .read_stage(WORLDWIDE_DAY)
                .unwrap(),
            AuctionStage::Cleared
        );
        // No series will ever exist for the day, so the contributor map is discarded.
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WORLDWIDE_DAY.value()).unwrap(),
            0
        );
    });
}

// --- Clearing algorithm ---

#[test]
fn clearing_allocates_up_to_supply() {
    with_storage(|s| {
        let supply = 3u32;
        open_clearing(&s, supply as u128);
        // 5 bidders competing for 3 supply units.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(5, 200),
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 5);
        let result = clear(&s);
        assert_eq!(result.issued_intex_count, supply);
        assert_eq!(result.winners.len(), supply as usize);
    });
}

#[test]
fn clearing_transitions_to_cleared() {
    with_storage(|s| {
        open_clearing(&s, 1);
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(1, 200),
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 1);
        clear(&s);
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Cleared
        );
        // The gate is released once the day clears.
        assert_eq!(contract.gate_active_count.read().unwrap(), 0);
    });
}

#[test]
fn zero_supply_brief_arms_clearing() {
    with_storage(|s| {
        open_clearing(&s, 0);
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.pending_supply_intex.read(&WORLDWIDE_DAY).unwrap(),
            0
        );
        assert_eq!(contract.clearing_initiated.read(&WORLDWIDE_DAY).unwrap(), 1);
    });
}

#[test]
fn clearing_empty_supply_refunds_all_bidders() {
    with_storage(|s| {
        open_clearing(&s, 0);
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(3, 200),
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 3);
        let result = clear(&s);

        assert_eq!(result.issued_intex_count, 0);
        assert!(result.winners.is_empty());
        assert_eq!(result.all_bidders.len(), 3);
        assert!(result.paid_amounts.iter().all(|&p| p == 0));
        assert!(result.refunded_amounts.iter().all(|&r| r > 0));

        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Cleared
        );
    });
}

#[test]
fn clearing_uniform_price_is_last_allocated_bid() {
    with_storage(|s| {
        open_clearing(&s, 2);
        // Three bids at descending prices: 300, 200, 150.
        let three_bids = vec![
            BidData {
                bidder_address: bidder(0),
                intex_bid_rate: 300,
                timestamp: 0,
                intex_quantity: 1,
                issuance_currency: QUALIFIER_REFERENCE_ISO,
                reference_currency: QUALIFIER_REFERENCE_ISO,
            },
            BidData {
                bidder_address: bidder(1),
                intex_bid_rate: 200,
                timestamp: 1,
                intex_quantity: 1,
                issuance_currency: QUALIFIER_REFERENCE_ISO,
                reference_currency: QUALIFIER_REFERENCE_ISO,
            },
            BidData {
                bidder_address: bidder(2),
                intex_bid_rate: 150,
                timestamp: 2,
                intex_quantity: 1,
                issuance_currency: QUALIFIER_REFERENCE_ISO,
                reference_currency: QUALIFIER_REFERENCE_ISO,
            },
        ];
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            three_bids,
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 3);
        let result = clear(&s);
        // Supply 2 → top 2 bids win (300 and 200); clearing rate = 200.
        assert_eq!(result.clearing_rate, 200);
        assert_eq!(result.issued_intex_count, 2);
    });
}

#[test]
fn clear_bids_below_min_price_skipped() {
    with_storage(|s| {
        open_clearing(&s, 3);
        s.contract::<DesisContract>()
            .config_min_bid_rate
            .write(&WORLDWIDE_DAY, 100)
            .unwrap();
        let low_bids = vec![
            BidData {
                bidder_address: bidder(0),
                intex_bid_rate: 50,
                timestamp: 0,
                intex_quantity: 1,
                issuance_currency: QUALIFIER_REFERENCE_ISO,
                reference_currency: QUALIFIER_REFERENCE_ISO,
            },
            BidData {
                bidder_address: bidder(1),
                intex_bid_rate: 200,
                timestamp: 1,
                intex_quantity: 1,
                issuance_currency: QUALIFIER_REFERENCE_ISO,
                reference_currency: QUALIFIER_REFERENCE_ISO,
            },
        ];
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            low_bids,
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 2);
        let result = clear(&s);
        // Only bid at 200 clears; bid at 50 < min_bid_price=100 is skipped.
        assert_eq!(result.issued_intex_count, 1);
    });
}

#[test]
fn clear_refunds_equal_locked_minus_paid() {
    with_storage(|s| {
        let supply = 1u32;
        open_clearing(&s, supply as u128);
        // Winner bids 300, clearing price will be 300 (only one slot).
        let two_bids = vec![
            BidData {
                bidder_address: bidder(0),
                intex_bid_rate: 300,
                timestamp: 0,
                intex_quantity: 1,
                issuance_currency: QUALIFIER_REFERENCE_ISO,
                reference_currency: QUALIFIER_REFERENCE_ISO,
            },
            BidData {
                bidder_address: bidder(1),
                intex_bid_rate: 200,
                timestamp: 1,
                intex_quantity: 1,
                issuance_currency: QUALIFIER_REFERENCE_ISO,
                reference_currency: QUALIFIER_REFERENCE_ISO,
            },
        ];
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            two_bids,
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 2);
        let result = clear(&s);
        // escrow basis = promis_load; lock/pay = qty * basis * rate / RATE_SCALE.
        // Winner (rate 300): paid at clearing 300, refund 0. Loser (rate 200): refund = its lock.
        let w_idx = result
            .all_bidders
            .iter()
            .position(|&a| a == bidder(0))
            .unwrap();
        let l_idx = result
            .all_bidders
            .iter()
            .position(|&a| a == bidder(1))
            .unwrap();
        assert_eq!(result.paid_amounts[w_idx], LOAD_MINOR * 300 / 1_000_000);
        assert_eq!(result.refunded_amounts[w_idx], 0);
        assert_eq!(result.refunded_amounts[l_idx], LOAD_MINOR * 200 / 1_000_000);
        assert_eq!(supply, result.issued_intex_count);
    });
}

#[test]
fn clear_rate_escrow_scales_by_basis() {
    // escrow basis != RATE_SCALE, so this exercises the * basis / RATE_SCALE.
    with_storage(|s| {
        open_clearing(&s, 2);
        let rate_bids = vec![
            BidData {
                bidder_address: bidder(0),
                intex_bid_rate: 800_000,
                timestamp: 0,
                intex_quantity: 1,
                issuance_currency: QUALIFIER_REFERENCE_ISO,
                reference_currency: QUALIFIER_REFERENCE_ISO,
            },
            BidData {
                bidder_address: bidder(1),
                intex_bid_rate: 600_000,
                timestamp: 1,
                intex_quantity: 1,
                issuance_currency: QUALIFIER_REFERENCE_ISO,
                reference_currency: QUALIFIER_REFERENCE_ISO,
            },
            BidData {
                bidder_address: bidder(2),
                intex_bid_rate: 400_000,
                timestamp: 2,
                intex_quantity: 1,
                issuance_currency: QUALIFIER_REFERENCE_ISO,
                reference_currency: QUALIFIER_REFERENCE_ISO,
            },
        ];
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            rate_bids,
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 3);
        let result = clear(&s);

        assert_eq!(result.clearing_rate, 600_000);
        // lock/pay = qty * promis_load * rate / 1e6; clearing rate 60%.
        let idx = |a: Address| result.all_bidders.iter().position(|&x| x == a).unwrap();
        assert_eq!(
            result.paid_amounts[idx(bidder(0))],
            LOAD_MINOR * 600_000 / 1_000_000
        );
        assert_eq!(
            result.refunded_amounts[idx(bidder(0))],
            LOAD_MINOR * 200_000 / 1_000_000
        );
        assert_eq!(
            result.paid_amounts[idx(bidder(1))],
            LOAD_MINOR * 600_000 / 1_000_000
        );
        assert_eq!(result.refunded_amounts[idx(bidder(1))], 0);
        assert_eq!(result.paid_amounts[idx(bidder(2))], 0);
        assert_eq!(
            result.refunded_amounts[idx(bidder(2))],
            LOAD_MINOR * 400_000 / 1_000_000
        );
    });
}

#[test]
fn clearing_returns_unsold_supply_and_dust_to_promis() {
    use outbe_promislimit::PromisLimitContract;

    with_storage(|s| {
        brief_at(&s, WORLDWIDE_DAY, 3 * LOAD_MINOR + 7, true);
        runtime::schedule_tick(&s, NOW).unwrap();
        runtime::schedule_tick(&s, ANCHOR + 86_400).unwrap();
        arm_clearing(&s);
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(1, 200),
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 1);
        let result = clear(&s);

        assert_eq!(result.issued_intex_count, 1);
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.pending_supply_promis.read(&WORLDWIDE_DAY).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            PromisLimitContract::new(s.clone())
                .get_total_unallocated()
                .unwrap(),
            U256::from(2 * LOAD_MINOR + 7),
            "unsold whole units and conversion dust return to PromisLimit"
        );
    });
}

// --- Multi-chain fan-in gate ---

/// Auction clearing over two target chains: bids merge into one clearing, and each
/// winner/bidder is tagged with its source chain for per-chain result/refund routing.
#[test]
fn two_chain_bids_merge_and_carry_source_chain() {
    let chain_a = 10u32;
    let chain_b = 20u32;
    with_targets(&[chain_a, chain_b], |s| {
        open_clearing(&s, 3);

        // Chain A: one bid at 300. Chain B: one bid at 200.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            chain_a,
            1,
            0,
            1,
            vec![BidData {
                bidder_address: bidder(1),
                intex_bid_rate: 300,
                timestamp: 0,
                intex_quantity: 1,
                issuance_currency: QUALIFIER_REFERENCE_ISO,
                reference_currency: QUALIFIER_REFERENCE_ISO,
            }],
        )
        .unwrap();
        mark_done(&s, chain_a, 1, 1, 1);
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            chain_b,
            1,
            0,
            1,
            vec![BidData {
                bidder_address: bidder(2),
                intex_bid_rate: 200,
                timestamp: 0,
                intex_quantity: 1,
                issuance_currency: QUALIFIER_REFERENCE_ISO,
                reference_currency: QUALIFIER_REFERENCE_ISO,
            }],
        )
        .unwrap();
        mark_done(&s, chain_b, 1, 1, 1);

        let result = clear(&s);
        assert_eq!(result.issued_intex_count, 2);
        // Both bidders win; each is tagged with its own chain.
        let a = result.winners.iter().position(|&w| w == bidder(1)).unwrap();
        let b = result.winners.iter().position(|&w| w == bidder(2)).unwrap();
        assert_eq!(result.winner_chains[a], chain_a);
        assert_eq!(result.winner_chains[b], chain_b);
        assert_eq!(result.bidder_chains.len(), 2);
    });
}

/// Manual clearing must wait until every snapshot chain has finalized.
/// The tick clears only once the gate is satisfied; before then `force_clear` yields `None`.
#[test]
fn force_clear_waits_then_fires_when_all_done() {
    let chain_a = 10u32;
    let chain_b = 20u32;
    with_targets(&[chain_a, chain_b], |s| {
        open_clearing(&s, 2);
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            chain_a,
            1,
            0,
            1,
            bids(1, 200),
        )
        .unwrap();
        mark_done(&s, chain_a, 1, 1, 1);
        // Before the deadline, a missing chain keeps the gate closed.
        assert!(runtime::force_clear(s.clone(), WORLDWIDE_DAY, NOW)
            .unwrap()
            .is_none());
        assert_eq!(
            s.contract::<DesisContract>()
                .read_stage(WORLDWIDE_DAY)
                .unwrap(),
            AuctionStage::Clearing
        );

        // Chain B reports → the gate opens and the tick clears.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            chain_b,
            1,
            0,
            1,
            bids(1, 200),
        )
        .unwrap();
        mark_done(&s, chain_b, 1, 1, 1);
        let result = runtime::force_clear(s.clone(), WORLDWIDE_DAY, NOW).unwrap();
        assert!(result.is_some());
        assert_eq!(
            s.contract::<DesisContract>()
                .read_stage(WORLDWIDE_DAY)
                .unwrap(),
            AuctionStage::Cleared
        );
    });
}

/// After the deadline, clearing proceeds without the missing chain and reports it skipped.
#[test]
fn force_clear_skips_missing_chain_after_deadline() {
    use crate::precompile::IDesis;
    use alloy_sol_types::SolEvent;

    let chain_a = 10u32;
    let chain_b = 20u32;
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(NOW));
    storage.stub_sub_call_at(ORIGIN_ROUTER_ADDRESS, targets_stub(&[chain_a, chain_b]));
    storage.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );

    let cleared = StorageHandle::enter(&mut storage, |s| {
        open_clearing(&s, 2);
        // Only chain A finalizes.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            chain_a,
            1,
            0,
            1,
            bids(1, 200),
        )
        .unwrap();
        mark_done(&s, chain_a, 1, 1, 1);
        // Past the deadline the gate clears without chain B.
        let deadline = ANCHOR + 2 * 86_400 + crate::constants::BIDS_FANIN_TIMEOUT_SECS;
        let result = runtime::force_clear(s.clone(), WORLDWIDE_DAY, deadline + 1).unwrap();
        assert!(result.is_some());
        let result = result.unwrap();
        // Only chain A's bid participated.
        assert_eq!(result.issued_intex_count, 1);
        assert!(result.bidder_chains.iter().all(|&c| c == chain_a));
        s.contract::<DesisContract>()
            .read_stage(WORLDWIDE_DAY)
            .unwrap()
    });
    assert_eq!(cleared, AuctionStage::Cleared);

    // The missing chain is reported skipped.
    let desis_addr = outbe_primitives::addresses::DESIS_ADDRESS;
    let skip_sig = IDesis::ChainSkipped::SIGNATURE_HASH;
    let found = storage.get_events(desis_addr).iter().any(|log| {
        log.topics().first() == Some(&skip_sig)
            && IDesis::ChainSkipped::decode_log_data(log)
                .map(|ev| ev.worldwideDay == WORLDWIDE_DAY.value() && ev.srcChainId == chain_b)
                .unwrap_or(false)
    });
    assert!(found, "expected ChainSkipped for the missing chain");
}

#[test]
fn tick_gate_no_active_days_is_noop() {
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    with_storage(|s| {
        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(1, NOW, CHAIN_ID), s.clone());
        runtime::tick_gate(&ctx).unwrap();
    });
}

#[test]
fn tick_gate_clears_ready_day() {
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    with_storage(|s| {
        open_clearing(&s, 1);
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            SRC_CHAIN,
            1,
            0,
            1,
            bids(1, 200),
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 1);

        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(1, NOW, CHAIN_ID), s.clone());
        runtime::tick_gate(&ctx).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Cleared
        );
        assert_eq!(contract.gate_active_count.read().unwrap(), 0);
    });
}

#[test]
fn test_iface_id_matches_selector_xor() {
    use crate::precompile::IDesis;
    use alloy_sol_types::SolCall;

    // `IDESIS_INTERFACE_ID` is what OriginRouter probes: `type(IDesis).interfaceId` of the
    // router-facing interface (contracts/intex/src/origin/interfaces/IDesis.sol) — the four
    // functions it declares. The precompile's extra diagnostic views (getChainBidsCount,
    // isChainDone) are not part of that interface, so they are excluded from the XOR.
    let xor: [u8; 4] = [
        IDesis::processBidsBatchCall::SELECTOR,
        IDesis::processBidsDoneCall::SELECTOR,
        IDesis::getAuctionStageCall::SELECTOR,
        IDesis::getBidsCountCall::SELECTOR,
    ]
    .into_iter()
    .fold([0u8; 4], |acc, sel| {
        [
            acc[0] ^ sel[0],
            acc[1] ^ sel[1],
            acc[2] ^ sel[2],
            acc[3] ^ sel[3],
        ]
    });

    assert_eq!(
        xor,
        crate::precompile::IDESIS_INTERFACE_ID,
        "IDESIS_INTERFACE_ID is stale; update it to match the new selector XOR"
    );
}

// --- Clearing: one series per currency pair ---

/// Brief `units` of supply against several priced reference currencies and drive
/// the schedule until the clearing gate is armed.
fn open_clearing_priced(s: &StorageHandle, units: u128, references: &[u16]) {
    let rows = references
        .iter()
        .map(|&iso_code| crate::schema::ReferencePrice {
            iso_code,
            entry_price_minor: U256::from(ENTRY_PRICE) * U256::from(iso_code),
        })
        .collect();
    assert_eq!(
        crate::api::dispatch_auction_brief(
            s.clone(),
            WORLDWIDE_DAY,
            U256::from(units * LOAD_MINOR),
            rows,
            true,
            NOW,
        )
        .unwrap(),
        AuctionBriefReceipt::Accepted
    );
    runtime::schedule_tick(s, NOW).unwrap();
    runtime::schedule_tick(s, ANCHOR + 86_400).unwrap();
    arm_clearing(s);
}

#[test]
fn clearing_issues_one_series_per_winning_currency_pair() {
    use outbe_intexfactory::SeriesId;

    let chain = 10u32;
    with_targets(&[chain], |s| {
        open_clearing_priced(&s, 4, &[840, 978]);

        // Three winners over two pairs: two price in USD, one in EUR.
        let mut relayed = bids(3, 200);
        relayed[2].issuance_currency = 949;
        relayed[2].reference_currency = 978;
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            chain,
            1,
            0,
            1,
            relayed,
        )
        .unwrap();
        mark_done(&s, chain, 1, 1, 3);

        let result = clear(&s);
        assert_eq!(result.issued_intex_count, 3);

        let usd = SeriesId::for_pair(WORLDWIDE_DAY.value(), 840, 840).unwrap();
        let lira = SeriesId::for_pair(WORLDWIDE_DAY.value(), 949, 978).unwrap();
        assert_eq!(usd.to_string(), "20260101-USD-U");
        assert_eq!(lira.to_string(), "20260101-TRY-E");

        // Each series holds only its own winners and its own reference price.
        let usd_series = outbe_intex::api::read_series(&s, usd).unwrap();
        let lira_series = outbe_intex::api::read_series(&s, lira).unwrap();
        assert_eq!(usd_series.issued_intex_count, 2);
        assert_eq!(lira_series.issued_intex_count, 1);
        assert_eq!(
            usd_series.entry_price_minor,
            U256::from(ENTRY_PRICE) * U256::from(840u16)
        );
        assert_eq!(
            lira_series.entry_price_minor,
            U256::from(ENTRY_PRICE) * U256::from(978u16)
        );
    });
}

#[test]
fn a_reference_currency_whose_letter_is_taken_is_dropped_from_the_day() {
    // CHF and CNY both spell their series `C`. The day keeps the lower code, so the
    // survivor cannot depend on the order the two brief paths collect prices in.
    with_storage(|s| {
        open_clearing_priced(&s, 4, &[756, 156]);
        let config = s
            .contract::<DesisContract>()
            .read_auction_config(WORLDWIDE_DAY)
            .unwrap();
        assert_eq!(
            config
                .reference_prices
                .iter()
                .map(|row| row.iso_code)
                .collect::<Vec<_>>(),
            vec![156]
        );
        assert!(config.entry_price_for(756).is_none());
    });
}

#[test]
fn clearing_without_winners_discards_the_day_contributor_map() {
    let chain = 10u32;
    with_targets(&[chain], |s| {
        open_clearing(&s, 2);
        outbe_intex::api::record_contributors(
            &s,
            WORLDWIDE_DAY.value(),
            &[(bidder(1), U256::from(100u64))],
        )
        .unwrap();

        // The only chain never reports, so the deadline clears the day with no bids.
        let deadline = ANCHOR + 2 * 86_400 + crate::constants::BIDS_FANIN_TIMEOUT_SECS;
        let result = runtime::force_clear(s.clone(), WORLDWIDE_DAY, deadline + 1)
            .unwrap()
            .unwrap();
        assert_eq!(result.issued_intex_count, 0);
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WORLDWIDE_DAY.value()).unwrap(),
            0
        );
    });
}

// --- Config construction ---

#[test]
fn escrow_basis_is_promis_load() {
    // wCOEN escrow basis = promis_load per Intex; entry no longer drives it.
    let cfg = AuctionConfig::from_reference_prices(vec![crate::schema::ReferencePrice {
        iso_code: QUALIFIER_REFERENCE_ISO,
        entry_price_minor: U256::from(1_000_000_150_000_000u128),
    }]);
    assert_eq!(cfg.escrow_basis_minor(), cfg.promis_load_minor);
}

// --- Refund fan-out chunking ---

#[test]
fn a_chains_bidders_ship_in_chunks_the_encoder_can_carry() {
    use crate::constants::{MAX_REFUND_CHUNKS, REFUND_CHUNK_LEN};

    // A chain relays every bidder it took, winners and losers alike, so the set is
    // bounded by bid intake rather than by supply.
    assert_eq!(runtime::refund_chunk_count(1).unwrap(), 1);
    assert_eq!(runtime::refund_chunk_count(REFUND_CHUNK_LEN).unwrap(), 1);
    assert_eq!(
        runtime::refund_chunk_count(REFUND_CHUNK_LEN + 1).unwrap(),
        2
    );

    // Intake's own ceiling — 64 bids across 256 batches — is exactly what the
    // arrival set can carry, and one bidder more is refused rather than truncated.
    let ceiling = REFUND_CHUNK_LEN * MAX_REFUND_CHUNKS;
    assert_eq!(
        runtime::refund_chunk_count(ceiling).unwrap(),
        MAX_REFUND_CHUNKS
    );
    assert!(runtime::refund_chunk_count(ceiling + 1).is_err());
}

// --- Days the oracle could not price ---

#[test]
fn a_day_nobody_could_price_is_cancelled_rather_than_failed() {
    // An oracle gap prices nothing — the same condition that makes a day red.
    // Settlement still has to complete, so the day must reach a terminal stage
    // instead of failing the brief.
    with_storage(|s| {
        assert_eq!(
            crate::api::dispatch_auction_brief(
                s.clone(),
                WORLDWIDE_DAY,
                U256::from(4 * LOAD_MINOR),
                Vec::new(),
                true,
                NOW,
            )
            .unwrap(),
            AuctionBriefReceipt::Accepted
        );

        runtime::schedule_tick(&s, NOW).unwrap();
        runtime::schedule_tick(&s, ANCHOR + 86_400).unwrap();

        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Cancelled,
            "an unpriced day ends as a closed record"
        );
        assert_eq!(
            contract.sched_active_count.read().unwrap(),
            0,
            "and leaves the schedule"
        );
        // It was briefed green, so it holds the day's PROMIS — unlike a red day, which
        // is briefed with none. Cancelling it must give that supply back.
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(s.clone())
                .get_total_unallocated()
                .unwrap(),
            U256::from(4 * LOAD_MINOR),
            "an unpriced day returns its supply"
        );
    });
}

// --- Bid intake: the currency pair ---

#[test]
fn a_relayed_bid_naming_an_unspellable_currency_is_refused_at_intake() {
    // A code no series id can spell would otherwise surface at clearing, which is the
    // one place that cannot recover: the day would revert every block until it expired.
    let chain = 10u32;
    with_targets(&[chain], |s| {
        open_clearing(&s, 2);
        let mut relayed = bids(1, 200);
        relayed[0].issuance_currency = 1949;

        let err = runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            WORLDWIDE_DAY,
            chain,
            1,
            0,
            1,
            relayed,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no series id can spell"), "{err}");
    });
}
