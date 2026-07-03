#![cfg(test)]

use alloy_primitives::{Address, Bytes, U256};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::constants::ORIGIN_ROUTER_ADDRESS;
use crate::runtime;
use crate::schema::{AuctionConfig, AuctionStage, BidData, DesisContract, IntexCallTrigger};

const CHAIN_ID: u64 = 1;
const SERIES_ID: u32 = 20260101;
const PROMIS_LOAD_MINOR: u128 = 1_000_000_000_000_000_000; // 1e18

fn bidder(n: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[19] = n;
    Address::from(bytes)
}

fn with_storage<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(1_700_000_000u64));
    // Stub OriginRouter: send* calls return bytes32 sendId (32 bytes); the value is ignored.
    storage.stub_sub_call_at(ORIGIN_ROUTER_ADDRESS, Bytes::from(vec![0u8; 32]));
    // Stub IntexNFT1155: createSeries/settle/burnSettled are void; balanceOf returns 0 (32 bytes).
    storage.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, f)
}

fn default_config() -> AuctionConfig {
    AuctionConfig {
        issuance_currency: 840,
        reference_currency: 840,
        promis_load_minor: PROMIS_LOAD_MINOR,
        call_trigger: IntexCallTrigger::default(),
        min_intex_bid_rate: 100,
        min_intex_bid_quantity: 0,
        entry_price_minor: U256::from(10_000_000_000_000u128), // 1e13, reference ccy (feeds floor/call)
    }
}

// --- State machine ---

#[test]
fn start_auction_sets_started_stage() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(SERIES_ID).unwrap(),
            AuctionStage::Started
        );
    });
}

#[test]
fn start_auction_duplicate_fails() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        assert!(runtime::start_auction(s.clone(), SERIES_ID, default_config()).is_err());
    });
}

#[test]
fn reveal_green_day_transitions_to_revealing() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(SERIES_ID).unwrap(),
            AuctionStage::Revealing
        );
    });
}

#[test]
fn reveal_red_day_transitions_to_cancelled() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, false).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(SERIES_ID).unwrap(),
            AuctionStage::Cancelled
        );
    });
}

#[test]
fn begin_clearing_stores_pending() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        let supply_promis = 10 * PROMIS_LOAD_MINOR;
        let remainder = runtime::begin_clearing(s.clone(), SERIES_ID, supply_promis).unwrap();
        assert_eq!(remainder, 0); // no rounding with exact multiple
        let contract = s.contract::<DesisContract>();
        let supply = contract.pending_supply_intex.read(&SERIES_ID).unwrap();
        assert_eq!(supply, 10);
    });
}

#[test]
fn start_auction_derives_min_bid_qty_from_prior_clearing() {
    with_storage(|s| {
        // First auction: start, reveal, clear with 100 issued (supply = 100).
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        runtime::begin_clearing(s.clone(), SERIES_ID, 100 * PROMIS_LOAD_MINOR).unwrap();
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            1,
            (0..100u8)
                .map(|i| BidData {
                    bidder_address: bidder(i),
                    intex_bid_rate: 200,
                    timestamp: i as u32,
                    intex_quantity: 1,
                })
                .collect(),
        )
        .unwrap();
        runtime::clear_auction(s.clone(), ORIGIN_ROUTER_ADDRESS, SERIES_ID).unwrap();

        // Second auction for a different series_id: min_bid_qty must be 4% of 100 = 4.
        runtime::start_auction(s.clone(), SERIES_ID + 1, default_config()).unwrap();
        let contract = s.contract::<DesisContract>();
        let min_qty = contract
            .config_min_bid_quantity
            .read(&(SERIES_ID + 1))
            .unwrap();
        assert_eq!(min_qty, 4);
    });
}

#[test]
fn process_bids_in_non_revealing_stage_fails() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        // Stage is Started, not Revealing — must be rejected.
        assert!(runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            1,
            bids(2, 200)
        )
        .is_err());
    });
}

// --- Origin gate (OriginRouter-only entries) ---

