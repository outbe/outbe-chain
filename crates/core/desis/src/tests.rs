#![cfg(test)]

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::constants::ORIGIN_ROUTER_ADDRESS;
use crate::runtime;
use crate::schema::{AuctionConfig, AuctionStage, BidData, DesisContract, IntexCallTrigger};

const CHAIN_ID: u64 = 1;
const WORLDWIDE_DAY: u32 = 20260101;
const AUCTION_TS: u64 = 1_767_261_600; // 2026-01-01 10:00 UTC
const PROMIS_LOAD_MINOR: u128 = 1_000_000_000_000_000_000; // 1e18
/// The single default target chain the auction fans in from (matches `src_chain_id` in the calls).
const SRC_CHAIN: u32 = 1;
/// Block timestamp threaded into `begin_clearing` for the fan-in deadline.
const NOW: u64 = 1_700_000_000;

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

fn default_config() -> AuctionConfig {
    AuctionConfig {
        issuance_currency: 840,
        reference_currency: 840,
        promis_load_minor: PROMIS_LOAD_MINOR,
        call_trigger: IntexCallTrigger::default(),
        min_intex_bid_rate: 100,
        min_intex_bid_quantity: 0,
        commit_bond_minor: 0, // populated at start_auction from the genesis IntexParams profile
        entry_price_minor: U256::from(10_000_000_000_000u128), // 1e13, reference ccy (feeds floor/call)
    }
}

// --- State machine ---

#[test]
fn start_auction_sets_started_stage() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Started
        );
        // Persisted config carries the commit bond folded in from the genesis profile.
        let cfg = contract.read_auction_config(WORLDWIDE_DAY).unwrap();
        let iparams = outbe_intexfactory::read_params(&s).unwrap();
        assert!(iparams.commit_bond_minor > 0, "profile carries a bond");
        assert_eq!(cfg.commit_bond_minor, iparams.commit_bond_minor);
    });
}

#[test]
fn start_auction_duplicate_fails() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        assert!(
            runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).is_err()
        );
    });
}

#[test]
fn start_auction_records_the_auction_timestamp() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.auction_at.read(&WORLDWIDE_DAY).unwrap(),
            AUCTION_TS as u32
        );
    });
}

#[test]
fn auction_noon_is_derived_from_the_timestamp_not_the_id() {
    let noon = runtime::auction_noon(AUCTION_TS).unwrap();
    assert_eq!(u64::from(noon), 1_767_225_600 + 12 * 3600);
    assert_eq!(
        runtime::auction_noon(1_767_225_600 + 23 * 3600).unwrap(),
        noon
    );
}

#[test]
fn reveal_green_day_transitions_to_revealing() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Revealing
        );
    });
}

#[test]
fn reveal_red_day_transitions_to_cancelled() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, false).unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::Cancelled
        );
    });
}

#[test]
fn begin_clearing_stores_pending() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        let supply_promis = 10 * PROMIS_LOAD_MINOR;
        let remainder =
            runtime::begin_clearing(s.clone(), WORLDWIDE_DAY, supply_promis, NOW).unwrap();
        assert_eq!(remainder, 0); // no rounding with exact multiple
        let contract = s.contract::<DesisContract>();
        let supply = contract.pending_supply_intex.read(&WORLDWIDE_DAY).unwrap();
        assert_eq!(supply, 10);
        // The fan-in gate is armed for the day.
        assert_eq!(
            contract.clearing_deadline.read(&WORLDWIDE_DAY).unwrap(),
            NOW + crate::constants::BIDS_FANIN_TIMEOUT_SECS
        );
        assert_eq!(contract.gate_active_count.read().unwrap(), 1);
    });
}

#[test]
fn start_auction_derives_min_bid_qty_from_prior_clearing() {
    with_storage(|s| {
        // First auction: start, reveal, clear with 100 issued (supply = 100).
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        runtime::begin_clearing(s.clone(), WORLDWIDE_DAY, 100 * PROMIS_LOAD_MINOR, NOW).unwrap();
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
                })
                .collect(),
        )
        .unwrap();
        mark_done(&s, SRC_CHAIN, 1, 1, 100);
        clear(&s);

        // Second auction for a different worldwide_day: min_bid_qty must be 4% of 100 = 4.
        runtime::start_auction(
            s.clone(),
            WORLDWIDE_DAY + 1,
            AUCTION_TS + 86_400,
            default_config(),
        )
        .unwrap();
        let contract = s.contract::<DesisContract>();
        let min_qty = contract
            .config_min_bid_quantity
            .read(&(WORLDWIDE_DAY + 1))
            .unwrap();
        assert_eq!(min_qty, 4);
    });
}

