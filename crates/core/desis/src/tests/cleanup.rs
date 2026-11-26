use alloy_primitives::Address;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_system_common::time::EmissionDate;

use crate::{
    run_cleanup,
    runtime::{collect_intex_auction_bids, complete_intex_auction_bids, Inbound},
    AuctionStage, DesisContract, IntexAuctionBidData,
};

const DAY: EmissionDate = EmissionDate::new(20231113);

fn seed(storage: StorageHandle<'_>, count: u32) {
    let contract = storage.contract::<DesisContract>();
    contract.track_intake_chain(DAY, 1).unwrap();
    let bid = IntexAuctionBidData {
        bidder_address: Address::repeat_byte(1),
        intex_bid_rate: 1,
        timestamp: 1,
        intex_quantity: 1,
        issuance_currency: 840,
        reference_currency: 840,
    };
    for _ in 0..count {
        contract.append_bid(DAY, 1, &bid).unwrap();
    }
    contract.write_stage(DAY, AuctionStage::Completed).unwrap();
    contract.day_bid_count.write(&DAY, 0).unwrap();
    contract.queue_intake_cleanup(DAY).unwrap();
}

#[test]
fn cleanup_1088_bids_finishes_in_five_bounded_pages() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |s| seed(s, 1088));
    provider.set_gas_limit(30_000_000);
    provider.enable_production_storage_gas_metering();
    StorageHandle::enter(&mut provider, |s| {
        let contract = s.contract::<DesisContract>();
        let key = DesisContract::chain_key(DAY, 1);
        let mut maximum = 0;
        for page in 0..5 {
            let before = s.gas_used().unwrap();
            run_cleanup(s.clone()).unwrap();
            let gas = s.gas_used().unwrap() - before;
            maximum = maximum.max(gas);
            assert!(gas < 3_000_000, "page {page}: {gas}");
            assert_eq!(contract.day_bid_count.read(&DAY).unwrap(), 0);
        }
        println!("1088 bid cleanup maximum page gross gas: {maximum}");
        assert_eq!(contract.chain_bid_count.read(&key).unwrap(), 0);
        assert_eq!(contract.intake_chain_count.read(&DAY).unwrap(), 0);
        assert_eq!(contract.cleanup_head.read().unwrap(), 0);
        assert_eq!(contract.cleanup_tail.read().unwrap(), 0);
        assert_eq!(contract.cleanup_pending.read(&DAY).unwrap(), 2);
        for index in 0..1088 {
            let key = DesisContract::bid_key(DAY, 1, index);
            assert_eq!(contract.bid_bidder.read(&key).unwrap(), Address::ZERO);
            assert!(contract.bid_packed.read(&key).unwrap().is_zero());
        }
    });
}

#[test]
fn cleanup_empty_headers_are_bounded_and_fifo_progresses() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |s| {
        let contract = s.contract::<DesisContract>();
        for chain in 0..9 {
            contract.track_intake_chain(DAY, chain).unwrap();
        }
        contract.write_stage(DAY, AuctionStage::Failed).unwrap();
        contract.queue_intake_cleanup(DAY).unwrap();
        let next = EmissionDate::new(20231114);
        contract.write_stage(next, AuctionStage::Canceled).unwrap();
        contract.queue_intake_cleanup(next).unwrap();
        run_cleanup(s.clone()).unwrap();
        assert_eq!(contract.intake_chain_count.read(&DAY).unwrap(), 5);
        assert_eq!(contract.cleanup_head.read().unwrap(), DAY.value());
        run_cleanup(s.clone()).unwrap();
        assert_eq!(contract.intake_chain_count.read(&DAY).unwrap(), 1);
        run_cleanup(s.clone()).unwrap();
        assert_eq!(contract.cleanup_head.read().unwrap(), 0);
        assert_eq!(contract.cleanup_pending.read(&next).unwrap(), 2);
    });
}

#[test]
fn cleanup_enqueue_is_terminal_and_idempotent_even_after_completion() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |s| {
        let contract = s.contract::<DesisContract>();
        assert!(contract.queue_intake_cleanup(DAY).is_err());
        contract.write_stage(DAY, AuctionStage::Failed).unwrap();
        contract.queue_intake_cleanup(DAY).unwrap();
        contract.queue_intake_cleanup(DAY).unwrap();
        assert_eq!(contract.cleanup_next.read(&DAY).unwrap(), 0);
        run_cleanup(s.clone()).unwrap();
        contract.queue_intake_cleanup(DAY).unwrap();
        assert_eq!(contract.cleanup_head.read().unwrap(), 0);
    });
}