#[test]
fn process_bids_rejects_non_origin_caller() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        // Series is in Revealing, so the only admission gate left is the caller check.
        let attacker = bidder(99);
        assert!(runtime::process_bids_batch(
            s.clone(),
            attacker,
            SERIES_ID,
            1,
            1,
            0,
            1,
            bids(3, 200)
        )
        .is_err());
        let contract = s.contract::<DesisContract>();
        assert_eq!(contract.bid_count.read(&SERIES_ID).unwrap(), 0);
    });
}

#[test]
fn clear_auction_rejects_non_origin_caller() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            1,
            bids(3, 200),
        )
        .unwrap();
        // Series is in BidsReceived, so the only admission gate left is the caller check.
        let attacker = bidder(99);
        assert!(runtime::clear_auction(s.clone(), attacker, SERIES_ID).is_err());
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(SERIES_ID).unwrap(),
            AuctionStage::BidsReceived
        );
    });
}

// --- Bid ingestion ---

fn bids(n: u8, rate: u32) -> Vec<BidData> {
    (0..n)
        .map(|i| BidData {
            bidder_address: bidder(i),
            intex_bid_rate: rate,
            timestamp: i as u32,
            intex_quantity: 1,
        })
        .collect()
}

#[test]
fn process_bids_accumulate_then_finalize() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();

        // Two batches of generation 1 (total_batches=2); the stage advances only once
        // both batch_index 0 and 1 have arrived.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            2,
            bids(3, 200),
        )
        .unwrap();
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            1,
            2,
            bids(2, 150),
        )
        .unwrap();

        let contract = s.contract::<DesisContract>();
        assert_eq!(contract.read_bid_count(SERIES_ID).unwrap(), 5);
        assert_eq!(
            contract.read_stage(SERIES_ID).unwrap(),
            AuctionStage::BidsReceived
        );
    });
}

#[test]
fn higher_generation_replaces_bids() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();

        // Gen 1 arrives incomplete (batch 0 of 2), so it never finalizes.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
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
            SERIES_ID,
            1,
            2,
            0,
            1,
            bids(2, 150),
        )
        .unwrap();

        let contract = s.contract::<DesisContract>();
        assert_eq!(contract.read_bid_count(SERIES_ID).unwrap(), 2);
    });
}

#[test]
fn stale_generation_is_rejected() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();

        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            2,
            0,
            2,
            bids(1, 200),
        )
        .unwrap();
        assert!(runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            1,
            bids(1, 200)
        )
        .is_err());
    });
}

#[test]
fn no_bids_last_batch_clears_as_no_sale() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        runtime::begin_clearing(s.clone(), SERIES_ID, 10 * PROMIS_LOAD_MINOR).unwrap();
        // A single empty batch (batch 0 of 1) completes the generation and advances to
        // BidsReceived (not Cancelled), so clearing auto-fires.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            1,
            vec![],
        )
        .unwrap();
        assert_eq!(
            s.contract::<DesisContract>().read_stage(SERIES_ID).unwrap(),
            AuctionStage::BidsReceived
        );

        // Clearing a zero-bid auction is a no-sale: Cleared with 0 issued and no winners (the
        // AuctionResult(0,0,0) lets the target chain finalize to Completed instead of stalling).
        let result = runtime::clear_auction(s.clone(), ORIGIN_ROUTER_ADDRESS, SERIES_ID).unwrap();
        assert_eq!(result.issued_intex_count, 0);
        assert!(result.winners.is_empty());
        assert_eq!(
            s.contract::<DesisContract>().read_stage(SERIES_ID).unwrap(),
            AuctionStage::Cleared
        );
    });
}

// --- Clearing algorithm ---

#[test]
fn clear_auction_allocates_up_to_supply() {
    with_storage(|s| {
        let supply = 3u32;
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        runtime::begin_clearing(s.clone(), SERIES_ID, supply as u128 * PROMIS_LOAD_MINOR).unwrap();
        // 5 bidders competing for 3 supply units.
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            1,
            bids(5, 200),
        )
        .unwrap();
        let result = runtime::clear_auction(s.clone(), ORIGIN_ROUTER_ADDRESS, SERIES_ID).unwrap();
        assert_eq!(result.issued_intex_count, supply);
        assert_eq!(result.winners.len(), supply as usize);
    });
}

