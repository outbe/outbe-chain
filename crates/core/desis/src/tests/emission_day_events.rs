//! Desis limit identity on every terminal auction, and a quiet repeat tick.

use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::DESIS_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_promislimit::PromisLimitContract;

use crate::constants::{
    CANCELED_BELOW_ONE_UNIT, CANCELED_LATE_START, CANCELED_LIMIT_OVERFLOW, CANCELED_NO_LIMIT,
    CANCELED_UNPRICED, COMMIT_WINDOW_SECONDS, INTEX_ISSUANCE_WINDOW_SECONDS, REVEAL_WINDOW_SECONDS,
};
use crate::precompile::IDesis;
use crate::runtime;
use crate::schema::{AuctionStage, DesisContract};

use super::{
    brief, brief_at, clear, mark_done, open_clearing, relay_bids, unprice_day, ANCHOR, CHAIN_ID,
    EMISSION_DAY, ENTRY_PRICE, LAST_CLEARING, LAST_START, LOAD_MINOR, NOW, ORIGIN_ROUTER_ADDRESS,
    SRC_CHAIN,
};

fn world(f: impl FnOnce(&StorageHandle)) -> HashMapStorageProvider {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(NOW));
    storage.stub_sub_call_at(ORIGIN_ROUTER_ADDRESS, super::targets_stub(&[SRC_CHAIN]));
    storage.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );
    StorageHandle::enter(&mut storage, |s| {
        outbe_intex::IntexContract::new(s.clone())
            .config_profile
            .write(outbe_intex::config::PROFILE_PROD)
            .unwrap();
        for day in 0..5 {
            super::seed_rate(&s, NOW + day * SECONDS_PER_DAY, ENTRY_PRICE);
        }
        f(&s);
    });
    storage
}

fn logs(storage: &HashMapStorageProvider) -> &[alloy_primitives::LogData] {
    storage.get_events(DESIS_ADDRESS)
}

fn amount_of<E>(storage: &HashMapStorageProvider) -> Vec<U256>
where
    E: SolEvent,
    E: AmountMinor,
{
    logs(storage)
        .iter()
        .filter_map(|log| E::decode_log_data(log).ok())
        .map(|event| event.amount())
        .collect()
}

trait AmountMinor {
    fn amount(&self) -> U256;
}

impl AmountMinor for IDesis::DesisLimitSet {
    fn amount(&self) -> U256 {
        self.amountMinor
    }
}

impl AmountMinor for IDesis::DesisLimitAllocated {
    fn amount(&self) -> U256 {
        self.amountMinor
    }
}

impl AmountMinor for IDesis::DesisLimitReturned {
    fn amount(&self) -> U256 {
        self.amountMinor
    }
}

fn assert_limit_identity(storage: &mut HashMapStorageProvider, returned_to_promis: U256) {
    let set = amount_of::<IDesis::DesisLimitSet>(storage);
    let allocated = amount_of::<IDesis::DesisLimitAllocated>(storage);
    let returned = amount_of::<IDesis::DesisLimitReturned>(storage);
    assert_eq!(set.len(), 1);
    assert_eq!(allocated.len(), 1);
    assert_eq!(returned.len(), 1);
    assert_eq!(set[0], allocated[0] + returned[0]);
    let pool = StorageHandle::enter(storage, |s| {
        PromisLimitContract::new(s.clone()).promis_limit().unwrap()
    });
    assert_eq!(pool, returned_to_promis);
}

fn cancel_reason(storage: &HashMapStorageProvider) -> u8 {
    logs(storage)
        .iter()
        .find_map(|log| IDesis::IntexAuctionCanceled::decode_log_data(log).ok())
        .expect("IntexAuctionCanceled")
        .reason
}

fn assert_cancel_before_limit_events(storage: &HashMapStorageProvider) {
    let events = logs(storage);
    let at = |signature: alloy_primitives::B256| {
        events
            .iter()
            .position(|log| log.topics().first() == Some(&signature))
            .expect("event")
    };
    let canceled = at(IDesis::IntexAuctionCanceled::SIGNATURE_HASH);
    let allocated = at(IDesis::DesisLimitAllocated::SIGNATURE_HASH);
    let returned = at(IDesis::DesisLimitReturned::SIGNATURE_HASH);
    assert!(canceled < allocated);
    assert!(allocated + 1 == returned);
}