#[test]
fn cleanup_tracks_out_of_order_batches_and_done_only_chains_once() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |s| {
        let contract = s.contract::<DesisContract>();
        contract
            .write_stage(DAY, AuctionStage::RevealingBids)
            .unwrap();
        let inbound = |chain| Inbound {
            receive_id: alloy_primitives::B256::ZERO,
            emission_day: DAY,
            src_chain_id: chain,
        };
        complete_intex_auction_bids(s.clone(), inbound(7), 2, 0).unwrap();
        collect_intex_auction_bids(s.clone(), inbound(7), 1, 2, vec![]).unwrap();
        collect_intex_auction_bids(s.clone(), inbound(7), 0, 2, vec![]).unwrap();
        complete_intex_auction_bids(s.clone(), inbound(7), 2, 0).unwrap();
        collect_intex_auction_bids(s.clone(), inbound(8), 0, 1, vec![]).unwrap();
        complete_intex_auction_bids(s.clone(), inbound(9), 1, 0).unwrap();
        assert_eq!(contract.intake_chain_count.read(&DAY).unwrap(), 3);
        for (index, chain) in [7, 8, 9].into_iter().enumerate() {
            assert_eq!(
                contract
                    .intake_chain_at
                    .read(&DesisContract::chain_key(DAY, index as u32))
                    .unwrap(),
                chain
            );
        }
        contract.write_stage(DAY, AuctionStage::Failed).unwrap();
        contract.queue_intake_cleanup(DAY).unwrap();
        run_cleanup(s.clone()).unwrap();
        assert_eq!(contract.cleanup_head.read().unwrap(), 0);
        for chain in [7, 8, 9] {
            let key = DesisContract::chain_key(DAY, chain);
            assert_eq!(contract.completed_pages.read(&key).unwrap(), 0);
            assert_eq!(contract.intake_chain_member.read(&key).unwrap(), 0);
        }
    });
}

#[test]
fn cleanup_page_rolls_back_partial_bid_deletion_on_storage_failure() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |s| seed(s, 5));
    provider.fail_after_mutation_at(2);
    StorageHandle::enter(&mut provider, |s| {
        assert!(run_cleanup(s.clone()).is_err());
        let contract = s.contract::<DesisContract>();
        assert_eq!(
            contract
                .chain_bid_count
                .read(&DesisContract::chain_key(DAY, 1))
                .unwrap(),
            5
        );
        assert_eq!(contract.cleanup_pending.read(&DAY).unwrap(), 1);
        for index in 0..5 {
            let key = DesisContract::bid_key(DAY, 1, index);
            assert_eq!(
                contract.bid_bidder.read(&key).unwrap(),
                Address::repeat_byte(1)
            );
            assert!(!contract.bid_packed.read(&key).unwrap().is_zero());
        }
    });
    provider.clear_mutation_failure();
    StorageHandle::enter(&mut provider, |s| {
        run_cleanup(s.clone()).unwrap();
        assert_eq!(
            s.contract::<DesisContract>().cleanup_head.read().unwrap(),
            0
        );
    });
}

#[test]
fn cleanup_clears_the_clearing_gas_of_every_chain_of_the_day() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |s| {
        let factory = outbe_intexfactory::IntexFactoryContract::new(s.clone());
        let contract = s.contract::<DesisContract>();
        for chain in [97, 98] {
            factory.day_targets.list(&DAY).push(chain).unwrap();
            contract
                .clearing_gas
                .write(&DesisContract::chain_key(DAY, chain), 1)
                .unwrap();
        }
        contract.write_stage(DAY, AuctionStage::Failed).unwrap();
        contract.queue_intake_cleanup(DAY).unwrap();
        run_cleanup(s.clone()).unwrap();
        assert_eq!(contract.cleanup_pending.read(&DAY).unwrap(), 2);
        for chain in [97, 98] {
            let key = DesisContract::chain_key(DAY, chain);
            assert_eq!(contract.clearing_gas.read(&key).unwrap(), 0);
        }
    });
}
