//! Test-only controls for block execution. Production builds omit this module.
//! Runtime integration tests enable it with the `test-hooks` feature.

use alloy_primitives::B256;

use super::OutbeBlockExecutor;

thread_local! {
    pub static PHASE1_VERIFY_DISABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Disables the Phase 1 certificate preflight for the duration of `f`.
pub fn with_phase1_verify_disabled<R>(f: impl FnOnce() -> R) -> R {
    PHASE1_VERIFY_DISABLED.with(|cell| cell.set(true));
    let result = f();
    PHASE1_VERIFY_DISABLED.with(|cell| cell.set(false));
    result
}

impl<Evm> OutbeBlockExecutor<'_, Evm> {
    /// Marks Phase 1 as already executed so a detached replay can observe it.
    pub fn force_preexecuted_phase1_witness_for_test(&mut self, tx_hash: B256) {
        self.preexecuted_phase1_tx_hash = Some(tx_hash);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::*;
    use alloy_evm::{eth::EthBlockExecutionCtx, EvmEnv, EvmFactory};
    use alloy_primitives::{Bytes, U256};
    use outbe_primitives::{
        block::{BlockOrder, BlockSystemFlags},
        signer::OutbeEvmSigner,
        system_tx::{build_unsigned_system_tx, BlockExecutionFsm, BlockExecutionStage, SystemTx},
        test_utils::TEST_BLOCK_ORDER,
        OutbeHeader,
    };
    use reth_ethereum::evm::RethReceiptBuilder;
    use reth_primitives_traits::Recovered;
    use revm::{
        context::{BlockEnv, CfgEnv},
        database::{EmptyDB, State},
        inspector::NoOpInspector,
        primitives::hardfork::SpecId,
    };
    use std::sync::Arc;

    type TestEvm = <crate::OutbeEvmFactory as EvmFactory>::Evm<State<EmptyDB>, NoOpInspector>;

    fn with_executor(f: impl FnOnce(OutbeBlockExecutor<'_, TestEvm>)) {
        with_executor_at(U256::from(1), f);
    }

    fn with_executor_at(timestamp: U256, f: impl FnOnce(OutbeBlockExecutor<'_, TestEvm>)) {
        let host = crate::host::stub_host();
        let chain_spec = Arc::new(
            reth_chainspec::MAINNET
                .as_ref()
                .clone()
                .map_header(OutbeHeader::new),
        );
        let receipt_builder = RethReceiptBuilder::default();
        let signer = Arc::new(OutbeEvmSigner::from_secret_bytes([1; 32]).unwrap());
        let evm = crate::OutbeEvmFactory::new(host.clone()).create_evm(
            State::builder()
                .with_database(EmptyDB::default())
                .with_bundle_update()
                .build(),
            EvmEnv {
                cfg_env: CfgEnv::new()
                    .with_chain_id(1)
                    .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
                block_env: BlockEnv {
                    number: U256::from(2),
                    gas_limit: 30_000_000,
                    timestamp,
                    ..Default::default()
                },
            },
        );
        let inner = alloy_evm::eth::EthBlockExecutor::new(
            evm,
            EthBlockExecutionCtx {
                parent_hash: B256::ZERO,
                parent_beacon_block_root: None,
                ommers: &[],
                withdrawals: None,
                extra_data: Bytes::new(),
                tx_count_hint: None,
                slot_number: None,
            },
            &chain_spec,
            &receipt_builder,
        );
        let mut executor = OutbeBlockExecutor::new(
            inner,
            BlockExecutorInputs {
                identity: BlockExecutionIdentity {
                    block_extra_data: Bytes::new(),
                    validate_execution_summary: false,
                    block_hash: None,
                    block_state_root: None,
                    parent_hash: B256::ZERO,
                },
                system_plan: BlockSystemPlan {
                    expected_begin_system_txs: Vec::new(),
                    system_layout_error: None,
                    proposer_evm_address: Some(signer.address()),
                    execute_outbe_block_hooks: false,
                    prebuilt_phase1_tx: None,
                    pending_tee_bootstrap: None,
                },
                parent_accounting: ParentAccountingInputs {
                    accounted_parent_artifact_provider: None,
                    parent_consensus_metadata: None,
                    parent_artifact_hint: None,
                },
                dependencies: BlockExecutionDependencies {
                    bridge: None,
                    evm_signer: Some(signer),
                    runtime_host: host,
                },
            },
        );
        assert_eq!(executor.fsm.stage(), BlockExecutionStage::MidBlock);
        assert!(executor.block_order.consensus.is_empty());
        executor.block_order = TEST_BLOCK_ORDER;
        executor.fsm = BlockExecutionFsm::for_mid_block(&executor.block_order);
        f(executor);
    }

    fn signed(
        executor: &OutbeBlockExecutor<'_, TestEvm>,
        input: &SystemTx,
        ordinal: u8,
    ) -> Recovered<reth_ethereum::TransactionSigned> {
        let signer = executor.evm_signer.as_ref().unwrap();
        let tx = signer
            .sign_unsigned(
                build_unsigned_system_tx(ordinal, 2, 1, input.encode().unwrap()).unwrap(),
            )
            .unwrap();
        Recovered::new_unchecked(tx, signer.address())
    }

    #[test]
    fn preexecuted_witness_hash_survives_rejected_admission_and_is_consumed_once() {
        with_executor(|mut executor| {
            let input = SystemTx::Consensus {
                step_index: 0,
                payload: Bytes::new(),
            };
            let tx = signed(&executor, &input, 0);
            let hash = tx.signature_hash();
            executor.force_preexecuted_phase1_witness_for_test(hash);
            let before = executor.fsm.clone();
            assert!(executor
                .execute_reserved_system_tx(tx.clone(), |_| CommitChanges::Yes)
                .is_err());
            assert_eq!(executor.preexecuted_phase1_tx_hash, Some(hash));
            assert_eq!(executor.fsm, before);
            assert!(executor.receipts().is_empty());

            executor.fsm =
                BlockExecutionFsm::for_block(2, BlockSystemFlags::default(), &TEST_BLOCK_ORDER);
            let wrong = signed(
                &executor,
                &SystemTx::Consensus {
                    step_index: 1,
                    payload: Bytes::new(),
                },
                0,
            );
            let before = executor.fsm.clone();
            assert!(executor
                .execute_reserved_system_tx(wrong, |_| CommitChanges::Yes)
                .is_err());
            assert_eq!(executor.preexecuted_phase1_tx_hash, Some(hash));
            assert_eq!(executor.fsm, before);
            assert!(executor
                .execute_reserved_system_tx(tx, |_| CommitChanges::Yes)
                .unwrap()
                .is_none());
            assert_eq!(executor.preexecuted_phase1_tx_hash, None);
            assert_eq!(executor.fsm.expected_prefix_body_index(), Some(1));
            assert!(executor.receipts().is_empty());
        });
    }

    #[test]
    fn end_block_fatal_error_preserves_cursor_and_soft_failures_commit_before_advancing() {
        with_executor(|mut executor| {
            let tx = signed(&executor, &SystemTx::EndBlock { step_index: 0 }, 24);
            assert!(crate::factory::with_forced_outbe_system_call_error(
                || executor.execute_transaction(tx.clone())
            )
            .is_err());
            assert_eq!(executor.fsm.end_cursor(), 0);
            assert!(executor.receipts().is_empty());
            for index in 0..5 {
                let tx = signed(
                    &executor,
                    &SystemTx::EndBlock { step_index: index },
                    24 + index,
                );
                let gas = crate::factory::with_forced_outbe_system_call_revert(|| {
                    executor.execute_transaction(tx)
                })
                .unwrap();
                assert_eq!(gas.tx_gas_used(), 0);
                assert_eq!(executor.fsm.end_cursor(), usize::from(index) + 1);
                assert_eq!(executor.receipts().len(), usize::from(index) + 1);
                assert!(!executor.receipts().last().unwrap().success);
                assert_eq!(executor.receipts().last().unwrap().cumulative_gas_used, 0);
            }
            assert_eq!(executor.fsm.stage(), BlockExecutionStage::Completed);
        });
    }

    #[test]
    fn built_suffix_preserves_static_indices_and_nonce_ordinals_after_filtering() {
        const ORDER: BlockOrder = BlockOrder {
            consensus: &[],
            begin_block: &[],
            end_block: &[
                TEST_BLOCK_ORDER.end_block[0].active_when(|_, _| false),
                TEST_BLOCK_ORDER.end_block[1],
                TEST_BLOCK_ORDER.end_block[2].active_when(|_, _| false),
                TEST_BLOCK_ORDER.end_block[3],
            ],
        };
        with_executor(|mut executor| {
            executor.block_order = ORDER;
            executor.fsm = BlockExecutionFsm::for_block(2, BlockSystemFlags::default(), &ORDER);
            crate::factory::with_forced_outbe_system_call_revert(|| {
                executor.apply_end_block_phase()
            })
            .unwrap();
            assert_eq!(executor.fsm.stage(), BlockExecutionStage::Completed);
            assert_eq!(executor.receipts().len(), 2);
            let built = executor.take_staged_end_block_txs();
            assert_eq!(built.len(), 2);
            for (tx, index) in built.iter().zip([1, 3]) {
                assert_eq!(
                    SystemTx::decode(tx.input()).unwrap(),
                    SystemTx::EndBlock { step_index: index }
                );
                let expected =
                    build_unsigned_system_tx(24 + index, 2, 1, tx.input().clone()).unwrap();
                assert_eq!(tx.signature_hash(), expected.signature_hash());
            }
            executor.apply_end_block_phase().unwrap();
            assert_eq!(executor.receipts().len(), 2);
            assert!(executor.take_staged_end_block_txs().is_empty());
        });
    }

    #[test]
    fn proposer_and_verifier_share_prefix_plan_and_reject_mismatched_steps_and_payloads() {
        with_executor(|mut executor| {
            executor.parent_consensus_metadata = Some(
                outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata {
                    finalized_block_number: 1,
                    ..Default::default()
                },
            );
            executor.parent_artifact_hint = Some(AccountedParentArtifact {
                summary: Default::default(),
                timestamp: 1,
                state_root: None,
            });
            let artifacts = outbe_primitives::reshare_artifact::OutbeBlockArtifacts::default();
            let proposed = executor
                .begin_block_system_tx_inputs(2, &artifacts)
                .unwrap();
            assert_eq!(proposed.len(), 12);
            assert!(proposed[0].1.is_some());
            assert!(proposed[1..].iter().all(|(_, summary)| summary.is_none()));
            executor.expected_begin_system_txs = proposed
                .iter()
                .enumerate()
                .map(|(index, (input, _))| signed(&executor, input, u8::try_from(index).unwrap()))
                .collect();
            let verified = executor
                .begin_block_system_tx_inputs(2, &artifacts)
                .unwrap();
            assert_eq!(verified, proposed);
            let mut wrong = proposed[1].0.clone();
            if let SystemTx::Consensus { step_index, .. } = &mut wrong {
                *step_index = 0;
            }
            executor.expected_begin_system_txs[1] = signed(&executor, &wrong, 1);
            assert!(executor
                .begin_block_system_tx_inputs(2, &artifacts)
                .is_err());
            executor.expected_begin_system_txs[1] = signed(&executor, &proposed[1].0, 1);
            let SystemTx::Consensus { payload, .. } = &mut wrong else {
                unreachable!()
            };
            *payload = Bytes::from_static(b"invalid parent payload");
            executor.expected_begin_system_txs[0] = signed(&executor, &wrong, 0);
            assert!(executor
                .begin_block_system_tx_inputs(2, &artifacts)
                .is_err());
        });
    }

    #[test]
    fn finish_reports_timestamp_overflow_before_incomplete_fsm() {
        with_executor_at(U256::MAX, |mut executor| {
            executor.execute_outbe_block_hooks = true;
            let error = executor
                .finish()
                .err()
                .expect("overflow must fail before FSM completion");
            assert!(error.to_string().contains("timestamp"));
            assert!(!error.to_string().contains("finished in stage"));
        });
    }
}