#[test]
fn clear_auction_transitions_to_cleared() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        runtime::begin_clearing(s.clone(), SERIES_ID, PROMIS_LOAD_MINOR).unwrap();
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            1,
            bids(1, 200),
        )
        .unwrap();
        runtime::clear_auction(s.clone(), ORIGIN_ROUTER_ADDRESS, SERIES_ID).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(SERIES_ID).unwrap(),
            AuctionStage::Cleared
        );
    });
}

#[test]
fn begin_clearing_accepts_zero_supply() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        let remainder = runtime::begin_clearing(s.clone(), SERIES_ID, 0).unwrap();
        assert_eq!(remainder, 0);
        let contract = s.contract::<DesisContract>();
        assert_eq!(contract.pending_supply_intex.read(&SERIES_ID).unwrap(), 0);
        assert_eq!(contract.clearing_initiated.read(&SERIES_ID).unwrap(), 1);
    });
}

#[test]
fn clear_auction_empty_supply_refunds_all_bidders() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        runtime::begin_clearing(s.clone(), SERIES_ID, 0).unwrap();
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            1,
            bids(3, 200),
        )
        .unwrap();
        let result = runtime::clear_auction(s.clone(), ORIGIN_ROUTER_ADDRESS, SERIES_ID).unwrap();

        assert_eq!(result.issued_intex_count, 0);
        assert!(result.winners.is_empty());
        assert_eq!(result.all_bidders.len(), 3);
        assert!(result.paid_amounts.iter().all(|&p| p == 0));
        assert!(result.refunded_amounts.iter().all(|&r| r > 0));

        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(SERIES_ID).unwrap(),
            AuctionStage::Cleared
        );
    });
}

#[test]
fn clear_auction_uniform_price_is_last_allocated_bid() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        let supply = 2u32;
        runtime::begin_clearing(s.clone(), SERIES_ID, supply as u128 * PROMIS_LOAD_MINOR).unwrap();
        // Three bids at descending prices: 300, 200, 150.
        let three_bids = vec![
            BidData {
                bidder_address: bidder(0),
                intex_bid_rate: 300,
                timestamp: 0,
                intex_quantity: 1,
            },
            BidData {
                bidder_address: bidder(1),
                intex_bid_rate: 200,
                timestamp: 1,
                intex_quantity: 1,
            },
            BidData {
                bidder_address: bidder(2),
                intex_bid_rate: 150,
                timestamp: 2,
                intex_quantity: 1,
            },
        ];
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            1,
            three_bids,
        )
        .unwrap();
        let result = runtime::clear_auction(s.clone(), ORIGIN_ROUTER_ADDRESS, SERIES_ID).unwrap();
        // Supply 2 → top 2 bids win (300 and 200); clearing rate = 200.
        assert_eq!(result.clearing_rate, 200);
        assert_eq!(result.issued_intex_count, 2);
    });
}

#[test]
fn clear_bids_below_min_price_skipped() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap(); // min_bid_price=100
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        runtime::begin_clearing(s.clone(), SERIES_ID, 3 * PROMIS_LOAD_MINOR).unwrap();
        let low_bids = vec![
            BidData {
                bidder_address: bidder(0),
                intex_bid_rate: 50,
                timestamp: 0,
                intex_quantity: 1,
            },
            BidData {
                bidder_address: bidder(1),
                intex_bid_rate: 200,
                timestamp: 1,
                intex_quantity: 1,
            },
        ];
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            1,
            low_bids,
        )
        .unwrap();
        let result = runtime::clear_auction(s.clone(), ORIGIN_ROUTER_ADDRESS, SERIES_ID).unwrap();
        // Only bid at 200 clears; bid at 50 < min_bid_price=100 is skipped.
        assert_eq!(result.issued_intex_count, 1);
    });
}