#[test]
fn process_bids_in_non_revealing_stage_fails() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
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

// --- Origin gate (OriginRouter-only entries) ---

#[test]
fn process_bids_rejects_non_origin_caller() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
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
fn process_bids_accumulate_across_batches() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();

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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();

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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();

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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        runtime::begin_clearing(s.clone(), WORLDWIDE_DAY, 10 * PROMIS_LOAD_MINOR, NOW).unwrap();
        // Lysis recorded creator rewards for the day before the auction concluded.
        outbe_intex::api::record_contributors(
            &s,
            WORLDWIDE_DAY,
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
            outbe_intex::api::contributor_count(&s, WORLDWIDE_DAY).unwrap(),
            0
        );
    });
}

// --- Clearing algorithm ---

#[test]
fn clearing_allocates_up_to_supply() {
    with_storage(|s| {
        let supply = 3u32;
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        runtime::begin_clearing(
            s.clone(),
            WORLDWIDE_DAY,
            supply as u128 * PROMIS_LOAD_MINOR,
            NOW,
        )
        .unwrap();
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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        runtime::begin_clearing(s.clone(), WORLDWIDE_DAY, PROMIS_LOAD_MINOR, NOW).unwrap();
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
fn begin_clearing_accepts_zero_supply() {
    with_storage(|s| {
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        let remainder = runtime::begin_clearing(s.clone(), WORLDWIDE_DAY, 0, NOW).unwrap();
        assert_eq!(remainder, 0);
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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        runtime::begin_clearing(s.clone(), WORLDWIDE_DAY, 0, NOW).unwrap();
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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        let supply = 2u32;
        runtime::begin_clearing(
            s.clone(),
            WORLDWIDE_DAY,
            supply as u128 * PROMIS_LOAD_MINOR,
            NOW,
        )
        .unwrap();
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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap(); // min_bid_price=100
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        runtime::begin_clearing(s.clone(), WORLDWIDE_DAY, 3 * PROMIS_LOAD_MINOR, NOW).unwrap();
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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        let supply = 1u32;
        runtime::begin_clearing(s.clone(), WORLDWIDE_DAY, PROMIS_LOAD_MINOR, NOW).unwrap();
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
            commit_bond_minor: 0,
            entry_price_minor: U256::from(20_000_000_000_000u128), // 2e13 (feeds floor/call; escrow basis = promis_load)
        };
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, cfg).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        let supply = 2u32;
        runtime::begin_clearing(
            s.clone(),
            WORLDWIDE_DAY,
            supply as u128 * PROMIS_LOAD_MINOR,
            NOW,
        )
        .unwrap();
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

// --- Multi-chain fan-in gate ---

/// Auction clearing over two target chains: bids merge into one clearing, and each
/// winner/bidder is tagged with its source chain for per-chain result/refund routing.
#[test]
fn two_chain_bids_merge_and_carry_source_chain() {
    let chain_a = 10u32;
    let chain_b = 20u32;
    with_targets(&[chain_a, chain_b], |s| {
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        runtime::begin_clearing(s.clone(), WORLDWIDE_DAY, 3 * PROMIS_LOAD_MINOR, NOW).unwrap();

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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        runtime::begin_clearing(s.clone(), WORLDWIDE_DAY, 2 * PROMIS_LOAD_MINOR, NOW).unwrap();
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
            AuctionStage::Revealing
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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        runtime::begin_clearing(s.clone(), WORLDWIDE_DAY, 2 * PROMIS_LOAD_MINOR, NOW).unwrap();
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
        let deadline = NOW + crate::constants::BIDS_FANIN_TIMEOUT_SECS;
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
                .map(|ev| ev.worldwideDay == WORLDWIDE_DAY && ev.srcChainId == chain_b)
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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();
        runtime::begin_clearing(s.clone(), WORLDWIDE_DAY, PROMIS_LOAD_MINOR, NOW).unwrap();
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
            WORLDWIDE_DAY,
            AUCTION_TS,
            U256::from(ENTRY_PRICE),
        )
        .unwrap();
        assert!(accepted, "valid start should be accepted");
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
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
    storage.stub_sub_call_at(ORIGIN_ROUTER_ADDRESS, targets_stub(&[SRC_CHAIN]));
    storage.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );

    // Second start hits the duplicate guard, so the best-effort wrapper swallows the
    // error, returns false, and emits AuctionDispatchFailed instead of propagating.
    let (first, second) = StorageHandle::enter(&mut storage, |s| {
        let first = crate::api::dispatch_stage_start(
            s.clone(),
            WORLDWIDE_DAY,
            AUCTION_TS,
            U256::from(ENTRY_PRICE),
        )
        .unwrap();
        let second = crate::api::dispatch_stage_start(
            s.clone(),
            WORLDWIDE_DAY,
            AUCTION_TS,
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
                .map(|ev| ev.worldwideDay == WORLDWIDE_DAY && ev.stage == "auction_stage_start")
                .unwrap_or(false)
    });
    assert!(
        found,
        "expected AuctionDispatchFailed event on DESIS_ADDRESS"
    );
}

#[test]
fn dispatch_stage_start_router_failure_leaves_no_state() {
    use crate::precompile::IDesis;
    use alloy_sol_types::SolEvent;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(1_700_000_000u64));
    storage.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );
    let accepted = StorageHandle::enter(&mut storage, |s| {
        let accepted = crate::api::dispatch_stage_start(
            s.clone(),
            WORLDWIDE_DAY,
            AUCTION_TS,
            U256::from(ENTRY_PRICE),
        )
        .unwrap();
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.read_stage(WORLDWIDE_DAY).unwrap(),
            AuctionStage::None,
            "stage write must be rolled back"
        );
        assert_eq!(
            contract.auction_at.read(&WORLDWIDE_DAY).unwrap(),
            0,
            "auction timestamp write must be rolled back"
        );
        accepted
    });
    assert!(!accepted, "router failure must surface as false");

    let desis_addr = outbe_primitives::addresses::DESIS_ADDRESS;
    let fail_sig = IDesis::AuctionDispatchFailed::SIGNATURE_HASH;
    let found = storage
        .get_events(desis_addr)
        .iter()
        .any(|log| log.topics().first() == Some(&fail_sig));
    assert!(found, "failure event must survive the rollback");
}

#[test]
fn dispatch_stage_clearing_router_failure_reverts_pending_supply() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(1_700_000_000u64));
    storage.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );
    let supply_promis = U256::from(10 * PROMIS_LOAD_MINOR);
    let returned = StorageHandle::enter(&mut storage, |s| {
        let contract = s.contract::<DesisContract>();
        contract
            .write_auction_config(WORLDWIDE_DAY, &default_config())
            .unwrap();
        contract
            .write_stage(WORLDWIDE_DAY, AuctionStage::Revealing)
            .unwrap();

        let returned =
            crate::api::dispatch_stage_clearing(s.clone(), WORLDWIDE_DAY, supply_promis, NOW)
                .unwrap();

        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract.pending_supply_intex.read(&WORLDWIDE_DAY).unwrap(),
            0,
            "pending supply must be rolled back"
        );
        assert_eq!(
            contract.clearing_initiated.read(&WORLDWIDE_DAY).unwrap(),
            0,
            "initiated flag must be rolled back"
        );
        returned
    });
    assert_eq!(
        returned, supply_promis,
        "the whole budget returns to the caller"
    );
}

#[test]
fn dispatch_stage_clearing_returns_rounding_remainder_and_does_not_touch_promis() {
    use outbe_promislimit::PromisLimitContract;

    with_storage(|s| {
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();
        runtime::reveal_auction(s.clone(), WORLDWIDE_DAY, true).unwrap();

        // Supply = 3 whole PROMIS_LOAD_MINOR units + 7 dust; only whole units can
        // be auctioned, so the dust is the remainder the caller must keep.
        let supply = U256::from(3u128 * PROMIS_LOAD_MINOR + 7);
        let remainder =
            crate::api::dispatch_stage_clearing(s.clone(), WORLDWIDE_DAY, supply, NOW).unwrap();

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
        runtime::start_auction(s.clone(), WORLDWIDE_DAY, AUCTION_TS, default_config()).unwrap();

        let supply = U256::from(5u128 * PROMIS_LOAD_MINOR);
        let remainder =
            crate::api::dispatch_stage_clearing(s.clone(), WORLDWIDE_DAY, supply, NOW).unwrap();
        assert_eq!(remainder, supply);
    });
}
