//! Desis takes the bids from the ERC-7786 hub and sends its messages from its outbox.

use super::*;
use alloy_primitives::B256;
use alloy_sol_types::SolValue;
use outbe_core_common::addresses::{ERC7786_HUB_ADDRESS, INTEX_AUCTION_TARGET_ROUTER_ADDRESS};
use outbe_intexfactory::transport::wire::{
    MSG_BIDS_PAGE, MSG_BIDS_PAUSE, MSG_BIDS_TOTALS, MSG_INTEX_AUCTION_CLEARING_START,
    MSG_INTEX_AUCTION_START,
};
use outbe_system_common::addresses::{CROSSCHAIN_BRIDGE_ADDRESS, DESIS_ADDRESS};

use crate::precompile::{dispatch, IDesis};

/// `InteroperableAddress.formatEvmV1(chain, account)`.
fn interop(chain: u32, account: Address) -> Vec<u8> {
    let reference: Vec<u8> = chain
        .to_be_bytes()
        .into_iter()
        .skip_while(|byte| *byte == 0)
        .collect();
    let mut out = vec![0x00, 0x01, 0x00, 0x00, reference.len() as u8];
    out.extend_from_slice(&reference);
    out.push(20);
    out.extend_from_slice(account.as_slice());
    out
}

fn bids_page(body_chain: u32, count: u8) -> Vec<u8> {
    let rows = bids(count, 200);
    let bidders: Vec<Address> = rows.iter().map(|row| row.bidder_address).collect();
    let packed: Vec<U256> = rows
        .iter()
        .map(|row| {
            U256::from(row.reference_currency)
                | (U256::from(row.issuance_currency) << 16)
                | (U256::from(row.timestamp) << 32)
                | (U256::from(row.intex_bid_rate) << 64)
                | (U256::from(row.intex_quantity) << 96)
        })
        .collect();
    let mut payload = vec![1, MSG_BIDS_PAGE];
    payload.extend(
        (
            EMISSION_DAY.value(),
            body_chain,
            0u16,
            1u16,
            bidders,
            packed,
        )
            .abi_encode_params(),
    );
    payload
}

fn bids_totals(total_bids: u32) -> Vec<u8> {
    let mut payload = vec![1, MSG_BIDS_TOTALS];
    payload.extend_from_slice(&EMISSION_DAY.value().to_be_bytes());
    payload.extend_from_slice(&SRC_CHAIN.to_be_bytes());
    payload.extend_from_slice(&1u16.to_be_bytes());
    payload.extend_from_slice(&total_bids.to_be_bytes());
    payload
}

fn bids_pause() -> Vec<u8> {
    let mut payload = vec![1, MSG_BIDS_PAUSE];
    payload.extend_from_slice(&EMISSION_DAY.value().to_be_bytes());
    payload.extend_from_slice(&SRC_CHAIN.to_be_bytes());
    payload.extend_from_slice(&1u16.to_be_bytes());
    payload.extend_from_slice(&2u16.to_be_bytes());
    payload
}

fn deliver(
    s: &StorageHandle,
    caller: Address,
    sender: Vec<u8>,
    payload: Vec<u8>,
) -> outbe_primitives::error::Result<Bytes> {
    let call = IDesis::receiveMessageCall {
        receiveId: B256::repeat_byte(0x11),
        sender: sender.into(),
        payload: payload.into(),
    };
    dispatch(s.clone(), &call.abi_encode(), caller, U256::ZERO)
}

fn from_intex_auction_target_router(s: &StorageHandle, payload: Vec<u8>) -> Bytes {
    deliver(
        s,
        ERC7786_HUB_ADDRESS,
        interop(SRC_CHAIN, INTEX_AUCTION_TARGET_ROUTER_ADDRESS),
        payload,
    )
    .unwrap()
}

fn ignored(storage: &HashMapStorageProvider) -> Vec<(u32, u32, u8)> {
    storage
        .get_events(DESIS_ADDRESS)
        .iter()
        .filter_map(|log| IDesis::IntexAuctionBidsIgnored::decode_log_data(log).ok())
        .map(|event| (event.emissionDay, event.srcChainId, event.reason))
        .collect()
}

fn chain_bids(s: &StorageHandle, chain: u32) -> u32 {
    s.contract::<DesisContract>()
        .chain_bid_count
        .read(&DesisContract::chain_key(EMISSION_DAY, chain))
        .unwrap()
}

