//! Steps for the intex engine deployed onto the committee's own chain.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::Address;
#[cfg(feature = "ocomp-integration")]
use alloy_primitives::U256;
use alloy_sol_types::sol;
use cucumber::{then, when};

use crate::env::environment;
use crate::internal::eth;
#[cfg(feature = "ocomp-integration")]
use crate::world::bidders;
use crate::world::{origin_venue, World};

#[when("the intex engine is deployed on the committee chain")]
fn deploy_origin_venue(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");
    // Genesis funds validators and Tribute owners, so the deploy account starts
    // empty on this chain and cannot even pay for its own scripts.
    // One owner's balance does not cover the whole deploy, so several chip in.
    // Taken from the tail: the scenario's Tribute submitters are the head of the
    // same list, and they still need their own gas.
    let funders: Vec<String> = world
        .state
        .ocomp_capacity_tribute_private_keys
        .iter()
        .rev()
        .take(6)
        .cloned()
        .collect();
    assert!(
        !funders.is_empty(),
        "capacity fixture funded no Tribute owners"
    );
    for funder in &funders {
        crate::internal::eth::send_value(
            &url,
            origin_venue::deployer_address(),
            funder,
            crate::internal::eth::coen(900),
        )
        .expect("fund the deploy account on the committee chain");
    }
    let contracts = origin_venue::deploy(&environment().repo, &url, chain_id)
        .expect("deploy the intex engine on the committee chain");
    world.state.origin_contracts = Some(contracts);
}

#[then("the committee chain hosts the intex engine")]
fn committee_chain_hosts_engine(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");

    for (name, address) in [
        ("ERC7786Bridge", contracts.bridge),
        ("LoopbackGatewayAdapter", contracts.loopback),
        ("OriginRouter", contracts.origin_router),
        ("IntexNFT1155", contracts.intex_nft),
        ("TargetRouter", contracts.target_router),
    ] {
        assert!(
            eth::code(&url, address).is_some_and(|code| !code.is_empty()),
            "{name} reported at {address} but the committee chain holds no code there"
        );
    }
}

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IProceedsRoute {
        function tokenBridge() external view returns (address);
        function wcoen() external view returns (address);
    }
}

