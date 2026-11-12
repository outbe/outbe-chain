//! Shared real-provider cleanup assertions for auction calibration tests.
use super::receipts_and_gas_support::*;
use super::*;

pub(super) fn verify_accepted_auction_cleanup(
    state: &mut CalibrationState,
    timestamp: u64,
    outcome: (outbe_desis::AuctionStage, u32, U256),
    first_logs: &[Log],
) {
    use outbe_evm::gas::SubcallGasMeter;
    use outbe_evm::storage::CtxStorageProvider;
    use revm::{context_interface::JournalTr as _, handler::MainContext as _, Context};
    let day = EmissionDate::new(20240101);
    let mut ctx = Context::mainnet().with_db(state).modify_cfg_chained(|cfg| {
        cfg.chain_id = CHAIN_ID;
        cfg.set_spec_and_mainnet_gas_params(SpecId::PRAGUE);
    });
    let (stage, bids, returned) = outcome;
    let mut previous = bids;
    let mut credit = None;
    for block in 2..=8u64 {
        ctx.block.number = U256::from(block);
        ctx.block.timestamp = U256::from(timestamp + block);
        let mut provider = CtxStorageProvider::new(
            &mut ctx,
            SubcallGasMeter::new(100_000_000),
            cleanup_storage_config(),
            std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
        );
        StorageHandle::enter(&mut provider, |storage| {
            let auction = outbe_desis::DesisContract::new(storage.clone());
            let key = outbe_desis::DesisContract::chain_key(day, 1);
            assert_eq!(auction.auction_stage.read(&day).unwrap(), stage as u8);
            let remaining = auction.chain_bid_count.read(&key).unwrap();
            assert_eq!(auction.day_bid_count.read(&day).unwrap(), 0);
            assert_eq!(auction.pending_desis_limit_units.read(&day).unwrap(), 0);
            assert_eq!(
                auction.pending_desis_limit_minor.read(&day).unwrap(),
                U256::ZERO
            );
            assert_eq!(auction.clearing_initiated.read(&day).unwrap(), 0);
            assert_eq!(auction.gate_active_count.read().unwrap(), 0);
            assert_eq!(auction.sched_active_count.read().unwrap(), 0);
            assert!(remaining < previous || remaining == 0);
            if block == 2 {
                assert_eq!(
                    remaining,
                    bids.saturating_sub(248),
                    "first Cycle page is bounded"
                );
            }
            previous = remaining;
            let promis = outbe_promislimit::PromisLimitContract::new(storage.clone());
            let balance = (promis.promis_limit().unwrap(), promis.rest_minor().unwrap());
            assert_eq!(balance, (returned, U256::ZERO));
            assert_eq!(*credit.get_or_insert(balance), balance);
            let runtime = BlockRuntimeContext::new(
                BlockContext {
                    block_gas_limit: 0,
                    user_gas_used: 0,
                    block_number: block,
                    timestamp: timestamp + block,
                    chain_id: CHAIN_ID,
                    genesis_hash: B256::ZERO,
                    genesis_timestamp: GENESIS,
                    proposer: test_evm_signer().address(),
                    validators: vec![test_evm_signer().address()],
                },
                storage.clone(),
            );
            let before = storage.gas_used().unwrap();
            outbe_cycle::runtime::handle_cycle_tick(&runtime, 1).unwrap();
            eprintln!(
                "ACCEPTED1088 cleanup Cycle block={block}, gross={}",
                storage.gas_used().unwrap() - before
            );
            if block == 8 {
                assert_eq!(auction.chain_bid_count.read(&key).unwrap(), 0);
                assert_eq!(auction.chain_done.read(&key).unwrap(), 0);
                assert_eq!(auction.chain_done_batches.read(&key).unwrap(), 0);
                assert_eq!(auction.chain_done_bids.read(&key).unwrap(), 0);
                assert_eq!(auction.chain_total_batches.read(&key).unwrap(), 0);
                assert_eq!(auction.chain_arrived_mask.read(&key).unwrap(), U256::ZERO);
                assert_eq!(auction.cleanup_pending.read(&day).unwrap(), 2);
                assert_eq!(auction.cleanup_head.read().unwrap(), 0);
                assert_eq!(auction.cleanup_tail.read().unwrap(), 0);
                assert_eq!(auction.cleanup_next.read(&day).unwrap(), 0);
                assert_eq!(auction.intake_chain_count.read(&day).unwrap(), 0);
                assert_eq!(auction.intake_chain_member.read(&key).unwrap(), 0);
                assert_eq!(
                    auction
                        .intake_chain_at
                        .read(&outbe_desis::DesisContract::chain_key(day, 0))
                        .unwrap(),
                    0
                );
                for index in 0..bids {
                    let key = outbe_desis::DesisContract::bid_key(day, 1, index);
                    assert_eq!(auction.bid_bidder.read(&key).unwrap(), Address::ZERO);
                    assert_eq!(auction.bid_packed.read(&key).unwrap(), U256::ZERO);
                }
            }
        });
    }
    assert!(first_logs.iter().any(|log| {
        outbe_desis::precompile::IDesis::IntexAuctionCompleted::decode_log(log).is_ok()
            || outbe_desis::precompile::IDesis::IntexAuctionFailed::decode_log(log).is_ok()
    }));
    assert!(!ctx.journaled_state.logs().iter().any(|log| {
        outbe_desis::precompile::IDesis::IntexAuctionCompleted::decode_log(log).is_ok()
            || outbe_desis::precompile::IDesis::IntexAuctionFailed::decode_log(log).is_ok()
            || outbe_intexfactory::precompile::IIntexFactory::IntexSeriesOpened::decode_log(log)
                .is_ok()
            || outbe_desis::precompile::IDesis::DesisLimitReturned::decode_log(log).is_ok()
    }));
}