#[test]
fn the_hub_delivers_a_page_and_its_totals_from_intex_auction_target_router() {
    with_targets(&[SRC_CHAIN], |s| {
        open_revealing(&s);
        let answer = from_intex_auction_target_router(&s, bids_page(SRC_CHAIN, 3));
        assert_eq!(
            IDesis::receiveMessageCall::abi_decode_returns(&answer).unwrap(),
            IDesis::receiveMessageCall::SELECTOR
        );
        from_intex_auction_target_router(&s, bids_totals(3));
        assert_eq!(chain_bids(&s, SRC_CHAIN), 3);
        let completed = s
            .contract::<DesisContract>()
            .bids_completed
            .read(&DesisContract::chain_key(EMISSION_DAY, SRC_CHAIN))
            .unwrap();
        assert_eq!(completed, 1);
    });
}

#[test]
fn only_the_hub_may_deliver() {
    with_targets(&[SRC_CHAIN], |s| {
        open_revealing(&s);
        assert!(deliver(
            &s,
            bidder(9),
            interop(SRC_CHAIN, INTEX_AUCTION_TARGET_ROUTER_ADDRESS),
            bids_page(SRC_CHAIN, 1)
        )
        .is_err());
        assert_eq!(chain_bids(&s, SRC_CHAIN), 0);
    });
}

#[test]
fn a_page_from_another_sender_is_ignored() {
    let mut storage = fixture_provider(&[SRC_CHAIN]);
    StorageHandle::enter(&mut storage, |s| {
        open_revealing(&s);
        deliver(
            &s,
            ERC7786_HUB_ADDRESS,
            interop(SRC_CHAIN, bidder(9)),
            bids_page(SRC_CHAIN, 1),
        )
        .unwrap();
        assert_eq!(chain_bids(&s, SRC_CHAIN), 0);
    });
    assert_eq!(ignored(&storage), vec![(0, 0, IGNORED_CONFLICT)]);
}

#[test]
fn a_page_whose_body_names_another_chain_is_ignored() {
    let mut storage = fixture_provider(&[SRC_CHAIN]);
    StorageHandle::enter(&mut storage, |s| {
        open_revealing(&s);
        from_intex_auction_target_router(&s, bids_page(SRC_CHAIN + 1, 1));
        assert_eq!(chain_bids(&s, SRC_CHAIN), 0);
    });
    assert_eq!(
        ignored(&storage),
        vec![(EMISSION_DAY.value(), SRC_CHAIN, IGNORED_CONFLICT)]
    );
}

#[test]
fn a_page_from_a_chain_outside_the_day_is_ignored() {
    let other = SRC_CHAIN + 1;
    let mut storage = fixture_provider(&[SRC_CHAIN]);
    StorageHandle::enter(&mut storage, |s| {
        open_revealing(&s);
        deliver(
            &s,
            ERC7786_HUB_ADDRESS,
            interop(other, INTEX_AUCTION_TARGET_ROUTER_ADDRESS),
            bids_page(other, 1),
        )
        .unwrap();
        assert_eq!(chain_bids(&s, other), 0);
    });
    assert_eq!(
        ignored(&storage),
        vec![(EMISSION_DAY.value(), other, IGNORED_NOT_FOUND)]
    );
}

#[test]
fn a_page_before_the_reveal_reverts_so_the_hub_delivers_it_again() {
    with_targets(&[SRC_CHAIN], |s| {
        brief(&s, true);
        runtime::advance_intex_auctions_at(&s, NOW).unwrap();
        assert!(deliver(
            &s,
            ERC7786_HUB_ADDRESS,
            interop(SRC_CHAIN, INTEX_AUCTION_TARGET_ROUTER_ADDRESS),
            bids_page(SRC_CHAIN, 1)
        )
        .is_err());
    });
}

#[test]
fn a_pause_queues_the_next_round_with_the_gas_of_the_first() {
    let mut storage = fixture_provider(&[SRC_CHAIN]);
    StorageHandle::enter(&mut storage, |s| {
        open_clearing(&s, 10);
        from_intex_auction_target_router(&s, bids_pause());
    });
    let rounds = outbox_of_type(&mut storage, MSG_INTEX_AUCTION_CLEARING_START);
    assert_eq!(rounds.len(), 2);
    assert_eq!(rounds[0].message_gas, rounds[1].message_gas);
    assert_eq!(rounds[1].payload, rounds[0].payload);
}

