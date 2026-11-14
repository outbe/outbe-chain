use super::receipts_and_gas_calibration_cleanup::*;
use super::receipts_and_gas_support::*;
use super::*;

#[test]
fn calibration_populated_auction_handlers() {
    let (_, samples, logs, _) = populated_auction(16);
    assert_eq!(samples.len(), 7);
    assert!(logs.iter().any(|log| log.data.topics().first()
        == Some(&keccak256("IntexAuctionCompletedEmpty(uint32,uint64)"))));
    // The Cycle does not push parked router work. Any address pushes it.
    assert!(!logs.iter().any(
        |log| outbe_cycle::ICycle::CycleTaskExecuted::decode_log(log)
            .is_ok_and(|event| event.data.id == 8)
    ));
}

#[test]
fn oversized_clearing_defers_atomically_and_deterministically_without_blocking_user() {
    let first = populated_auction(128);
    let second = populated_auction(128);
    assert_eq!(
        first, second,
        "same prestate must produce identical gross gas, logs and state root"
    );
    assert!(first.3.is_some());
}

fn populated_auction(chain_count: u32) -> Calibration {
    use router_abi::IOriginRouter;
    let targets: Vec<u32> = (1..=chain_count).collect();
    let router = returning_contract(vec![(
        IOriginRouter::targetsOfCall::SELECTOR,
        IOriginRouter::targetsOfCall::abi_encode_returns(&targets),
    )]);
    let proposer = test_evm_signer().address();
    let timestamp = GENESIS + SECONDS_PER_DAY + 60;
    run_prefix(
        (
            &format!(
                "populated auction{} bids/{chain_count} chains",
                chain_count * 2
            ),
            &[(proposer, dummy_pubkey(0xa2))],
            (if chain_count > 16 { 2 } else { 1 }, timestamp),
        ),
        |storage| {
            seed_cycle_tick_genesis(storage.clone(), GENESIS, proposer).unwrap();
            let day = EmissionDate::new(20240101);
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
            auction.config_min_bid_rate.write(&day, 2_000_000).unwrap();
            for chain in 1..=chain_count {
                let key = outbe_desis::DesisContract::chain_key(day, chain);
                auction.chain_done.write(&key, 1).unwrap();
                auction.chain_bid_count.write(&key, 2).unwrap();
                for index in 0..2 {
                    let key = outbe_desis::DesisContract::bid_key(day, chain, index);
                    auction
                        .bid_bidder
                        .write(
                            &key,
                            numbered_test_address(0xa1, u64::from(chain * 2 + index)),
                        )
                        .unwrap();
                    let packed = U256::from(1_000_000u64)
                        | (U256::from((840u64 << 48) | (1 << 32)) << 64)
                        | (U256::from(840u64) << 128);
                    auction.bid_packed.write(&key, packed).unwrap();
                }
            }
            auction.day_bid_count.write(&day, chain_count * 2).unwrap();
            outbe_desis::api::brief_auction(
                storage.clone(),
                outbe_desis::api::AuctionBrief {
                    emission_day: EmissionDate::new(20240102),
                    desis_limit_minor: U256::ZERO,
                    has_limit: false,
                },
            )
            .unwrap();
            seed_previous_day_vwap(&storage, timestamp, U256::from(1_000_000));
            outbe_oracle::schema::OracleContract::new(storage.clone())
                .utc_day_vwap_last_finalized
                .write(20240101)
                .unwrap();
            for spec in outbe_cycle::tasks::ACTIVE_TASKS {
                outbe_cycle::schema::Cycle::new(storage.clone())
                    .last_executed_at
                    .write(&spec.id, GENESIS + 60)
                    .unwrap();
            }
        },
        vec![
            (outbe_core_common::addresses::ORIGIN_ROUTER_ADDRESS, router),
            (RELAY, Bytecode::new_legacy(bridge_work(false).into())),
        ],
        None,
        chain_count > 16,
    )
}

