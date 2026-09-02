//! Chain-state probes for the origin venue.
//!
//! Every one answers a question a failing step needs to explain itself - which
//! stage the venue is in, what the router froze, what a parked delivery said -
//! and returns a human sentence rather than a value, so a panic message reads
//! as a diagnosis instead of a mismatch.

use alloy_primitives::Address;
#[cfg(feature = "ocomp-integration")]
use alloy_primitives::U256;
use alloy_sol_types::sol;

use crate::internal::eth;
use crate::world::{origin_venue, World};

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IProceedsRoute {
        function tokenBridge() external view returns (address);
        function wcoen() external view returns (address);
    }
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
        function nextParkedIdx() external view returns (uint256);
        function retryDelivery(uint256 idx) external;
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
/// Every probe below asks the same question of the chain - "what did this
/// contract emit for this day?" - so the query shape lives here once.
#[cfg_attr(not(feature = "ocomp-integration"), allow(dead_code))]
fn logs_of(
    url: &str,
    address: Address,
    topics: Vec<serde_json::Value>,
) -> Option<Vec<serde_json::Value>> {
    eth::raw_json_with_params(
        url,
        "eth_getLogs",
        serde_json::json!([{
            "fromBlock": "0x0",
            "toBlock": "latest",
            "address": format!("{address:?}"),
            "topics": topics,
        }]),
    )
    .as_ref()
    .and_then(|value| value.as_array())
    .cloned()
}