#[test]
fn a_pause_after_the_intake_closed_is_ignored() {
    let mut storage = fixture_provider(&[SRC_CHAIN]);
    StorageHandle::enter(&mut storage, |s| {
        open_clearing(&s, 10);
        relay_bids(&s, SRC_CHAIN, 1, 200);
        clear(&s);
        from_intex_auction_target_router(&s, bids_pause());
    });
    assert_eq!(
        outbox_of_type(&mut storage, MSG_INTEX_AUCTION_CLEARING_START).len(),
        1
    );
    assert_eq!(
        ignored(&storage),
        vec![(EMISSION_DAY.value(), SRC_CHAIN, IGNORED_OBSOLETE)]
    );
}

#[test]
fn a_day_with_no_chain_in_auctions_is_canceled_before_its_start() {
    let mut storage = fixture_provider(&[]);
    StorageHandle::enter(&mut storage, |s| {
        brief(&s, true);
        runtime::advance_intex_auctions_at(&s, NOW).unwrap();
        assert_eq!(stage_of(&s), AuctionStage::Canceled);
    });
    let cancels: Vec<u8> = storage
        .get_events(DESIS_ADDRESS)
        .iter()
        .filter_map(|log| IDesis::DesisCanceled::decode_log_data(log).ok())
        .map(|event| event.reason)
        .collect();
    assert_eq!(cancels, vec![crate::constants::CANCELED_NO_TARGETS]);
    assert!(outbox_messages(&mut storage).is_empty());
}

#[test]
fn the_start_freezes_only_the_chains_in_auctions() {
    use outbe_intexfactory::targets::{pack_target, TargetMode};
    let series_only = 77;
    let mut storage = fixture_provider(&[SRC_CHAIN]);
    StorageHandle::enter(&mut storage, |s| {
        outbe_intexfactory::IntexFactoryContract::new(s.clone())
            .target_chains
            .push(pack_target(series_only, TargetMode::Series))
            .unwrap();
        brief(&s, true);
        runtime::advance_intex_auctions_at(&s, NOW).unwrap();
        assert_eq!(
            outbe_intexfactory::targets::day_targets(&s, EMISSION_DAY).unwrap(),
            vec![SRC_CHAIN]
        );
    });
    let starts = outbox_of_type(&mut storage, MSG_INTEX_AUCTION_START);
    assert_eq!(
        starts.iter().map(|m| m.chain_id).collect::<Vec<_>>(),
        vec![SRC_CHAIN]
    );
}

#[test]
fn flush_sends_with_the_bridge_and_emits_the_sent_event() {
    let mut storage = fixture_provider(&[SRC_CHAIN]);
    storage.stub_sub_call_at(
        CROSSCHAIN_BRIDGE_ADDRESS,
        Bytes::from(B256::repeat_byte(0xCD).to_vec()),
    );
    StorageHandle::enter(&mut storage, |s| {
        brief(&s, true);
        runtime::advance_intex_auctions_at(&s, NOW).unwrap();
        crate::outbox::flush(&s).unwrap();
    });
    assert!(outbox_messages(&mut storage).is_empty());
    let sent: Vec<_> = storage
        .get_events(DESIS_ADDRESS)
        .iter()
        .filter_map(|log| IDesis::IntexAuctionStartSent::decode_log_data(log).ok())
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].sendId, B256::repeat_byte(0xCD));
    assert_eq!(sent[0].emissionDay, EMISSION_DAY.value());
    assert_eq!(sent[0].dstChainId, SRC_CHAIN);
}

#[test]
fn a_failed_send_emits_its_send_failed_event_and_stays() {
    let mut storage = fixture_provider(&[SRC_CHAIN]);
    StorageHandle::enter(&mut storage, |s| {
        brief(&s, true);
        runtime::advance_intex_auctions_at(&s, NOW).unwrap();
        crate::outbox::flush(&s).unwrap();
    });
    assert_eq!(outbox_messages(&mut storage).len(), 1);
    let failed: Vec<_> = storage
        .get_events(DESIS_ADDRESS)
        .iter()
        .filter_map(|log| IDesis::IntexAuctionStartSendFailed::decode_log_data(log).ok())
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].failures, 1);
    assert_eq!(failed[0].dstChainId, SRC_CHAIN);
}