#[then("the origin router knows where proceeds come from")]
fn origin_router_knows_proceeds_route(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");

    let bridge = eth::read_call(
        &url,
        contracts.origin_router,
        &IProceedsRoute::tokenBridgeCall {},
    )
    .expect("read tokenBridge from the origin router");
    let token = eth::read_call(&url, contracts.origin_router, &IProceedsRoute::wcoenCall {})
        .expect("read wcoen from the origin router");
    assert!(
        !bridge.is_zero() && token == contracts.wcoen,
        "proceeds route is unset: bridge {bridge}, token {token}"
    );
}

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IAuctionStage {
        function getAuctionStage(uint32 worldwideDay) external view returns (uint8);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IOriginTargets {
        function targetsOf(uint32 worldwideDay) external view returns (uint32[] memory);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IVenueSchedule {
        struct IntexCallTrigger { uint32 callWindow; uint32 callThreshold; uint32 callNoticePeriod; }
        struct AuctionSchedule { uint32 commitEnd; uint32 revealEnd; uint32 issuanceEnd; }
        struct AuctionParams {
            uint16 issuanceCurrency; uint16 referenceCurrency; uint128 promisLoadMinor;
            IntexCallTrigger callTrigger; uint32 minIntexBidRate; uint16 minIntexBidQuantity;
            uint64 entryPriceMinor; uint64 floorPriceMinor; uint64 callPriceMinor; uint128 commitBondMinor;
        }
        struct AuctionResult {
            uint64 auctionClearingRate; uint32 wonBidsCount; uint32 issuedIntexCount; uint128 issuedIntexLoadedPromis;
        }
        function auctions(uint32 worldwideDay)
            external view
            returns (uint8 worldwideDayState, AuctionSchedule memory schedule, AuctionParams memory params, AuctionResult memory result);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IParkedWork {
        struct ParkedSend { uint32 dstChainId; uint64 gasLimit; bool sent; bytes payload; }
        function nextPendingBidsRelayIdx() external view returns (uint256);
        function flushPendingBidsRelay(uint256 idx) external;
        function parkedSend(uint256 idx) external view returns (ParkedSend memory);
        function flushPendingSend(uint256 idx) external;
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IVenueCounts {
        function auctionRunningCounts(uint32 worldwideDay)
            external view returns (uint32 committedBidsCount, uint32 revealedBidsCount);
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IDesisBids {
        function getBidsCount(uint32 worldwideDay) external view returns (uint256);
        function isChainDone(uint32 worldwideDay, uint32 srcChainId) external view returns (bool);
    }
}

fn frozen_targets(url: &str, router: Address, worldwide_day: u32) -> String {
    match eth::read_call(
        url,
        router,
        &IOriginTargets::targetsOfCall {
            worldwideDay: worldwide_day,
        },
    ) {
        Some(targets) if targets.is_empty() => "the router froze no targets for the day".to_owned(),
        Some(targets) => format!("the router froze targets {targets:?}"),
        None => "the router did not answer".to_owned(),
    }
}

fn desis_stage(url: &str, worldwide_day: u32) -> String {
    let desis = origin_venue::DESIS
        .parse()
        .expect("desis precompile address");
    match eth::read_call(
        url,
        desis,
        &IAuctionStage::getAuctionStageCall {
            worldwideDay: worldwide_day,
        },
    ) {
        Some(0) => "no brief ever reached Desis".to_owned(),
        Some(1) => "Desis holds the brief but never started the auction".to_owned(),
        Some(2) => "Desis started it, so the message never reached the venue".to_owned(),
        Some(6) => "Desis cancelled the day".to_owned(),
        Some(other) => format!("Desis stage {other}"),
        None => "Desis did not answer".to_owned(),
    }
}

#[cfg(feature = "ocomp-integration")]
fn day_colour(world: &World, worldwide_day: u32) -> String {
    match world
        .rpc
        .metadosis_wwd_state_on(world.validators.primary_port(), worldwide_day)
    {
        Some(state) => format!("day type {} in status {}", state.day_type, state.status),
        None => "day state unreadable".to_owned(),
    }
}

#[cfg(not(feature = "ocomp-integration"))]
fn day_colour(_world: &World, _worldwide_day: u32) -> String {
    "day type unreadable without ocomp-integration".to_owned()
}

#[cfg(feature = "ocomp-integration")]
fn day_closure(world: &World, worldwide_day: u32) -> String {
    match world
        .rpc
        .metadosis_terminal_receipt_on(world.validators.primary_port(), worldwide_day)
    {
        // An absent receipt reads back zeroed rather than reverting, so the
        // block number is what tells a real closure from no closure at all.
        Some(receipt) if receipt.block_number > 0 => format!(
            "Metadosis closed the day at block {} with outcome {}",
            receipt.block_number, receipt.outcome
        ),
        _ => "Metadosis never closed the day, so nothing ever briefed Desis".to_owned(),
    }
}

#[cfg(not(feature = "ocomp-integration"))]
fn day_closure(_world: &World, _worldwide_day: u32) -> String {
    "day closure unreadable without ocomp-integration".to_owned()
}

/// An e2e build runs the auction on minute-long windows, so the run waits the
/// stages out rather than moving the clock across a day it never formed.
#[cfg(feature = "ocomp-integration")]
const AUCTION_STAGE_TIMEOUT: Duration = Duration::from_secs(2400);

#[cfg(feature = "ocomp-integration")]
fn advance_past_window_to_stage(world: &mut World, target_stage: u8) {
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let worldwide_day = settled_day(world);
    let url = world.rpc.url(world.validators.primary_port());
    let deadline = Instant::now() + AUCTION_STAGE_TIMEOUT;
    loop {
        let stage = eth::read_call(
            &url,
            contracts.intex_auction,
            &IAuctionStage::getAuctionStageCall {
                worldwideDay: worldwide_day,
            },
        )
        .expect("the venue knows the auction");
        assert_ne!(stage, 4, "day {worldwide_day} was cancelled on the venue");
        if stage >= target_stage {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "day {worldwide_day} stalled at venue stage {stage} short of {target_stage}"
        );
        sleep(Duration::from_secs(2));
    }
}

const AUCTION_START_TIMEOUT: Duration = Duration::from_secs(1200);

#[then("the auction for that day opens on the target chain")]
fn auction_opens_on_target(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let worldwide_day = world
        .state
        .wwd
        .as_deref()
        .expect("the settled day's WorldwideDay")
        .parse::<u32>()
        .expect("numeric WorldwideDay");

    // The venue reverts with AuctionNotFound until the start message lands, so a
    // reply at all is the proof that the whole origin-to-target path ran.
    let deadline = Instant::now() + AUCTION_START_TIMEOUT;
    loop {
        if let Some(stage) = eth::read_call(
            &url,
            contracts.intex_auction,
            &IAuctionStage::getAuctionStageCall {
                worldwideDay: worldwide_day,
            },
        ) {
            assert!(
                stage < 2,
                "day {worldwide_day} reached the venue at stage {stage}: {}",
                venue_schedule(&url, contracts.intex_auction, worldwide_day)
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "day {worldwide_day} settled but no auction ever opened on the venue at {}: {}; {}; {}; {}",
            contracts.intex_auction,
            desis_stage(&url, worldwide_day),
            frozen_targets(&url, contracts.origin_router, worldwide_day),
            day_closure(world, worldwide_day),
            day_colour(world, worldwide_day)
        );
        sleep(Duration::from_secs(2));
    }
}

fn venue_schedule(url: &str, venue: Address, worldwide_day: u32) -> String {
    let now = eth::latest_block_timestamp(url).unwrap_or_default();
    match eth::read_call(
        url,
        venue,
        &IVenueSchedule::auctionsCall {
            worldwideDay: worldwide_day,
        },
    ) {
        Some(a) => format!(
            "commitEnd {} revealEnd {} issuanceEnd {} against block time {now}",
            a.schedule.commitEnd, a.schedule.revealEnd, a.schedule.issuanceEnd
        ),
        None => "venue did not report the schedule".to_owned(),
    }
}

/// Both sides swallow a failed send by parking it, so the parked entry is the
/// only trace of work that never left.
#[cfg(feature = "ocomp-integration")]
fn parked_work(url: &str, router: Address, venue_router: Address) -> String {
    let relays = eth::read_call(
        url,
        venue_router,
        &IParkedWork::nextPendingBidsRelayIdxCall {},
    );
    let parked = eth::read_call(
        url,
        router,
        &IParkedWork::parkedSendCall { idx: U256::ZERO },
    );
    let parked_note = match parked {
        Some(send) if !send.payload.is_empty() => {
            // Retrying a parked send is permissionless, and its revert carries
            // the reason the router swallowed when it parked.
            let flush = eth::send_call(
                url,
                router,
                crate::world::forge::DEPLOYER_KEY,
                &IParkedWork::flushPendingSendCall { idx: U256::ZERO },
                None,
            );
            format!(
                "origin parked a send to chain {} ({} bytes, gas {}), retry says {:?}",
                send.dstChainId,
                send.payload.len(),
                send.gasLimit,
                flush.err().map(|error| error.to_string())
            )
        }
        Some(_) => "origin parked nothing".to_owned(),
        None => "origin did not report parked sends".to_owned(),
    };
    let relay_note = match relays {
        Some(count) if count > U256::ZERO => {
            // Retrying a parked relay is permissionless, and its revert carries
            // the reason the venue swallowed when it parked.
            let flush = eth::send_call(
                url,
                venue_router,
                crate::world::forge::DEPLOYER_KEY,
                &IParkedWork::flushPendingBidsRelayCall { idx: U256::ZERO },
                None,
            );
            format!(
                "venue parked {count} bid relays, retry says {:?}",
                flush.err().map(|error| error.to_string())
            )
        }
        _ => format!("venue parked bid relays {relays:?}"),
    };
    format!("{parked_note}, {relay_note}")
}

/// The venue emits this at the end of every inbound stage handler, so its
/// absence separates a message that never arrived from one that did nothing.
#[cfg(feature = "ocomp-integration")]
fn stages_received(url: &str, venue_router: Address, worldwide_day: u32) -> String {
    let topic0 =
        alloy_primitives::keccak256(b"AuctionStageReceived(uint32,uint32,uint8)".as_slice());
    let day_topic = format!("0x{:064x}", worldwide_day);
    let logs = eth::raw_json_with_params(
        url,
        "eth_getLogs",
        serde_json::json!([{
            "fromBlock": "0x0",
            "toBlock": "latest",
            "address": format!("{venue_router:?}"),
            "topics": [format!("{topic0:?}"), serde_json::Value::Null, day_topic],
        }]),
    );
    match logs.as_ref().and_then(|value| value.as_array()) {
        Some(entries) if entries.is_empty() => "the venue received no inbound stage".to_owned(),
        Some(entries) => format!("the venue received {} inbound stages", entries.len()),
        None => "the venue stage log is unreadable".to_owned(),
    }
}

#[cfg(feature = "ocomp-integration")]
fn venue_bid_counts(url: &str, venue: Address, worldwide_day: u32) -> String {
    match eth::read_call(
        url,
        venue,
        &IVenueCounts::auctionRunningCountsCall {
            worldwideDay: worldwide_day,
        },
    ) {
        Some(counts) => format!(
            "venue holds {} committed and {} revealed bids",
            counts.committedBidsCount, counts.revealedBidsCount
        ),
        None => "venue did not report its bid counts".to_owned(),
    }
}

#[cfg(feature = "ocomp-integration")]
fn relayed_bids(url: &str, worldwide_day: u32, chain_id: u32) -> String {
    let desis = origin_venue::DESIS
        .parse()
        .expect("desis precompile address");
    let count = eth::read_call(
        url,
        desis,
        &IDesisBids::getBidsCountCall {
            worldwideDay: worldwide_day,
        },
    );
    let done = eth::read_call(
        url,
        desis,
        &IDesisBids::isChainDoneCall {
            worldwideDay: worldwide_day,
            srcChainId: chain_id,
        },
    );
    match (count, done) {
        (Some(count), Some(done)) => {
            format!("Desis holds {count} relayed bids, chain {chain_id} done={done}")
        }
        _ => "Desis did not report its relayed bids".to_owned(),
    }
}

/// After a logical-time jump the ratchet lets each block carry up to an hour, so
/// a minute-long auction window would burn out in seconds. Wait for blocks that
/// carry ordinary time: a chain that has not resumed yet looks identical to a
/// settled one if only the clock is sampled.
#[cfg(feature = "ocomp-integration")]
/// Desis starts a day's auction from the `auction_advance` cycle trigger, which
/// has a twelve-hour slot. A day briefed just after one slot waits for the next,
/// so the scenario walks the clock there rather than expecting an instant start.
#[when("the committee clock reaches the next auction slot")]
fn committee_clock_reaches_next_auction_slot(world: &mut World) {
    const AUCTION_SLOT_SECS: u64 = 43_200;
    let port = world.validators.primary_port();
    let now = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp");
    let next_slot = now / AUCTION_SLOT_SECS * AUCTION_SLOT_SECS + AUCTION_SLOT_SECS;
    let _ = crate::features::ocomp::restart_committee_at_logical_time(world, next_slot + 60);
}

#[when("the committee clock settles after the jump")]
fn committee_clock_settles(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let deadline = Instant::now() + AUCTION_STAGE_TIMEOUT;
    let mut calm_samples = 0;
    let mut previous: Option<(u64, u64)> = None;
    loop {
        sleep(Duration::from_secs(6));
        let height = eth::block_number(&url);
        let stamp = eth::latest_block_timestamp(&url);
        if let (Some(height), Some(stamp), Some((last_height, last_stamp))) =
            (height, stamp, previous)
        {
            let blocks = height.saturating_sub(last_height);
            let seconds = stamp.saturating_sub(last_stamp);
            if blocks > 0 && seconds / blocks < 120 {
                calm_samples += 1;
                if calm_samples >= 2 {
                    return;
                }
            } else {
                calm_samples = 0;
            }
        }
        previous = height.zip(stamp);
        assert!(
            Instant::now() < deadline,
            "the committee clock never settled after the jump"
        );
    }
}

/// A loopback venue cannot relay its bids from inside the clearing delivery —
/// that would be a nested send in the same transaction — so it parks the relay
/// for a permissionless retry. Production has a keeper for this; the run does it
/// itself.
#[cfg(feature = "ocomp-integration")]
fn flush_parked_bid_relays(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let venue_router = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses")
        .target_router;
    let parked = eth::read_call(
        &url,
        venue_router,
        &IParkedWork::nextPendingBidsRelayIdxCall {},
    )
    .unwrap_or_default();
    let mut idx = U256::ZERO;
    while idx < parked {
        eth::send_call(
            &url,
            venue_router,
            crate::world::forge::DEPLOYER_KEY,
            &IParkedWork::flushPendingBidsRelayCall { idx },
            None,
        )
        .ok();
        idx += U256::from(1);
    }
}

/// The loopback adapter delivers inside the send and swallows a failure: it
/// parks the delivery with the revert reason and still reports the send as
/// done. That reason is the only place a refused message explains itself.
#[cfg(feature = "ocomp-integration")]
fn parked_deliveries(world: &World) -> String {
    let url = world.rpc.url(world.validators.primary_port());
    let Some(contracts) = world.state.origin_contracts.clone() else {
        return "no deploy recorded".to_owned();
    };
    let topic0 = alloy_primitives::keccak256(b"DeliveryParked(uint256,bytes)".as_slice());
    let logs = eth::raw_json_with_params(
        &url,
        "eth_getLogs",
        serde_json::json!([{
            "fromBlock": "0x0",
            "toBlock": "latest",
            "address": format!("{:?}", contracts.loopback),
            "topics": [format!("{topic0:?}")],
        }]),
    );
    match logs.as_ref().and_then(|value| value.as_array()) {
        Some(entries) if entries.is_empty() => "no delivery was parked".to_owned(),
        Some(entries) => {
            let reasons: Vec<String> = entries
                .iter()
                .filter_map(|entry| entry.get("data")?.as_str())
                .map(|data| {
                    let bytes = alloy_primitives::hex::decode(data.trim_start_matches("0x"))
                        .unwrap_or_default();
                    let text: String = bytes
                        .iter()
                        .map(|b| {
                            if b.is_ascii_graphic() || *b == b' ' {
                                *b as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    format!("{data} ({text})")
                })
                .collect();
            format!(
                "{} parked deliveries: {}",
                entries.len(),
                reasons.join(" | ")
            )
        }
        None => "the adapter log is unreadable".to_owned(),
    }
}

/// Did Desis get as far as handing the refunds to the router? Separates "the
/// clearing never sent them" from "the target chain refused them".
#[cfg(feature = "ocomp-integration")]
fn refunds_were_sent(world: &World, worldwide_day: u32) -> bool {
    let url = world.rpc.url(world.validators.primary_port());
    let Some(contracts) = world.state.origin_contracts.clone() else {
        return false;
    };
    let topic0 =
        alloy_primitives::keccak256(b"RefundInstructionsSent(bytes32,uint32,uint256)".as_slice());
    eth::raw_json_with_params(
        &url,
        "eth_getLogs",
        serde_json::json!([{
            "fromBlock": "0x0",
            "toBlock": "latest",
            "address": format!("{:?}", contracts.origin_router),
            "topics": [
                format!("{topic0:?}"),
                serde_json::Value::Null,
                format!("0x{worldwide_day:064x}"),
            ],
        }]),
    )
    .as_ref()
    .and_then(|value| value.as_array())
    .is_some_and(|entries| !entries.is_empty())
}

/// Clearing fires the auction result, the issuance instructions and the refunds
/// back to back. A router that cannot pay the bridge fee parks the tail of that
/// burst instead of reverting, so a parked send is the first thing to look at
/// when a message never lands. Counting only: a run must not need a nudge.
#[cfg(feature = "ocomp-integration")]
fn parked_origin_sends(world: &World) -> u32 {
    let url = world.rpc.url(world.validators.primary_port());
    let Some(contracts) = world.state.origin_contracts.clone() else {
        return 0;
    };
    let mut parked = 0;
    for idx in 0..64u64 {
        let Some(send) = eth::read_call(
            &url,
            contracts.origin_router,
            &IParkedWork::parkedSendCall {
                idx: U256::from(idx),
            },
        ) else {
            break;
        };
        if send.dstChainId == 0 {
            break;
        }
        if !send.sent {
            parked += 1;
        }
    }
    parked
}

#[cfg(feature = "ocomp-integration")]
fn settled_day(world: &World) -> u32 {
    world
        .state
        .wwd
        .as_deref()
        .expect("the settled day's WorldwideDay")
        .parse::<u32>()
        .expect("numeric WorldwideDay")
}

#[cfg(feature = "ocomp-integration")]
const BIDDER_ALLOWANCE: u128 = 1_000_000_000 * 1_000_000_000_000_000_000;

#[cfg(feature = "ocomp-integration")]
const BIDS: [(u16, u32); 2] = [(30, 800_000), (40, 700_000)];

#[cfg(feature = "ocomp-integration")]
#[when("two bidders commit their bids")]
fn bidders_commit(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let worldwide_day = settled_day(world);

    let bidders = bidders::derive(&BIDS).expect("derive the bidders");
    bidders::fund(
        &url,
        contracts.payment_token,
        contracts.escrow,
        &bidders,
        U256::from(BIDDER_ALLOWANCE),
    )
    .expect("fund the bidders");
    bidders::commit(
        &url,
        contracts.intex_auction,
        chain_id,
        worldwide_day,
        &bidders,
    )
    .expect("commit the bids");
    world.state.auction_bidders = bidders;
}

#[cfg(feature = "ocomp-integration")]
#[when("those bidders reveal their bids once the venue is revealing")]
fn bidders_reveal(world: &mut World) {
    advance_past_window_to_stage(world, 1);

    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let worldwide_day = settled_day(world);
    let bidders = world.state.auction_bidders.clone();

    bidders::reveal(
        &url,
        contracts.intex_auction,
        chain_id,
        worldwide_day,
        &bidders,
    )
    .expect("reveal the bids");
}

#[cfg(feature = "ocomp-integration")]
#[then("the auction clears and the venue moves past its reveal window")]
fn auction_clears(world: &mut World) {
    advance_past_window_to_stage(world, 2);

    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .expect("committee chain id");
    let deployed = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let venue = deployed.intex_auction;
    let router = deployed.origin_router;
    let venue_router = deployed.target_router;
    let worldwide_day = settled_day(world);
    let deadline = Instant::now() + AUCTION_START_TIMEOUT;
    loop {
        let url = world.rpc.url(world.validators.primary_port());
        let desis = origin_venue::DESIS
            .parse()
            .expect("desis precompile address");
        let stage = eth::read_call(
            &url,
            desis,
            &IAuctionStage::getAuctionStageCall {
                worldwideDay: worldwide_day,
            },
        );
        if stage == Some(5) {
            return;
        }
        flush_parked_bid_relays(world);
        assert_ne!(stage, Some(6), "Desis cancelled day {worldwide_day}");
        assert!(
            Instant::now() < deadline,
            "day {worldwide_day} never cleared on Desis: {}; {}; venue stage {:?}, {}; {}; {}",
            desis_stage(&url, worldwide_day),
            relayed_bids(&url, worldwide_day, chain_id as u32),
            eth::read_call(
                &url,
                venue,
                &IAuctionStage::getAuctionStageCall {
                    worldwideDay: worldwide_day
                },
            ),
            venue_bid_counts(&url, venue, worldwide_day),
            parked_work(&url, router, venue_router),
            stages_received(&url, venue_router, worldwide_day)
        );
        sleep(Duration::from_secs(2));
    }
}

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IIssuedSeries {
        function seriesExists(bytes14 seriesId) external view returns (bool);
        function issuedTokenId(bytes14 seriesId) external pure returns (uint256);
        function balanceOf(address account, uint256 id) external view returns (uint256);
    }
}

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IPaymentToken {
        function balanceOf(address account) external view returns (uint256);
    }
}

/// The series the target chain was told to issue, read back from its own log so
/// the identifier comes from the chain rather than from a rebuilt guess.
#[cfg(feature = "ocomp-integration")]
fn issued_series(url: &str, venue_router: Address) -> Option<alloy_primitives::FixedBytes<14>> {
    let topic0 = alloy_primitives::keccak256(
        b"IssuanceInstructionsReceived(uint32,bytes14,uint256)".as_slice(),
    );
    let logs = eth::raw_json_with_params(
        url,
        "eth_getLogs",
        serde_json::json!([{
            "fromBlock": "0x0",
            "toBlock": "latest",
            "address": format!("{venue_router:?}"),
            "topics": [format!("{topic0:?}")],
        }]),
    )?;
    let topic = logs
        .as_array()?
        .first()?
        .get("topics")?
        .as_array()?
        .get(2)?
        .as_str()?;
    let word: alloy_primitives::B256 = topic.parse().ok()?;
    Some(alloy_primitives::FixedBytes::<14>::from_slice(
        &word.0[..14],
    ))
}

#[cfg(feature = "ocomp-integration")]
fn deferred_mints(url: &str, venue_router: Address) -> usize {
    let topic0 = alloy_primitives::keccak256(
        b"IssuanceMintDeferred(uint256,bytes14,address,bytes)".as_slice(),
    );
    eth::raw_json_with_params(
        url,
        "eth_getLogs",
        serde_json::json!([{
            "fromBlock": "0x0",
            "toBlock": "latest",
            "address": format!("{venue_router:?}"),
            "topics": [format!("{topic0:?}")],
        }]),
    )
    .as_ref()
    .and_then(|value| value.as_array())
    .map_or(0, Vec::len)
}

/// Whether Desis issued anything at all. Absent issuance means one of two very
/// different things, and only the clearing event tells them apart.
#[cfg(feature = "ocomp-integration")]
fn cleared_empty(url: &str, worldwide_day: u32) -> Option<bool> {
    let desis: Address = origin_venue::DESIS.parse().ok()?;
    let day_topic = format!("0x{worldwide_day:064x}");
    let seen = |signature: &[u8]| {
        let topic0 = alloy_primitives::keccak256(signature);
        eth::raw_json_with_params(
            url,
            "eth_getLogs",
            serde_json::json!([{
                "fromBlock": "0x0",
                "toBlock": "latest",
                "address": format!("{desis:?}"),
                "topics": [format!("{topic0:?}"), day_topic],
            }]),
        )
        .as_ref()
        .and_then(|value| value.as_array())
        .is_some_and(|entries| !entries.is_empty())
    };
    if seen(b"AuctionClearedEmpty(uint32,uint64)".as_slice()) {
        return Some(true);
    }
    if seen(b"AuctionCleared(uint32,uint32,uint32,uint64)".as_slice()) {
        return Some(false);
    }
    None
}

#[cfg(feature = "ocomp-integration")]
#[then("the cleared day mints the Intex on the target chain")]
fn issuance_mints_intex(world: &mut World) {
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let bidders = world.state.auction_bidders.clone();
    let url = world.rpc.url(world.validators.primary_port());
    let worldwide_day = settled_day(world);

    assert_ne!(
        cleared_empty(&url, worldwide_day),
        Some(true),
        "day {worldwide_day} cleared empty: the day's supply bought no whole Intex, so there is \
         nothing to issue and this scenario cannot cover minting"
    );

    let deadline = Instant::now() + AUCTION_STAGE_TIMEOUT;
    let series = loop {
        if let Some(series) = issued_series(&url, contracts.target_router) {
            break series;
        }
        assert!(
            Instant::now() < deadline,
            "day {worldwide_day} cleared but the target chain never received issuance instructions"
        );
        sleep(Duration::from_secs(2));
    };

    let exists = eth::read_call(
        &url,
        contracts.intex_nft,
        &IIssuedSeries::seriesExistsCall { seriesId: series },
    )
    .expect("ask the collection whether the series exists");
    assert!(exists, "series {series} was instructed but never created");

    let token_id = eth::read_call(
        &url,
        contracts.intex_nft,
        &IIssuedSeries::issuedTokenIdCall { seriesId: series },
    )
    .expect("derive the token id of the issued series");

    let minted: U256 = bidders
        .iter()
        .map(|bidder| {
            eth::read_call(
                &url,
                contracts.intex_nft,
                &IIssuedSeries::balanceOfCall {
                    account: bidder.address,
                    id: token_id,
                },
            )
            .expect("read a bidder's Intex balance")
        })
        .sum();
    assert!(
        !minted.is_zero(),
        "series {series} exists but no bidder holds any Intex ({} mints were deferred)",
        deferred_mints(&url, contracts.target_router)
    );
}

#[cfg(feature = "ocomp-integration")]
#[then("the escrow settles the day and returns what the bids did not buy")]
fn escrow_refunds_the_rest(world: &mut World) {
    let contracts = world
        .state
        .origin_contracts
        .clone()
        .expect("a deploy recorded its addresses");
    let url = world.rpc.url(world.validators.primary_port());
    let worldwide_day = settled_day(world);
    let topic0 = alloy_primitives::keccak256(
        b"RefundInstructionsReceived(uint32,uint32,uint256)".as_slice(),
    );
    let day_topic = format!("0x{worldwide_day:064x}");

    let deadline = Instant::now() + AUCTION_STAGE_TIMEOUT;
    loop {
        let received = eth::raw_json_with_params(
            &url,
            "eth_getLogs",
            serde_json::json!([{
                "fromBlock": "0x0",
                "toBlock": "latest",
                "address": format!("{:?}", contracts.target_router),
                "topics": [format!("{topic0:?}"), serde_json::Value::Null, day_topic],
            }]),
        )
        .as_ref()
        .and_then(|value| value.as_array())
        .is_some_and(|entries| !entries.is_empty());
        if received {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "day {worldwide_day} cleared but the escrow never received refund instructions \
             (sent by Desis: {}; {} origin sends are parked; {})",
            refunds_were_sent(world, worldwide_day),
            parked_origin_sends(world),
            parked_deliveries(world)
        );
        sleep(Duration::from_secs(2));
    }

    // Settlement either pays a bid or gives it back, so the day leaves nothing behind.
    let held = eth::read_call(
        &url,
        contracts.payment_token,
        &IPaymentToken::balanceOfCall {
            account: contracts.escrow,
        },
    )
    .expect("read what the escrow still holds");
    assert!(
        held.is_zero(),
        "escrow still holds {held} of the payment token after settling day {worldwide_day}"
    );
}
