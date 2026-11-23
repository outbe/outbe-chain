use super::*;
use outbe_desis::{AuctionStage, DesisContract};
use outbe_system_common::time::EmissionDate;

#[test]
fn cycle_worker_failure_is_a_success_receipt_but_corrupt_cleanup_rejects_block() {
    let day = EmissionDate::new(20_231_113);
    for corrupt in [false, true] {
        let config = config(Failure::None);
        let mut state = seeded(|storage| {
            let agent = outbe_agentreward::AgentRewardContract::new(storage.clone());
            // A complete four-descriptor range with a missing head is reachable AccrualJobMissing.
            agent.accrual_tail.write(4).unwrap();
            let desis = DesisContract::new(storage);
            if corrupt {
                desis.cleanup_tail.write(20_240_101).unwrap();
            } else {
                desis
                    .auction_stage
                    .write(&day, AuctionStage::Completed as u8)
                    .unwrap();
                desis.queue_intake_cleanup(day).unwrap();
            }
        });
        let (ctx, begin) = context(&config);
        let evm = config.evm_with_env(&mut state, test_evm_env(BLOCK, REWARDS_ADDRESS));
        let mut executor = config.create_executor(evm, ctx);
        with_phase1_verify_disabled(|| executor.apply_pre_execution_changes()).unwrap();
        let read_ctx = executor.block_context(
            BLOCK,
            TEST_BLOCK_TIMESTAMP_BASE + BLOCK,
            test_evm_signer().address(),
        );
        for tx in begin {
            let cycle = outbe_evm::EvmRuntimeHost::is_protocol_cycle_call(
                &outbe_runtime::OutbeRuntime::new(),
                OUTBE_SYSTEM_TX_ADDRESS,
                outbe_system_common::addresses::SYSTEM_ADDRESS,
                tx.tx().input(),
            );
            let before = executor.receipts().len();
            let outcome = executor.execute_transaction(tx);
            if cycle && corrupt {
                let error = outcome.expect_err("InvalidCleanupState must reject block");
                assert!(
                    error.to_string().contains("invalid intake cleanup state"),
                    "{error}"
                );
                assert_eq!(executor.receipts().len(), before);
                break;
            }
            outcome.unwrap();
            if cycle {
                let receipt = &executor.receipts()[before];
                assert!(receipt.success);
                assert_eq!(receipt.cumulative_gas_used, 0);
                let failures: Vec<_> = receipt
                    .logs
                    .iter()
                    .filter_map(|log| outbe_cycle::ICycle::CycleWorkerFailed::decode_log(log).ok())
                    .collect();
                assert_eq!(failures.len(), 1);
                assert_eq!(failures[0].address, CYCLE_ADDRESS);
                assert_eq!(failures[0].data.workerId, 1);
                assert_eq!(failures[0].data.blockNumber, BLOCK);
            }
        }
        if !corrupt {
            executor.apply_end_block_phase().unwrap();
            finalize_executor_header_artifacts(&mut executor, 0).unwrap();
            let output = executor.finish().unwrap();
            state.commit_source(&BundleSource(&output.state));
        } else {
            drop(executor);
        }
        let mut provider = DirectStorageProvider::new(&mut state, read_ctx);
        StorageHandle::enter(&mut provider, |storage| {
            let agent = outbe_agentreward::AgentRewardContract::new(storage.clone());
            assert_eq!(agent.accrual_head.read().unwrap(), 0);
            assert_eq!(agent.worker_height.read().unwrap(), 0);
            assert!(!agent.worker_initialized.read().unwrap());
            let desis = DesisContract::new(storage);
            assert_eq!(desis.cleanup_head.read().unwrap(), 0);
            if corrupt {
                assert_eq!(desis.cleanup_tail.read().unwrap(), 20_240_101);
            } else {
                assert_eq!(desis.cleanup_tail.read().unwrap(), 0);
                assert_eq!(desis.cleanup_pending.read(&day).unwrap(), 2);
            }
        });
    }
}