#[test]
fn clear_refunds_equal_locked_minus_paid() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        let supply = 1u32;
        runtime::begin_clearing(s.clone(), SERIES_ID, PROMIS_LOAD_MINOR).unwrap();
        // Winner bids 300, clearing price will be 300 (only one slot).
        let two_bids = vec![
            BidData {
                bidder_address: bidder(0),
                intex_bid_rate: 300,
                timestamp: 0,
                intex_quantity: 1,
            },
            BidData {
                bidder_address: bidder(1),
                intex_bid_rate: 200,
                timestamp: 1,
                intex_quantity: 1,
            },
        ];
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            1,
            two_bids,
        )
        .unwrap();
        let result = runtime::clear_auction(s.clone(), ORIGIN_ROUTER_ADDRESS, SERIES_ID).unwrap();
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
        assert_eq!(
            result.paid_amounts[w_idx],
            PROMIS_LOAD_MINOR * 300 / 1_000_000
        );
        assert_eq!(result.refunded_amounts[w_idx], 0);
        assert_eq!(
            result.refunded_amounts[l_idx],
            PROMIS_LOAD_MINOR * 200 / 1_000_000
        );
        assert_eq!(supply, result.issued_intex_count);
    });
}

#[test]
fn clear_rate_escrow_scales_by_basis() {
    // escrow basis != RATE_SCALE, so this exercises the * basis / RATE_SCALE.
    with_storage(|s| {
        let cfg = AuctionConfig {
            issuance_currency: 840,
            reference_currency: 840,
            promis_load_minor: PROMIS_LOAD_MINOR,
            call_trigger: IntexCallTrigger::default(),
            min_intex_bid_rate: 0,
            min_intex_bid_quantity: 0,
            entry_price_minor: U256::from(20_000_000_000_000u128), // 2e13 (feeds floor/call; escrow basis = promis_load)
        };
        runtime::start_auction(s.clone(), SERIES_ID, cfg).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();
        let supply = 2u32;
        runtime::begin_clearing(s.clone(), SERIES_ID, supply as u128 * PROMIS_LOAD_MINOR).unwrap();
        let rate_bids = vec![
            BidData {
                bidder_address: bidder(0),
                intex_bid_rate: 800_000,
                timestamp: 0,
                intex_quantity: 1,
            },
            BidData {
                bidder_address: bidder(1),
                intex_bid_rate: 600_000,
                timestamp: 1,
                intex_quantity: 1,
            },
            BidData {
                bidder_address: bidder(2),
                intex_bid_rate: 400_000,
                timestamp: 2,
                intex_quantity: 1,
            },
        ];
        runtime::process_bids_batch(
            s.clone(),
            ORIGIN_ROUTER_ADDRESS,
            SERIES_ID,
            1,
            1,
            0,
            1,
            rate_bids,
        )
        .unwrap();
        let result = runtime::clear_auction(s.clone(), ORIGIN_ROUTER_ADDRESS, SERIES_ID).unwrap();

        assert_eq!(result.clearing_rate, 600_000);
        // lock/pay = qty * promis_load * rate / 1e6; clearing rate 60%.
        let idx = |a: Address| result.all_bidders.iter().position(|&x| x == a).unwrap();
        assert_eq!(
            result.paid_amounts[idx(bidder(0))],
            PROMIS_LOAD_MINOR * 600_000 / 1_000_000
        );
        assert_eq!(
            result.refunded_amounts[idx(bidder(0))],
            PROMIS_LOAD_MINOR * 200_000 / 1_000_000
        );
        assert_eq!(
            result.paid_amounts[idx(bidder(1))],
            PROMIS_LOAD_MINOR * 600_000 / 1_000_000
        );
        assert_eq!(result.refunded_amounts[idx(bidder(1))], 0);
        assert_eq!(result.paid_amounts[idx(bidder(2))], 0);
        assert_eq!(
            result.refunded_amounts[idx(bidder(2))],
            PROMIS_LOAD_MINOR * 400_000 / 1_000_000
        );
    });
}

