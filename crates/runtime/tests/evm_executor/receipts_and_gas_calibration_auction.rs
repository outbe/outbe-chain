use super::receipts_and_gas_calibration_envelope::*;
use super::*;

#[test]
fn calibration_live_auction_start() {
    use router_abi::IOriginRouter;
    let proposer = test_evm_signer().address();
    let timestamp = GENESIS + SECONDS_PER_DAY + 60;
    let router = returning_contract(vec![
        (
            IOriginRouter::targetsOfCall::SELECTOR,
            IOriginRouter::targetsOfCall::abi_encode_returns(&(1..=16u32).collect::<Vec<_>>()),
        ),
        (
            IOriginRouter::parkedMessageCountCall::SELECTOR,
            IOriginRouter::parkedMessageCountCall::abi_encode_returns(&U256::ZERO),
        ),
        (
            IOriginRouter::parkedProceedsCountCall::SELECTOR,
            IOriginRouter::parkedProceedsCountCall::abi_encode_returns(&U256::ZERO),
        ),
    ]);
    let (_, _, logs, _) = run_prefix(
        "live priced auction start16 chains",
        &[(proposer, dummy_pubkey(0xa2))],
        (1, timestamp),
        |storage| {
            seed_cycle_tick_genesis(storage.clone(), GENESIS, proposer).unwrap();
            seed_previous_day_vwap(&storage, timestamp, U256::from(1_000_000));
            seed_previous_day_vwap(&storage, GENESIS, U256::from(1_000_000));
            outbe_oracle::schema::OracleContract::new(storage.clone())
                .utc_day_vwap_last_finalized
                .write(20240101)
                .unwrap();
            outbe_desis::api::dispatch_auction_brief(
                storage.clone(),
                outbe_desis::api::AuctionBrief {
                    emission_day: EmissionDate::new(20240101),
                    desis_limit_minor: U256::from(10_000_000_000_000u64),
                    has_limit: true,
                },
                outbe_desis::api::BriefOverflowPolicy::ReturnToPromisLimit,
            )
            .unwrap();
            for spec in outbe_cycle::triggers::ACTIVE_TRIGGERS {
                outbe_cycle::schema::Cycle::new(storage.clone())
                    .last_executed_at
                    .write(
                        &spec.id,
                        if spec.id == 0 {
                            timestamp
                        } else {
                            GENESIS + 60
                        },
                    )
                    .unwrap();
            }
        },
        vec![
            (outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS, router),
            (RELAY, Bytecode::new_legacy(bridge_work(false).into())),
        ],
        None,
        false,
    );
    assert!(logs.iter().any(
        |log| log.address == outbe_primitives::addresses::DESIS_ADDRESS
            && log.data.topics().first()
                == Some(&outbe_desis::precompile::IDesis::IntexAuctionCreated::SIGNATURE_HASH)
    ));
}

/// Intake uses the real bridge-facing native entry points before exporting the
/// fixture to EVM storage. Each batch is independently deliverable; there is no
/// single oversized transaction or repeated batch counted as new demand.
fn seed_accepted_auction(storage: StorageHandle<'_>, timestamp: u64, batches: u16) {
    let day = EmissionDate::new(20240101);
    seed_cycle_tick_genesis(storage.clone(), GENESIS, test_evm_signer().address()).unwrap();
    let auction = outbe_desis::DesisContract::new(storage.clone());
    auction
        .auction_stage
        .write(&day, outbe_desis::AuctionStage::CollectingBids as u8)
        .unwrap();
    auction
        .auction_at
        .write(&day, u32::try_from(timestamp).unwrap())
        .unwrap();
    auction.gate_active_count.write(1).unwrap();
    auction.gate_active_at.write(&0, day.value()).unwrap();
    auction.gate_active_slot.write(&day, 1).unwrap();
    auction.clearing_initiated.write(&day, 1).unwrap();
    auction
        .config_promis_load_minor
        .write(&day, U256::from(1_000_000))
        .unwrap();
    auction.config_min_bid_rate.write(&day, 1_000_000).unwrap();
    auction.config_min_bid_quantity.write(&day, 1).unwrap();
    auction.pending_desis_limit_units.write(&day, 3).unwrap();
    auction
        .pending_desis_limit_minor
        .write(&day, U256::from(3_000_007))
        .unwrap();
    auction.reference_price_count.write(&day, 1).unwrap();
    let price = outbe_desis::DesisContract::reference_price_key(day, 0);
    auction.reference_price_iso.write(&price, 840).unwrap();
    auction
        .reference_entry_price_minor
        .write(&price, U256::from(1_000_000))
        .unwrap();
    auction
        .reference_call_price_minor
        .write(&price, U256::from(2_000_000))
        .unwrap();
    accept_auction_batches(storage.clone(), day, 1, batches);
    for spec in outbe_cycle::triggers::ACTIVE_TRIGGERS {
        outbe_cycle::schema::Cycle::new(storage.clone())
            .last_executed_at
            .write(
                &spec.id,
                if spec.id == 0 {
                    timestamp
                } else {
                    GENESIS + 60
                },
            )
            .unwrap();
    }
}

