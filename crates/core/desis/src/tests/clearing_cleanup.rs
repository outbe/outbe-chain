#![cfg(not(feature = "e2e-test"))]

use super::*;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};

#[test]
fn cleanup_progress_does_not_change_the_next_auctions_gas_quote() {
    with_storage(|s| {
        open_clearing(&s, 100);
        relay_bids(&s, SRC_CHAIN, 100, 200);
        clear(&s);
        let before = runtime::clearing_round_gas(&s, NEXT_EMISSION_DAY, SRC_CHAIN).unwrap();
        assert!(holds_bids(&s, SRC_CHAIN, 100));
        crate::process_cleanup(s.clone()).unwrap();
        assert!(!holds_bids(&s, SRC_CHAIN, 100));
        assert_eq!(
            runtime::clearing_round_gas(&s, NEXT_EMISSION_DAY, SRC_CHAIN).unwrap(),
            before
        );
    });
}

fn poll(s: &StorageHandle<'_>) {
    let ctx = BlockRuntimeContext::new(
        BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: 1,
            timestamp: NOW,
            chain_id: CHAIN_ID,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: NOW,
            proposer: Address::ZERO,
            validators: Vec::new(),
        },
        s.clone(),
    );
    runtime::complete_auctions(&ctx).unwrap();
}

#[test]
fn a_waiting_gate_does_not_starve_another_ready_auction() {
    with_storage(|s| {
        open_clearing(&s, 100);
        let mut contract = s.contract::<DesisContract>();
        let config = contract.read_auction_config(EMISSION_DAY).unwrap();
        contract
            .write_auction_config(NEXT_EMISSION_DAY, &config)
            .unwrap();
        contract
            .config_promis_load_minor
            .write(&NEXT_EMISSION_DAY, U256::from(LOAD_MINOR))
            .unwrap();
        contract
            .auction_at
            .write(&NEXT_EMISSION_DAY, u32::try_from(ANCHOR).unwrap())
            .unwrap();
        contract
            .write_stage(NEXT_EMISSION_DAY, AuctionStage::CollectingBids)
            .unwrap();
        contract
            .clearing_initiated
            .write(&NEXT_EMISSION_DAY, 1)
            .unwrap();
        contract
            .clearing_deadline
            .write(&NEXT_EMISSION_DAY, NOW + 100)
            .unwrap();
        contract
            .chain_done
            .write(&DesisContract::chain_key(NEXT_EMISSION_DAY, SRC_CHAIN), 1)
            .unwrap();
        contract.push_gate_active(NEXT_EMISSION_DAY).unwrap();

        poll(&s);
        assert_eq!(
            contract.read_stage(EMISSION_DAY).unwrap(),
            AuctionStage::CollectingBids
        );
        assert_eq!(
            contract.read_stage(NEXT_EMISSION_DAY).unwrap(),
            AuctionStage::CollectingBids
        );
        assert_eq!(contract.gate_poll_cursor.read().unwrap(), 1);

        poll(&s);
        assert_eq!(
            contract.read_stage(NEXT_EMISSION_DAY).unwrap(),
            AuctionStage::Completed
        );
        assert_eq!(
            contract.read_stage(EMISSION_DAY).unwrap(),
            AuctionStage::CollectingBids
        );
        assert_eq!(contract.gate_active_count.read().unwrap(), 1);
        assert_eq!(contract.gate_poll_cursor.read().unwrap(), 0);
    });
}