fn assert_repeat_tick_is_quiet(storage: &mut HashMapStorageProvider) {
    let before = logs(storage).len();
    StorageHandle::enter(storage, |s| {
        runtime::schedule_tick(&s, NOW).unwrap();
        runtime::tick_gate(&BlockRuntimeContext::new(
            BlockContext {
                block_number: 3,
                timestamp: NOW,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: u64::max(NOW, 1),
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            s.clone(),
        ))
        .unwrap();
    });
    assert_eq!(logs(storage).len(), before);
}

#[test]
fn a_partial_clear_balances_and_a_repeat_tick_is_quiet() {
    let mut storage = world(|s| {
        open_clearing(s, 3);
        relay_bids(s, SRC_CHAIN, 1, 200);
        assert_eq!(clear(s).issued_units, 1);
    });
    assert_limit_identity(&mut storage, U256::from(2 * LOAD_MINOR));
    assert_eq!(
        amount_of::<IDesis::DesisLimitAllocated>(&storage),
        vec![U256::from(LOAD_MINOR)]
    );
    assert_repeat_tick_is_quiet(&mut storage);
}

#[test]
fn a_full_clear_reports_a_zero_return_without_crediting_it() {
    let mut storage = world(|s| {
        open_clearing(s, 2);
        relay_bids(s, SRC_CHAIN, 2, 200);
        assert_eq!(clear(s).issued_units, 2);
    });
    assert_limit_identity(&mut storage, U256::ZERO);
    assert_eq!(
        amount_of::<IDesis::DesisLimitReturned>(&storage),
        vec![U256::ZERO]
    );
    assert_eq!(
        logs(&storage)
            .iter()
            .filter(
                |log| log.topics().first() == Some(&IDesis::IntexAuctionCompleted::SIGNATURE_HASH)
            )
            .count(),
        1
    );
    let bids_completed = logs(&storage)
        .iter()
        .find_map(|log| IDesis::BidsCompleted::decode_log_data(log).ok())
        .expect("BidsCompleted");
    assert_eq!(bids_completed.emissionDay, EMISSION_DAY.value());
    assert_eq!(bids_completed.srcChainId, SRC_CHAIN);
}

#[test]
fn an_empty_clear_returns_the_whole_limit() {
    let mut storage = world(|s| {
        open_clearing(s, 3);
        runtime::process_bids_batch(
            s.clone(),
            runtime::Inbound {
                caller: ORIGIN_ROUTER_ADDRESS,
                emission_day: EMISSION_DAY,
                src_chain_id: SRC_CHAIN,
            },
            0,
            1,
            vec![],
        )
        .unwrap();
        mark_done(s, SRC_CHAIN, 1, 0);
        assert_eq!(clear(s).issued_units, 0);
    });
    assert_limit_identity(&mut storage, U256::from(3 * LOAD_MINOR));
    assert_eq!(
        amount_of::<IDesis::DesisLimitAllocated>(&storage),
        vec![U256::ZERO]
    );
    assert!(logs(&storage).iter().any(|log| {
        IDesis::IntexAuctionCompletedEmpty::decode_log_data(log)
            .map(|event| event.totalDemand == 0)
            .unwrap_or(false)
    }));
}

type CancelCase = (u8, fn(&StorageHandle<'_>), U256);

#[test]
fn each_cancel_reason_balances_and_a_repeat_tick_is_quiet() {
    let cases: [CancelCase; 4] = [
        (
            CANCELED_NO_LIMIT,
            |s| {
                brief_at(s, EMISSION_DAY, 0, false);
                unprice_day(s, NOW);
                runtime::schedule_tick(s, NOW).unwrap();
            },
            U256::ZERO,
        ),
        (
            CANCELED_UNPRICED,
            |s| {
                brief_at(s, EMISSION_DAY, 0, true);
                unprice_day(s, NOW);
                runtime::schedule_tick(s, NOW).unwrap();
            },
            U256::ZERO,
        ),
        (
            CANCELED_BELOW_ONE_UNIT,
            |s| {
                brief_at(s, EMISSION_DAY, LOAD_MINOR - 1, true);
                runtime::schedule_tick(s, NOW).unwrap();
            },
            U256::from(LOAD_MINOR - 1),
        ),
        (
            CANCELED_LATE_START,
            |s| {
                brief(s, true);
                runtime::schedule_tick(s, LAST_START + 1).unwrap();
            },
            U256::from(10 * LOAD_MINOR),
        ),
    ];
    for (reason, drive, returned) in cases {
        let mut storage = world(drive);
        assert_eq!(cancel_reason(&storage), reason, "reason {reason}");
        assert_limit_identity(&mut storage, returned);
        assert_cancel_before_limit_events(&storage);
        assert_repeat_tick_is_quiet(&mut storage);
    }
}

#[test]
fn an_oversized_brief_cancels_in_order_and_a_second_brief_is_rejected() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |s| {
        assert!(matches!(
            crate::api::dispatch_auction_brief(
                s.clone(),
                crate::api::AuctionBrief {
                    emission_day: EMISSION_DAY,
                    desis_limit_minor: U256::MAX,
                    has_limit: true,
                },
                crate::api::BriefOverflowPolicy::ReturnToPromisLimit,
            )
            .unwrap(),
            crate::api::AuctionBriefReceipt::ReturnedToPromisLimit { .. }
        ));
        assert_eq!(
            s.contract::<DesisContract>()
                .read_stage(EMISSION_DAY)
                .unwrap(),
            AuctionStage::Cancelled
        );
        assert!(crate::api::dispatch_auction_brief(
            s.clone(),
            crate::api::AuctionBrief {
                emission_day: EMISSION_DAY,
                desis_limit_minor: U256::MAX,
                has_limit: true,
            },
            crate::api::BriefOverflowPolicy::ReturnToPromisLimit,
        )
        .is_err());
    });
    let names: Vec<_> = logs(&storage)
        .iter()
        .map(|log| log.topics().first().copied())
        .collect();
    assert_eq!(
        names,
        vec![
            Some(IDesis::DesisLimitSet::SIGNATURE_HASH),
            Some(IDesis::IntexAuctionCanceled::SIGNATURE_HASH),
            Some(IDesis::DesisLimitAllocated::SIGNATURE_HASH),
            Some(IDesis::DesisLimitReturned::SIGNATURE_HASH),
        ]
    );
    assert_eq!(cancel_reason(&storage), CANCELED_LIMIT_OVERFLOW);
    assert_limit_identity(&mut storage, U256::ZERO);
    assert_repeat_tick_is_quiet(&mut storage);
}

#[test]
fn a_failed_auction_reports_the_stage_before_the_zero_allocation() {
    let mut storage = world(|s| {
        brief(s, true);
        runtime::schedule_tick(s, NOW).unwrap();
        runtime::schedule_tick(s, LAST_CLEARING).unwrap();
        assert_eq!(
            s.contract::<DesisContract>()
                .read_stage(EMISSION_DAY)
                .unwrap(),
            AuctionStage::Failed
        );
    });
    let events = logs(&storage);
    let failed = events
        .iter()
        .position(|log| log.topics().first() == Some(&IDesis::IntexAuctionFailed::SIGNATURE_HASH))
        .expect("IntexAuctionFailed");
    let allocated = events
        .iter()
        .position(|log| log.topics().first() == Some(&IDesis::DesisLimitAllocated::SIGNATURE_HASH))
        .expect("DesisLimitAllocated");
    assert!(failed < allocated);
    let stage = events
        .iter()
        .find_map(|log| IDesis::IntexAuctionFailed::decode_log_data(log).ok())
        .unwrap()
        .stage;
    assert_eq!(stage, AuctionStage::CommittingBids as u8);
    assert_limit_identity(&mut storage, U256::from(10 * LOAD_MINOR));
    assert_eq!(
        amount_of::<IDesis::DesisLimitAllocated>(&storage),
        vec![U256::ZERO]
    );
    assert_repeat_tick_is_quiet(&mut storage);
}

#[test]
fn a_live_start_announces_the_windows_and_the_folded_profile() {
    let mut storage = world(|s| {
        brief(s, true);
        runtime::schedule_tick(s, NOW).unwrap();
    });
    let config = StorageHandle::enter(&mut storage, |s| {
        s.contract::<DesisContract>()
            .read_auction_config(EMISSION_DAY)
            .unwrap()
    });
    let created = logs(&storage)
        .iter()
        .find_map(|log| IDesis::IntexAuctionCreated::decode_log_data(log).ok())
        .expect("IntexAuctionCreated");
    assert_eq!(created.emissionDay, EMISSION_DAY.value());
    assert_eq!(created.commitEnd, ANCHOR + COMMIT_WINDOW_SECONDS);
    assert_eq!(created.revealEnd, created.commitEnd + REVEAL_WINDOW_SECONDS);
    assert_eq!(
        created.issuanceEnd,
        created.revealEnd + INTEX_ISSUANCE_WINDOW_SECONDS
    );
    assert_eq!(created.promisLoadMinor, config.promis_load_minor);
    assert_eq!(created.minBidRate, config.min_intex_bid_rate);
    assert_eq!(created.minUnits, u32::from(config.min_intex_bid_quantity));
    assert_eq!(created.bidBondMinor, config.commit_bond_minor);
}