fn accept_auction_batches(storage: StorageHandle<'_>, day: EmissionDate, chain: u32, batches: u16) {
    for batch in 0..batches {
        let indices = (0..64u32).map(|offset| u32::from(batch) * 64 + offset);
        let call = outbe_desis::precompile::IDesis::processBidsBatchCall {
            emissionDay: day.value(),
            srcChainId: chain,
            batchIndex: batch,
            totalBatches: batches,
            bidderAddresses: indices
                .clone()
                .map(|index| {
                    numbered_test_address(0xa3, u64::from(chain) * 16384 + u64::from(index + 1))
                })
                .collect(),
            packedBids: indices
                .map(|index| {
                    U256::from_limbs([
                        840 | (840 << 16) | (u64::from(index) << 32),
                        2_000_000 | (1 << 32),
                        0,
                        0,
                    ])
                })
                .collect(),
        };
        outbe_desis::precompile::dispatch(
            storage.clone(),
            &call.abi_encode(),
            outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS,
            U256::ZERO,
        )
        .unwrap();
    }
    let done = outbe_desis::precompile::IDesis::processBidsDoneCall {
        emissionDay: day.value(),
        srcChainId: chain,
        totalBatches: batches,
        totalBids: u32::from(batches) * 64,
    };
    outbe_desis::precompile::dispatch(
        storage.clone(),
        &done.abi_encode(),
        outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS,
        U256::ZERO,
    )
    .unwrap();
    let auction = outbe_desis::DesisContract::new(storage);
    let key = outbe_desis::DesisContract::chain_key(day, chain);
    assert_eq!(
        auction.chain_bid_count.read(&key).unwrap(),
        u32::from(batches) * 64
    );
    assert_eq!(auction.chain_done.read(&key).unwrap(), 1);
}

#[test]
fn accepted_1088_bid_auction_completes_before_incremental_cycle_cleanup() {
    use router_abi::IOriginRouter;
    let proposer = test_evm_signer().address();
    let timestamp = GENESIS + SECONDS_PER_DAY + 60;
    let router = returning_contract(vec![
        (
            IOriginRouter::targetsOfCall::SELECTOR,
            IOriginRouter::targetsOfCall::abi_encode_returns(&vec![1u32]),
        ),
        (
            IOriginRouter::parkedMessageCountCall::SELECTOR,
            IOriginRouter::parkedMessageCountCall::abi_encode_returns(&U256::ZERO),
        ),
        (
            IOriginRouter::parkedProceedsCountCall::SELECTOR,
            IOriginRouter::parkedProceedsCountCall::abi_encode_returns(&U256::ZERO),
        ),
    ]);
    let (_, samples, logs, _) = run_prefix_checked(
        "accepted1088 valid bids/1chain + incremental GC",
        &[(proposer, dummy_pubkey(0xa2))],
        (1, timestamp),
        |storage| seed_accepted_auction(storage, timestamp, 17),
        vec![
            (outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS, router),
            (RELAY, Bytecode::new_legacy(bridge_work(false).into())),
        ],
        None,
        (false, |state, logs| {
            verify_accepted_auction_cleanup(
                state,
                timestamp,
                (outbe_desis::AuctionStage::Completed, 1088, U256::from(7)),
                logs,
            )
        }),
    );
    assert!(!logs
        .iter()
        .any(|log| outbe_cycle::ICycle::CycleTriggerOverBudget::decode_log(log).is_ok()));
    assert!(logs.iter().any(|log| {
        outbe_desis::precompile::IDesis::IntexAuctionCompleted::decode_log(log)
            .is_ok_and(|event| event.data.units == 3 && event.data.totalDemand == 1088)
    }));
    assert_eq!(
        logs.iter()
            .filter(
                |log| outbe_desis::precompile::IDesis::DesisLimitReturned::decode_log(log)
                    .is_ok_and(|event| event.data.amountMinor == U256::from(7))
            )
            .count(),
        1
    );
    assert_eq!(
        logs.iter()
            .filter(|log| {
                outbe_intexfactory::precompile::IIntexFactory::IntexSeriesOpened::decode_log(log)
                    .is_ok_and(|event| event.data.units == 3)
            })
            .count(),
        1
    );
    let cost = samples.iter().find(|(id, _)| *id == 4).unwrap().1;
    assert!(cost <= 10_333_000);
    eprintln!("ACCEPTED1088 clearing gross={cost}, limit=10333000");
}