pub(crate) fn frozen_targets(url: &str, router: Address, worldwide_day: u32) -> String {
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
pub(crate) fn desis_stage(url: &str, worldwide_day: u32) -> String {
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
pub(crate) fn day_colour(world: &World, worldwide_day: u32) -> String {
    match world
        .rpc
        .metadosis_wwd_state_on(world.validators.primary_port(), worldwide_day)
    {
        Some(state) => format!("day type {} in status {}", state.day_type, state.status),
        None => "day state unreadable".to_owned(),
    }
}
#[cfg(not(feature = "ocomp-integration"))]
pub(crate) fn day_colour(_world: &World, _worldwide_day: u32) -> String {
    "day type unreadable without ocomp-integration".to_owned()
}
#[cfg(feature = "ocomp-integration")]
pub(crate) fn day_closure(world: &World, worldwide_day: u32) -> String {
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
pub(crate) fn day_closure(_world: &World, _worldwide_day: u32) -> String {
    "day closure unreadable without ocomp-integration".to_owned()
}
pub(crate) fn venue_schedule(url: &str, venue: Address, worldwide_day: u32) -> String {
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
pub(crate) fn parked_work(
    url: &str,
    venue_url: &str,
    router: Address,
    venue_router: Address,
) -> String {
    let relays = eth::read_call(
        venue_url,
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
                venue_url,
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
/// Split the relay in two: what left the venue, and what the origin took in.
/// A gap between them is the transport losing the message, not either side
/// refusing it.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn bid_relay_traffic(
    url: &str,
    venue_url: &str,
    router: Address,
    venue_router: Address,
    worldwide_day: u32,
) -> String {
    let day_topic = format!("0x{:064x}", worldwide_day);
    let count = |at: &str, address: Address, signature: &[u8], day_topic_index: usize| -> String {
        let topic0 = alloy_primitives::keccak256(signature);
        let mut topics = vec![
            serde_json::json!(format!("{topic0:?}")),
            serde_json::Value::Null,
        ];
        if day_topic_index == 2 {
            topics.push(serde_json::json!(day_topic.clone()));
        }
        match logs_of(at, address, topics) {
            Some(entries) => entries.len().to_string(),
            None => "unreadable".to_owned(),
        }
    };
    let sent_batches = count(
        venue_url,
        venue_router,
        b"BidsBatchSent(bytes32,uint32,uint256)".as_slice(),
        2,
    );
    let sent_done = count(
        venue_url,
        venue_router,
        b"BidsDoneSent(bytes32,uint32,uint16,uint32)".as_slice(),
        2,
    );
    let got_batches = count(
        url,
        router,
        b"BidsBatchReceived(uint32,uint32,uint256)".as_slice(),
        2,
    );
    format!(
        "the venue sent {sent_batches} bid batches and {sent_done} done markers, \
         the origin took in {got_batches} batches"
    )
}
/// Desis drops inbound work of its own, with its own reason codes, and those
/// never appear in either router's ignore log.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn ignored_by_desis(url: &str, worldwide_day: u32) -> String {
    let desis: Address = origin_venue::DESIS
        .parse()
        .expect("desis precompile address");
    let topic0 = alloy_primitives::keccak256(b"InboundIgnored(uint32,uint32,uint8)".as_slice());
    let day_topic = format!("0x{:064x}", worldwide_day);
    let logs = logs_of(
        url,
        desis,
        vec![
            serde_json::json!(format!("{topic0:?}")),
            serde_json::json!(day_topic),
        ],
    );
    let entries = match logs.as_ref() {
        Some(entries) if entries.is_empty() => return "Desis ignored no inbound work".to_owned(),
        Some(entries) => entries,
        None => return "Desis ignore log is unreadable".to_owned(),
    };
    let described: Vec<String> = entries
        .iter()
        .map(|entry| {
            let reason = entry["data"]
                .as_str()
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
            match reason {
                Some(2) => "obsolete".to_owned(),
                Some(3) => "conflict".to_owned(),
                Some(4) => "not found".to_owned(),
                Some(other) => format!("reason {other}"),
                None => "unnamed reason".to_owned(),
            }
        })
        .collect();
    format!("Desis ignored inbound work as {}", described.join(", "))
}
/// A router may accept a message and drop it on purpose, naming a reason. That
/// reason is the only record of work that arrived and did nothing.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn ignored_inbound(url: &str, router: Address, side: &str) -> String {
    let topic0 = alloy_primitives::keccak256(
        b"InboundMessageIgnored(uint32,uint8,bytes32,uint8)".as_slice(),
    );
    let logs = logs_of(url, router, vec![serde_json::json!(format!("{topic0:?}"))]);
    let entries = match logs.as_ref() {
        Some(entries) if entries.is_empty() => {
            return format!("{side} ignored no inbound message");
        }
        Some(entries) => entries,
        None => return format!("{side} ignore log is unreadable"),
    };
    let described: Vec<String> = entries
        .iter()
        .map(|entry| {
            let topic = |i: usize| {
                entry["topics"][i]
                    .as_str()
                    .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            };
            let msg_type = match topic(2) {
                Some(1) => "bids batch".to_owned(),
                Some(2) => "bids done".to_owned(),
                Some(3) => "auction stage start".to_owned(),
                Some(4) => "auction stage clearing".to_owned(),
                Some(5) => "auction result".to_owned(),
                Some(6) => "issuance instructions".to_owned(),
                Some(7) => "refund instructions".to_owned(),
                Some(8) => "mark called".to_owned(),
                Some(9) => "mark qualified".to_owned(),
                Some(other) => format!("message type {other}"),
                None => "unreadable message type".to_owned(),
            };
            let reason = entry["data"]
                .as_str()
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
            let reason = match reason {
                Some(1) => "duplicate",
                Some(2) => "obsolete",
                Some(3) => "conflict",
                Some(4) => "not found",
                Some(5) => "late",
                Some(6) => "invalid",
                Some(7) => "deferred",
                _ => "unnamed reason",
            };
            format!("{msg_type} as {reason}")
        })
        .collect();
    format!("{side} ignored {}", described.join(", "))
}
pub(crate) fn stages_received(url: &str, venue_router: Address, worldwide_day: u32) -> String {
    let topic0 =
        alloy_primitives::keccak256(b"AuctionStageReceived(uint32,uint32,uint8)".as_slice());
    let day_topic = format!("0x{:064x}", worldwide_day);
    let logs = logs_of(
        url,
        venue_router,
        vec![
            serde_json::json!(format!("{topic0:?}")),
            serde_json::Value::Null,
            serde_json::json!(day_topic),
        ],
    );
    match logs.as_ref() {
        Some(entries) if entries.is_empty() => "the venue received no inbound stage".to_owned(),
        Some(entries) => format!("the venue received {} inbound stages", entries.len()),
        None => "the venue stage log is unreadable".to_owned(),
    }
}
#[cfg(feature = "ocomp-integration")]
pub(crate) fn venue_bid_counts(url: &str, venue: Address, worldwide_day: u32) -> String {
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
pub(crate) fn relayed_bids(url: &str, worldwide_day: u32, chain_id: u32) -> String {
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
/// The loopback adapter delivers inside the send and swallows a failure: it
/// parks the delivery with the revert reason and still reports the send as
/// done. That reason is the only place a refused message explains itself.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn parked_deliveries(world: &World) -> String {
    let url = world.rpc.url(world.validators.primary_port());
    let Some(contracts) = world.state.origin_contracts.clone() else {
        return "no deploy recorded".to_owned();
    };
    let topic0 = alloy_primitives::keccak256(b"DeliveryParked(uint256,bytes)".as_slice());
    let logs = logs_of(
        &url,
        contracts.loopback,
        vec![serde_json::json!(format!("{topic0:?}"))],
    );
    match logs.as_ref() {
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
pub(crate) fn refunds_were_sent(world: &World, worldwide_day: u32) -> bool {
    let url = world.rpc.url(world.validators.primary_port());
    let Some(contracts) = world.state.origin_contracts.clone() else {
        return false;
    };
    let topic0 =
        alloy_primitives::keccak256(b"RefundInstructionsSent(bytes32,uint32,uint256)".as_slice());
    logs_of(
        &url,
        contracts.origin_router,
        vec![
            serde_json::json!(format!("{topic0:?}")),
            serde_json::Value::Null,
            serde_json::json!(format!("0x{worldwide_day:064x}")),
        ],
    )
    .is_some_and(|entries| !entries.is_empty())
}
/// Clearing fires the auction result, the issuance instructions and the refunds
/// back to back. A router that cannot pay the bridge fee parks the tail of that
/// burst instead of reverting, so a parked send is the first thing to look at
/// when a message never lands. Counting only: a run must not need a nudge.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn parked_origin_sends(world: &World) -> u32 {
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
sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    struct IntexCallTrigger {
        uint32 callWindow;
        uint32 callThreshold;
        uint32 callNoticePeriod;
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    struct SeriesData {
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint32 issuedIntexCount;
        uint128 promisLoadMinor;
        uint64 entryPriceMinor;
        uint64 floorPriceMinor;
        uint64 callPriceMinor;
        IntexCallTrigger callTrigger;
        uint32 issuedAt;
        uint32 calledAt;
        uint32 totalSupply;
        uint8 status;
        uint8 state;
        uint32 worldwideDay;
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IIssuedSeries {
        function seriesExists(bytes14 seriesId) external view returns (bool);
        function issuedTokenId(bytes14 seriesId) external pure returns (uint256);
        function settledTokenId(bytes14 seriesId) external pure returns (uint256);
        function statusOf(uint256 tokenId) external view returns (uint8);
        function readData(bytes14 seriesId) external view returns (SeriesData);
        function pendingMark(bytes14 seriesId) external view returns (uint8);
        function applyPendingMark(bytes14 seriesId) external;
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
pub(crate) fn issued_series(
    url: &str,
    venue_router: Address,
) -> Option<alloy_primitives::FixedBytes<14>> {
    let topic0 = alloy_primitives::keccak256(
        b"IssuanceInstructionsReceived(uint32,bytes14,uint256)".as_slice(),
    );
    let logs = logs_of(
        url,
        venue_router,
        vec![serde_json::json!(format!("{topic0:?}"))],
    )?;
    let topic = logs.first()?.get("topics")?.as_array()?.get(2)?.as_str()?;
    let word: alloy_primitives::B256 = topic.parse().ok()?;
    Some(alloy_primitives::FixedBytes::<14>::from_slice(
        &word.0[..14],
    ))
}
#[cfg(feature = "ocomp-integration")]
pub(crate) fn deferred_mints(url: &str, venue_router: Address) -> usize {
    let topic0 = alloy_primitives::keccak256(
        b"IssuanceMintDeferred(uint256,bytes14,address,bytes)".as_slice(),
    );
    logs_of(
        url,
        venue_router,
        vec![serde_json::json!(format!("{topic0:?}"))],
    )
    .map_or(0, |e| e.len())
}
/// Whether Desis issued anything at all. Absent issuance means one of two very
/// different things, and only the clearing event tells them apart.
#[cfg(feature = "ocomp-integration")]
pub(crate) fn cleared_empty(url: &str, worldwide_day: u32) -> Option<bool> {
    let desis: Address = origin_venue::DESIS.parse().ok()?;
    let day_topic = format!("0x{worldwide_day:064x}");
    let seen = |signature: &[u8]| {
        let topic0 = alloy_primitives::keccak256(signature);
        logs_of(
            url,
            desis,
            vec![
                serde_json::json!(format!("{topic0:?}")),
                serde_json::json!(day_topic),
            ],
        )
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

/// What `holder` owns of `series`: units still issued, and units already settled.
pub(crate) fn series_balances(
    url: &str,
    nft: Address,
    series: alloy_primitives::FixedBytes<14>,
    holder: Address,
) -> Option<(u64, u64)> {
    let issued_id = eth::read_call(
        url,
        nft,
        &IIssuedSeries::issuedTokenIdCall { seriesId: series },
    )?;
    let settled_id = eth::read_call(
        url,
        nft,
        &IIssuedSeries::settledTokenIdCall { seriesId: series },
    )?;
    let issued = eth::read_call(
        url,
        nft,
        &IIssuedSeries::balanceOfCall {
            account: holder,
            id: issued_id,
        },
    )?;
    let settled = eth::read_call(
        url,
        nft,
        &IIssuedSeries::balanceOfCall {
            account: holder,
            id: settled_id,
        },
    )?;
    Some((issued.to::<u64>(), settled.to::<u64>()))
}

/// Whether the collection knows the series at all.
pub(crate) fn series_exists(
    url: &str,
    nft: Address,
    series: alloy_primitives::FixedBytes<14>,
) -> bool {
    eth::read_call(
        url,
        nft,
        &IIssuedSeries::seriesExistsCall { seriesId: series },
    )
    .unwrap_or_default()
}

/// A series' lifecycle state as the collection has it: 0 Issued, 1 Qualified, 2 Called.
pub(crate) fn series_state(
    url: &str,
    nft: Address,
    series: alloy_primitives::FixedBytes<14>,
) -> Option<u8> {
    eth::read_call(url, nft, &IIssuedSeries::readDataCall { seriesId: series })
        .map(|data| data.state)
}

/// Units the series was issued across every chain. A forfeit is measured against
/// this, not against what one chain happens to hold.
pub(crate) fn series_issued_count(
    url: &str,
    nft: Address,
    series: alloy_primitives::FixedBytes<14>,
) -> Option<u32> {
    eth::read_call(url, nft, &IIssuedSeries::readDataCall { seriesId: series })
        .map(|data| data.issuedIntexCount)
}

/// When the series was Called, as both chains recorded it.
pub(crate) fn series_called_at(
    url: &str,
    nft: Address,
    series: alloy_primitives::FixedBytes<14>,
) -> Option<u32> {
    eth::read_call(url, nft, &IIssuedSeries::readDataCall { seriesId: series })
        .map(|data| data.calledAt)
}

/// When the notice a Called series was given runs out. Expiry is derived against
/// this, never stored, so a scenario has to wait past it rather than watch a flag.
pub(crate) fn series_call_deadline(
    url: &str,
    nft: Address,
    series: alloy_primitives::FixedBytes<14>,
) -> Option<u64> {
    eth::read_call(url, nft, &IIssuedSeries::readDataCall { seriesId: series })
        .map(|data| u64::from(data.calledAt) + u64::from(data.callTrigger.callNoticePeriod))
}

/// The prices the engine derived at issuance: entry, floor, and call.
pub(crate) fn series_prices(
    url: &str,
    nft: Address,
    series: alloy_primitives::FixedBytes<14>,
) -> Option<(u64, u64, u64)> {
    eth::read_call(url, nft, &IIssuedSeries::readDataCall { seriesId: series }).map(|data| {
        (
            data.entryPriceMinor,
            data.floorPriceMinor,
            data.callPriceMinor,
        )
    })
}

/// PROMIS-units the series carries per Intex unit.
pub(crate) fn series_promis_load(
    url: &str,
    nft: Address,
    series: alloy_primitives::FixedBytes<14>,
) -> Option<u128> {
    eth::read_call(url, nft, &IIssuedSeries::readDataCall { seriesId: series })
        .map(|data| data.promisLoadMinor)
}

/// The Issued token id of `series`, which the bridge moves.
pub(crate) fn issued_token_id(
    url: &str,
    nft: Address,
    series: alloy_primitives::FixedBytes<14>,
) -> Option<alloy_primitives::U256> {
    eth::read_call(
        url,
        nft,
        &IIssuedSeries::issuedTokenIdCall { seriesId: series },
    )
}