pub(super) fn cleanup_storage_config() -> outbe_evm::storage::CtxStorageProviderConfig {
    use outbe_evm::storage::{CtxStorageProviderConfig, ReentrancyStack};
    CtxStorageProviderConfig {
        is_static: false,
        self_address: CYCLE_ADDRESS,
        reentrancy_stack: ReentrancyStack,
        spec: SpecId::PRAGUE,
        genesis_hash: B256::ZERO,
        genesis_timestamp: GENESIS,
        abort_bridge: Default::default(),
        metadosis_mutation_entitlements: Default::default(),
    }
}

pub(super) fn assert_cleanup_page(storage: StorageHandle<'_>, chains: u32) {
    let day = EmissionDate::new(20240101);

    let before = storage.gas_used().unwrap();
    outbe_desis::process_cleanup(storage.clone()).unwrap();
    let cost = storage.gas_used().unwrap() - before;
    assert!(cost <= 2_560_000);
    let auction = outbe_desis::DesisContract::new(storage);
    match chains {
        0 => {
            assert_eq!(auction.cleanup_head.read().unwrap(), 0);
            for value in 20240101..=20240104 {
                assert_eq!(
                    auction
                        .cleanup_pending
                        .read(&EmissionDate::new(value))
                        .unwrap(),
                    2
                );
            }
        }
        1 => assert_eq!(
            auction
                .chain_bid_count
                .read(&outbe_desis::DesisContract::chain_key(day, 1))
                .unwrap(),
            840
        ),
        4 => {
            assert_eq!(auction.intake_chain_count.read(&day).unwrap(), 1);
            assert_eq!(
                auction
                    .chain_bid_count
                    .read(&outbe_desis::DesisContract::chain_key(day, 1))
                    .unwrap(),
                32
            );
            for chain in 2..=4 {
                assert_eq!(
                    auction
                        .chain_total_batches
                        .read(&outbe_desis::DesisContract::chain_key(day, chain))
                        .unwrap(),
                    0
                );
            }
        }
        _ => unreachable!(),
    }
    eprintln!("DESIS GC real-provider chains={chains} gross={cost}, pagebound=2560000");
}