#[test]
fn calibration_live_auction_start() {
    use router_abi::IOriginRouter;
    let proposer = test_evm_signer().address();
    let timestamp = GENESIS + SECONDS_PER_DAY + 60;
    let router = returning_contract(vec![(
        IOriginRouter::targetsOfCall::SELECTOR,
        IOriginRouter::targetsOfCall::abi_encode_returns(&(1..=16u32).collect::<Vec<_>>()),
    )]);
    let (_, _, logs, _) = run_prefix(
        (
            "live priced auction start16 chains",
            &[(proposer, dummy_pubkey(0xa2))],
            (1, timestamp),
        ),
        |storage| {
            seed_cycle_tick_genesis(storage.clone(), GENESIS, proposer).unwrap();
            seed_previous_day_vwap(&storage, timestamp, U256::from(1_000_000));
            seed_previous_day_vwap(&storage, GENESIS, U256::from(1_000_000));
            outbe_oracle::schema::OracleContract::new(storage.clone())
                .utc_day_vwap_last_finalized
                .write(20240101)
                .unwrap();
            outbe_desis::api::brief_auction(
                storage.clone(),
                outbe_desis::api::AuctionBrief {
                    emission_day: EmissionDate::new(20240101),
                    desis_limit_minor: U256::from(10_000_000_000_000u64),
                    has_limit: true,
                },
            )
            .unwrap();
            for spec in outbe_cycle::tasks::ACTIVE_TASKS {
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
            (outbe_core_common::addresses::ORIGIN_ROUTER_ADDRESS, router),
            (RELAY, Bytecode::new_legacy(bridge_work(false).into())),
        ],
        None,
        false,
    );
    assert!(logs.iter().any(
        |log| log.address == outbe_system_common::addresses::DESIS_ADDRESS
            && log.data.topics().first()
                == Some(&outbe_desis::precompile::IDesis::IntexAuctionCreated::SIGNATURE_HASH)
    ));
}

/// A scheduled day at 00:00, in the step that the `advance_auctions` run makes for it.
#[derive(Clone, Copy, Debug)]
enum DueStep {
    /// A briefed day starts and sends START to every target chain.
    Start,
    /// A day at its commit end moves to `RevealingBids`.
    CommitEnd,
    /// A day at its reveal end arms the clearing and sends COLLECTING_BIDS to every target chain.
    RevealEnd,
    /// A day that waits for its bids. Nothing is due.
    Waiting,
}

impl DueStep {
    const ALL: [Self; 4] = [Self::Start, Self::CommitEnd, Self::RevealEnd, Self::Waiting];

    fn day(self) -> EmissionDate {
        EmissionDate::new(match self {
            Self::Start => 20240101,
            Self::CommitEnd => 20231231,
            Self::RevealEnd => 20231230,
            Self::Waiting => 20231229,
        })
    }

    /// The stage after one run.
    fn advanced(self) -> outbe_desis::AuctionStage {
        match self {
            Self::Start => outbe_desis::AuctionStage::CommittingBids,
            Self::CommitEnd => outbe_desis::AuctionStage::RevealingBids,
            Self::RevealEnd | Self::Waiting => outbe_desis::AuctionStage::CollectingBids,
        }
    }

    /// Put a started day in the schedule. The brief schedules `Start` itself.
    fn schedule(self, storage: &StorageHandle<'_>, midnight: u64) {
        let (stage, anchor) = match self {
            Self::Start => return,
            Self::CommitEnd => (
                outbe_desis::AuctionStage::CommittingBids,
                midnight - SECONDS_PER_DAY,
            ),
            Self::RevealEnd => (
                outbe_desis::AuctionStage::RevealingBids,
                midnight - 2 * SECONDS_PER_DAY,
            ),
            // Started with `RevealEnd`, so it is still inside its last clearing time.
            Self::Waiting => (
                outbe_desis::AuctionStage::CollectingBids,
                midnight - 2 * SECONDS_PER_DAY,
            ),
        };
        let day = self.day();
        let auction = outbe_desis::DesisContract::new(storage.clone());
        auction.auction_stage.write(&day, stage as u8).unwrap();
        auction
            .auction_at
            .write(&day, u32::try_from(anchor).unwrap())
            .unwrap();
        auction
            .config_promis_load_minor
            .write(&day, U256::from(1_000_000))
            .unwrap();
        auction
            .pending_desis_limit_minor
            .write(&day, U256::from(10_000_000_000_000u64))
            .unwrap();
        let count = auction.sched_active_count.read().unwrap();
        auction.sched_active_at.write(&count, day.value()).unwrap();
        auction.sched_active_slot.write(&day, count + 1).unwrap();
        auction.sched_active_count.write(count + 1).unwrap();
    }
}

fn max_targets() -> Vec<u32> {
    (1..=outbe_desis::constants::MAX_TARGET_CHAINS as u32).collect()
}

/// One `advance_auctions` run at 00:00 over `steps`, with the most target chains Desis takes.
/// Returns the gross gas of the run and the logs of the block.
fn due_auctions(steps: &[DueStep]) -> (u64, Vec<Log>) {
    use router_abi::IOriginRouter;
    let proposer = test_evm_signer().address();
    let midnight = GENESIS + SECONDS_PER_DAY;
    let timestamp = midnight + 60;
    let router = returning_contract(vec![(
        IOriginRouter::targetsOfCall::SELECTOR,
        IOriginRouter::targetsOfCall::abi_encode_returns(&max_targets()),
    )]);
    let (_, samples, logs, _) = run_prefix_checked(
        (
            &format!("due auctions {steps:?} {} chains", max_targets().len()),
            &[(proposer, dummy_pubkey(0xa2))],
            (1, timestamp),
        ),
        |storage| {
            seed_cycle_tick_genesis(storage.clone(), GENESIS, proposer).unwrap();
            seed_previous_day_vwap(&storage, timestamp, U256::from(1_000_000));
            seed_previous_day_vwap(&storage, GENESIS, U256::from(1_000_000));
            outbe_oracle::schema::OracleContract::new(storage.clone())
                .utc_day_vwap_last_finalized
                .write(20240101)
                .unwrap();
            for step in steps {
                if matches!(step, DueStep::Start) {
                    outbe_desis::api::brief_auction(
                        storage.clone(),
                        outbe_desis::api::AuctionBrief {
                            emission_day: step.day(),
                            desis_limit_minor: U256::from(10_000_000_000_000u64),
                            has_limit: true,
                        },
                    )
                    .unwrap();
                }
                step.schedule(&storage, midnight);
            }
            for spec in outbe_cycle::tasks::ACTIVE_TASKS {
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
            (outbe_core_common::addresses::ORIGIN_ROUTER_ADDRESS, router),
            (RELAY, Bytecode::new_legacy(bridge_work(false).into())),
        ],
        None,
        (false, |state: &mut CalibrationState, _: &[Log]| {
            let read_ctx = BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 1,
                timestamp,
                chain_id: CHAIN_ID,
                genesis_hash: B256::ZERO,
                genesis_timestamp: GENESIS,
                proposer,
                validators: vec![proposer],
            };
            let mut provider =
                outbe_primitives::storage::direct::DirectStorageProvider::new(state, read_ctx);
            StorageHandle::enter(&mut provider, |storage| {
                let auction = outbe_desis::DesisContract::new(storage);
                for step in steps {
                    assert_eq!(
                        auction.auction_stage.read(&step.day()).unwrap(),
                        step.advanced() as u8,
                        "{step:?} advances in the same run"
                    );
                }
            });
        }),
    );
    assert!(!logs
        .iter()
        .any(|log| outbe_cycle::ICycle::CycleTaskOverBudget::decode_log(log).is_ok()));
    let gross = samples
        .iter()
        .find(|(id, _)| *id == outbe_cycle::tasks::TaskId::AdvanceAuctions.as_u32())
        .unwrap()
        .1;
    eprintln!("DUE AUCTIONS {steps:?}: advance gross={gross}");
    (gross, logs)
}

/// The steps that send nothing stay inside the Desis base bound, and one run moves all four days
/// that are due at 00:00 inside the `advance_auctions` budget.
#[test]
fn calibration_four_due_auctions_at_midnight() {
    use outbe_desis::constants::ADVANCE_DAY_BASE_GAS;
    for (step, bound) in [
        (DueStep::CommitEnd, 2 * ADVANCE_DAY_BASE_GAS),
        (DueStep::Waiting, ADVANCE_DAY_BASE_GAS),
    ] {
        let (gross, _) = due_auctions(&[step]);
        assert!(gross <= bound, "{step:?} gross={gross}, bound={bound}");
    }
    let (gross, logs) = due_auctions(&DueStep::ALL);
    let budget = outbe_cycle::tasks::ACTIVE_TASKS
        .iter()
        .find(|spec| spec.id == outbe_cycle::tasks::TaskId::AdvanceAuctions.as_u32())
        .unwrap()
        .work_budget_gas();
    assert!(gross <= budget, "gross={gross}, budget={budget}");
    let created = logs
        .iter()
        .filter(|log| {
            outbe_desis::precompile::IDesis::IntexAuctionCreated::decode_log(log)
                .is_ok_and(|event| event.data.emissionDay == DueStep::Start.day().value())
        })
        .count();
    assert_eq!(created, 1, "START goes out in the run of the brief");
    let collecting = router_abi::IOriginRouter::sendAuctionStageCollectingBidsCall::SELECTOR;
    assert_eq!(
        logs.iter()
            .filter(
                |log| log.address == outbe_core_common::addresses::ORIGIN_ROUTER_ADDRESS
                    && log.data.data.starts_with(&collecting)
            )
            .count(),
        outbe_desis::constants::MAX_TARGET_CHAINS
    );
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
    for spec in outbe_cycle::tasks::ACTIVE_TASKS {
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

#[test]
fn accepted_1088_bid_auction_completes_before_incremental_cycle_cleanup() {
    use router_abi::IOriginRouter;
    let proposer = test_evm_signer().address();
    let timestamp = GENESIS + SECONDS_PER_DAY + 60;
    let router = returning_contract(vec![(
        IOriginRouter::targetsOfCall::SELECTOR,
        IOriginRouter::targetsOfCall::abi_encode_returns(&vec![1u32]),
    )]);
    let (_, samples, logs, _) = run_prefix_checked(
        (
            "accepted1088 valid bids/1chain + incremental GC",
            &[(proposer, dummy_pubkey(0xa2))],
            (1, timestamp),
        ),
        |storage| seed_accepted_auction(storage, timestamp, 17),
        vec![
            (outbe_core_common::addresses::ORIGIN_ROUTER_ADDRESS, router),
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
        .any(|log| outbe_cycle::ICycle::CycleTaskOverBudget::decode_log(log).is_ok()));
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

#[test]
fn accepted_bid_deadlines_retire_with_reverting_router_within_advance_budget() {
    let proposer = test_evm_signer().address();
    let timestamp = GENESIS + SECONDS_PER_DAY + 60;
    for batches in [1, 17] {
        let (_, samples, logs, _) = run_prefix_checked(
            (
                &format!(
                    "deadline accepted{} bids reverting router",
                    u32::from(batches) * 64
                ),
                &[(proposer, dummy_pubkey(0xa2))],
                (1, timestamp),
            ),
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
                outbe_core_common::addresses::ORIGIN_ROUTER_ADDRESS,
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
            .any(|log| outbe_cycle::ICycle::CycleTaskOverBudget::decode_log(log).is_ok()));
        let cost = samples.iter().find(|(id, _)| *id == 2).unwrap().1;
        let bound =
            outbe_desis::constants::ADVANCE_DAY_BASE_GAS + outbe_desis::constants::ADVANCE_FAIL_GAS;
        assert!(cost <= bound);
        eprintln!(
            "DEADLINE bids={} advance gross={cost}, bound={bound}",
            u32::from(batches) * 64
        );
    }
}

#[test]
fn calibration_desis_cleanup_max_page_and_four_headers() {
    use outbe_evm::gas::SubcallGasMeter;
    use outbe_evm::storage::CtxStorageProvider;
    use revm::{handler::MainContext as _, Context};
    let proposer = test_evm_signer().address();
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
            cleanup_storage_config(),
            std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
        );
        StorageHandle::enter(&mut provider, |storage| {
            assert_cleanup_page(storage, chains)
        });
    }
}