#[test]
fn test_iface_id_matches_selector_xor() {
    use crate::precompile::IDesis;
    use alloy_sol_types::SolCall;

    let xor: [u8; 4] = [
        IDesis::processBidsBatchCall::SELECTOR,
        IDesis::clearAuctionCall::SELECTOR,
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

// --- Config construction ---

#[test]
fn escrow_basis_is_promis_load() {
    // wCOEN escrow basis = promis_load per Intex; entry no longer drives it.
    let cfg = AuctionConfig::from_entry_price(U256::from(1_000_000_150_000_000u128));
    assert_eq!(cfg.escrow_basis_minor(), cfg.promis_load_minor);
}

// --- Best-effort dispatch API ---

const ENTRY_PRICE: u128 = 2_000_000_000_000_000; // 2e15 (entry feeds floor/call; escrow basis = promis_load)

#[test]
fn dispatch_stage_start_success_returns_true() {
    with_storage(|s| {
        let accepted = crate::api::dispatch_stage_start(
            s.clone(),
            outbe_primitives::time::date_key_to_utc_timestamp(SERIES_ID),
            U256::from(ENTRY_PRICE),
        )
        .unwrap();
        assert!(accepted, "valid start should be accepted");
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(SERIES_ID).unwrap(),
            AuctionStage::Started
        );
    });
}

#[test]
fn dispatch_stage_start_failure_returns_false_and_emits_event() {
    use crate::precompile::IDesis;
    use alloy_sol_types::SolEvent;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(1_700_000_000u64));
    storage.stub_sub_call_at(ORIGIN_ROUTER_ADDRESS, Bytes::from(vec![0u8; 32]));
    storage.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );

    // Second start hits the duplicate guard, so the best-effort wrapper swallows the
    // error, returns false, and emits AuctionDispatchFailed instead of propagating.
    let (first, second) = StorageHandle::enter(&mut storage, |s| {
        let first = crate::api::dispatch_stage_start(
            s.clone(),
            outbe_primitives::time::date_key_to_utc_timestamp(SERIES_ID),
            U256::from(ENTRY_PRICE),
        )
        .unwrap();
        let second = crate::api::dispatch_stage_start(
            s.clone(),
            outbe_primitives::time::date_key_to_utc_timestamp(SERIES_ID),
            U256::from(ENTRY_PRICE),
        )
        .unwrap();
        (first, second)
    });
    assert!(first, "first dispatch should succeed");
    assert!(!second, "duplicate dispatch should fail best-effort");

    let desis_addr = outbe_primitives::addresses::DESIS_ADDRESS;
    let fail_sig = IDesis::AuctionDispatchFailed::SIGNATURE_HASH;
    let found = storage.get_events(desis_addr).iter().any(|log| {
        log.topics().first() == Some(&fail_sig)
            && IDesis::AuctionDispatchFailed::decode_log_data(log)
                .map(|ev| ev.seriesId == SERIES_ID && ev.stage == "auction_stage_start")
                .unwrap_or(false)
    });
    assert!(
        found,
        "expected AuctionDispatchFailed event on DESIS_ADDRESS"
    );
}

#[test]
fn dispatch_stage_clearing_returns_rounding_remainder_and_does_not_touch_promis() {
    use outbe_promislimit::PromisLimitContract;

    with_storage(|s| {
        let auction_ts = outbe_primitives::time::date_key_to_utc_timestamp(SERIES_ID);
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), SERIES_ID, true).unwrap();

        // Supply = 3 whole PROMIS_LOAD_MINOR units + 7 dust; only whole units can
        // be auctioned, so the dust is the remainder the caller must keep.
        let supply = U256::from(3u128 * PROMIS_LOAD_MINOR + 7);
        let remainder = crate::api::dispatch_stage_clearing(s.clone(), auction_ts, supply).unwrap();

        // The dispatch returns the dust to the caller instead of writing it to
        // PromisLimit, so it cannot collide with the caller's own set/add. (The
        // bid-settlement path routes unsold whole units separately; no bids here.)
        assert_eq!(remainder, U256::from(7u64));
        assert_eq!(
            PromisLimitContract::new(s.clone())
                .get_total_unallocated()
                .unwrap(),
            U256::ZERO,
            "clearing dispatch must not write the PromisLimit accumulator"
        );
    });
}

#[test]
fn dispatch_stage_clearing_failure_returns_whole_supply() {
    // No reveal: auction is still `Started`, so `begin_clearing` rejects the
    // stage. The best-effort wrapper must return the whole supply so the caller
    // routes the full budget back to PromisLimit and nothing is lost.
    with_storage(|s| {
        let auction_ts = outbe_primitives::time::date_key_to_utc_timestamp(SERIES_ID);
        runtime::start_auction(s.clone(), SERIES_ID, default_config()).unwrap();

        let supply = U256::from(5u128 * PROMIS_LOAD_MINOR);
        let remainder = crate::api::dispatch_stage_clearing(s.clone(), auction_ts, supply).unwrap();
        assert_eq!(remainder, supply);
    });
}