fn verify_accepted_auction_cleanup(
    state: &mut CalibrationState,
    timestamp: u64,
    outcome: (outbe_desis::AuctionStage, u32, U256),
    first_logs: &[Log],
) {
    use outbe_evm::gas::SubcallGasMeter;
    use outbe_evm::storage::{CtxStorageProvider, CtxStorageProviderConfig, ReentrancyStack};
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
            CtxStorageProviderConfig {
                is_static: false,
                self_address: CYCLE_ADDRESS,
                reentrancy_stack: ReentrancyStack,
                spec: SpecId::PRAGUE,
                genesis_hash: B256::ZERO,
                genesis_timestamp: GENESIS,
                abort_bridge: Default::default(),
                metadosis_mutation_entitlements: Default::default(),
            },
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

#[test]
fn accepted_bid_deadlines_retire_with_reverting_router_within_advance_budget() {
    let proposer = test_evm_signer().address();
    let timestamp = GENESIS + SECONDS_PER_DAY + 60;
    for batches in [1, 17] {
        let (_, samples, logs, _) = run_prefix_checked(
            &format!(
                "deadline accepted{} bids reverting router",
                u32::from(batches) * 64
            ),
            &[(proposer, dummy_pubkey(0xa2))],
            (1, timestamp),
            |storage| {
                seed_accepted_auction(storage.clone(), timestamp, batches);
                let auction = outbe_desis::DesisContract::new(storage);
                let day = EmissionDate::new(20240101);
                auction
                    .auction_at
                    .write(&day, u32::try_from(GENESIS - 2 * SECONDS_PER_DAY).unwrap())
                    .unwrap();
                auction.sched_active_count.write(1).unwrap();
                auction.sched_active_at.write(&0, day.value()).unwrap();
                auction.sched_active_slot.write(&day, 1).unwrap();
            },
            vec![(
                outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS,
                Bytecode::new_legacy(vec![0x60, 0, 0x60, 0, 0xfd].into()),
            )],
            None,
            (false, |state, logs| {
                verify_accepted_auction_cleanup(
                    state,
                    timestamp,
                    (
                        outbe_desis::AuctionStage::Failed,
                        u32::from(batches) * 64,
                        U256::from(3_000_007),
                    ),
                    logs,
                )
            }),
        );
        assert_eq!(
            logs.iter()
                .filter(
                    |log| outbe_desis::precompile::IDesis::IntexAuctionFailed::decode_log(log)
                        .is_ok()
                )
                .count(),
            1
        );
        assert_eq!(
            logs.iter()
                .filter(
                    |log| outbe_desis::precompile::IDesis::DesisLimitReturned::decode_log(log)
                        .is_ok_and(|event| event.data.amountMinor == U256::from(3_000_007))
                )
                .count(),
            1
        );
        assert!(!logs
            .iter()
            .any(|log| outbe_cycle::ICycle::CycleTriggerOverBudget::decode_log(log).is_ok()));
        let cost = samples.iter().find(|(id, _)| *id == 2).unwrap().1;
        assert!(cost <= 638_000);
        eprintln!(
            "DEADLINE bids={} advance gross={cost}, limit=638000",
            u32::from(batches) * 64
        );
    }
}

#[test]
fn calibration_desis_cleanup_max_page_and_four_headers() {
    use outbe_evm::gas::SubcallGasMeter;
    use outbe_evm::storage::{CtxStorageProvider, CtxStorageProviderConfig, ReentrancyStack};
    use revm::{handler::MainContext as _, Context};
    let proposer = test_evm_signer().address();
    let day = EmissionDate::new(20240101);
    for chains in [0, 1, 4] {
        let mut state =
            state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xa2))], |storage| {
                if chains == 0 {
                    for value in 20240101..=20240104 {
                        let day = EmissionDate::new(value);
                        let auction = outbe_desis::DesisContract::new(storage.clone());
                        auction
                            .auction_stage
                            .write(&day, outbe_desis::AuctionStage::Completed as u8)
                            .unwrap();
                        auction.queue_intake_cleanup(day).unwrap();
                    }
                } else {
                    queue_accepted_cleanup(storage, chains);
                }
            });
        let mut ctx = Context::mainnet()
            .with_db(&mut state)
            .modify_cfg_chained(|cfg| {
                cfg.chain_id = CHAIN_ID;
                cfg.set_spec_and_mainnet_gas_params(SpecId::PRAGUE);
            });
        let mut provider = CtxStorageProvider::new(
            &mut ctx,
            SubcallGasMeter::new(10_000_000),
            CtxStorageProviderConfig {
                is_static: false,
                self_address: CYCLE_ADDRESS,
                reentrancy_stack: ReentrancyStack,
                spec: SpecId::PRAGUE,
                genesis_hash: B256::ZERO,
                genesis_timestamp: GENESIS,
                abort_bridge: Default::default(),
                metadosis_mutation_entitlements: Default::default(),
            },
            std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
        );
        StorageHandle::enter(&mut provider, |storage| {
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
        });
    }
}

pub(crate) fn queue_accepted_cleanup(storage: StorageHandle<'_>, chains: u32) {
    let day = EmissionDate::new(20240101);
    let auction = outbe_desis::DesisContract::new(storage.clone());
    auction
        .auction_stage
        .write(&day, outbe_desis::AuctionStage::CollectingBids as u8)
        .unwrap();
    for chain in 1..=chains {
        accept_auction_batches(
            storage.clone(),
            day,
            chain,
            if chains == 1 { 17 } else { 1 },
        );
    }
    auction
        .auction_stage
        .write(&day, outbe_desis::AuctionStage::Completed as u8)
        .unwrap();
    auction.queue_intake_cleanup(day).unwrap();
}
