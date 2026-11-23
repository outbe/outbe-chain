use super::*;
use outbe_primitives::zero_fee_types::ZeroFeeCandidate;
use reth_evm::InvalidTxError;
use std::any::Any;

/// Record one zero-fee soft-failure accepted within the block budget.
pub fn record_zero_fee_soft_failure_metric() {
    metrics::counter!("outbe_evm_zero_fee_soft_failures_total").increment(1);
}

/// Record one zero-fee soft-failure rejected by the block cap.
pub fn record_zero_fee_cap_exceeded_metric() {
    metrics::counter!("outbe_evm_zero_fee_cap_exceeded_total").increment(1);
}

/// Exact transaction-validation reason used to defer sponsorship to a later block.
pub const SPONSORED_BLOCK_BUDGET_EXCEEDED: &str = "sponsored block budget exceeded";

/// Exact transaction-validation reason used to defer a transaction whose enclave witness entries
/// would exceed the current block's witness size budget.
pub const ENCLAVE_WITNESS_BUDGET_EXCEEDED: &str = "enclave witness block budget exceeded";

/// Typed Outbe transaction validation error for block-level deferrals and zero-fee rejections.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutbeInvalidTxError {
    #[error("{SPONSORED_BLOCK_BUDGET_EXCEEDED}")]
    SponsoredBlockBudgetExceeded,
    #[error("{ENCLAVE_WITNESS_BUDGET_EXCEEDED}")]
    EnclaveWitnessBudgetExceeded,
    #[error(
        "zero-fee soft-failure cap ({cap}) exceeded for this block; tx rejected to bound block stuffing"
    )]
    ZeroFeeSoftFailureCapExceeded { cap: u32 },
    #[error("enclave returned an unsigned error; transaction cannot be included")]
    UnsignedEnclaveError,
}

impl InvalidTxError for OutbeInvalidTxError {
    fn as_any(&self) -> &(dyn Any + 'static) {
        self
    }
}

/// Rejects inclusion without consuming nonce, daily quota, or a soft-failure slot.
pub fn sponsored_block_budget_deferral(tx_hash: B256) -> BlockExecutionError {
    BlockExecutionError::Validation(BlockValidationError::InvalidTx {
        hash: tx_hash,
        error: Box::new(OutbeInvalidTxError::SponsoredBlockBudgetExceeded),
    })
}

/// Identifies only the block-local sponsorship deferral, not other invalid transactions.
pub fn is_sponsored_block_budget_deferral(error: &dyn InvalidTxError) -> bool {
    matches!(
        error.as_any().downcast_ref::<OutbeInvalidTxError>(),
        Some(OutbeInvalidTxError::SponsoredBlockBudgetExceeded)
    )
}

/// Rejects inclusion into the current block without evicting the transaction from the pool.
pub fn enclave_witness_budget_deferral(tx_hash: B256) -> BlockExecutionError {
    BlockExecutionError::Validation(BlockValidationError::InvalidTx {
        hash: tx_hash,
        error: Box::new(OutbeInvalidTxError::EnclaveWitnessBudgetExceeded),
    })
}

/// Identifies only the block-local enclave witness budget deferral.
pub fn is_enclave_witness_budget_deferral(error: &dyn InvalidTxError) -> bool {
    matches!(
        error.as_any().downcast_ref::<OutbeInvalidTxError>(),
        Some(OutbeInvalidTxError::EnclaveWitnessBudgetExceeded)
    )
}

#[derive(Debug, Clone, Copy)]
pub struct ZeroFeeCfgSnapshot {
    balance_check: bool,
    base_fee_check: bool,
    fee_charge: bool,
}

pub trait ZeroFeeCfgAccess {
    fn enable_zero_fee_overrides(&mut self) -> ZeroFeeCfgSnapshot;
    fn restore_zero_fee_overrides(&mut self, snapshot: ZeroFeeCfgSnapshot);
    fn set_executor_owned(&mut self, _owned: bool) {}
    fn set_witness_state(
        &mut self,
        _state: Option<Arc<std::sync::Mutex<outbe_tee::witness::WitnessState>>>,
    ) {
    }
}

impl ZeroFeeCfgAccess for OutbeEvm<'_> {
    fn enable_zero_fee_overrides(&mut self) -> ZeroFeeCfgSnapshot {
        let mut version = *self.version();
        let snapshot = ZeroFeeCfgSnapshot {
            balance_check: version.features.contains(EvmFeatures::BALANCE_CHECK),
            base_fee_check: version.features.contains(EvmFeatures::BASE_FEE_CHECK),
            fee_charge: version.features.contains(EvmFeatures::FEE_CHARGE),
        };
        version.features.remove(
            EvmFeatures::BALANCE_CHECK | EvmFeatures::BASE_FEE_CHECK | EvmFeatures::FEE_CHARGE,
        );
        let config = evm2::ExecutionConfig::for_spec_and_version(self.spec_id(), version);
        let block = *self.block_env();
        self.set_block_and_execution_config_within_spec(block, config);
        snapshot
    }

    fn restore_zero_fee_overrides(&mut self, snapshot: ZeroFeeCfgSnapshot) {
        let mut version = *self.version();
        version
            .features
            .set(EvmFeatures::BALANCE_CHECK, snapshot.balance_check);
        version
            .features
            .set(EvmFeatures::BASE_FEE_CHECK, snapshot.base_fee_check);
        version
            .features
            .set(EvmFeatures::FEE_CHARGE, snapshot.fee_charge);
        let config = evm2::ExecutionConfig::for_spec_and_version(self.spec_id(), version);
        let block = *self.block_env();
        self.set_block_and_execution_config_within_spec(block, config);
    }

    fn set_executor_owned(&mut self, owned: bool) {
        self.ext_mut().set_owned_by_executor(owned);
    }

    fn set_witness_state(
        &mut self,
        state: Option<Arc<std::sync::Mutex<outbe_tee::witness::WitnessState>>>,
    ) {
        self.ext_mut().set_attached_witness_state(state);
    }
}

pub fn zero_fee_transaction<'a, T>(tx: &'a T, signer: Address) -> ZeroFeeTransaction<'a>
where
    T: alloy_consensus::Transaction + ?Sized,
{
    ZeroFeeTransaction::from_transaction(tx, signer)
}

impl OutbeBlockExecutor<'_> {
    pub fn sponsored_gas_fits(&self, gas_limit: u64) -> bool {
        !self
            .runtime_host
            .user_fee_policy()
            .sponsored_gas_exceeds_block_budget(self.sponsored_gas_used, gas_limit)
    }

    /// Called only after inclusion of a transaction whose gas limit passed the fit check.
    pub fn charge_sponsored_gas(&mut self, gas_limit: u64) {
        self.sponsored_gas_used += gas_limit;
    }

    /// Intrinsic gas accounted on the synthetic receipt that replaces a
    /// hard `BlockExecutionError` when the executor rejects a user transaction
    /// outside the EVM (zero-fee policy).
    pub const SOFT_FAILURE_GAS: u64 = 21_000;

    /// Maximum number of zero-fee soft-failure receipts a single block may carry.
    const MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK: u32 = 64;

    /// Account for one zero-fee soft-failure and enforce the global and per-hook block caps,
    /// checking both before mutating either counter.
    pub fn record_zero_fee_soft_failure(
        &mut self,
        tx_hash: B256,
        candidate: Option<ZeroFeeCandidate>,
    ) -> Result<(), BlockExecutionError> {
        let exceeded_cap =
            if self.zero_fee_soft_failures >= Self::MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK {
                Some(Self::MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK)
            } else {
                candidate.and_then(|cand| {
                    cand.max_soft_failures_per_block().filter(|cap| {
                        self.zero_fee_hook_soft_failures
                            .get(&cand.hook.id())
                            .copied()
                            .unwrap_or(0)
                            >= *cap
                    })
                })
            };
        if let Some(cap) = exceeded_cap {
            record_zero_fee_cap_exceeded_metric();
            return Err(BlockExecutionError::Validation(
                BlockValidationError::InvalidTx {
                    hash: tx_hash,
                    error: Box::new(OutbeInvalidTxError::ZeroFeeSoftFailureCapExceeded { cap }),
                },
            ));
        }
        self.zero_fee_soft_failures += 1;
        if let Some(cand) = candidate {
            *self
                .zero_fee_hook_soft_failures
                .entry(cand.hook.id())
                .or_default() += 1;
        }
        record_zero_fee_soft_failure_metric();
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use evm2::{env::BlockEnvExt, evm::EmptyDB, SpecId};
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use outbe_primitives::zero_fee_types::{ZeroFeeCandidate, ZeroFeeHookTag};
    use reth_ethereum::evm::{EthBlockExecutionCtx, EthEvmEnv};

    #[test]
    fn zero_fee_soft_failure_metrics_respect_block_cap() {
        for (try_over_cap, expected_cap_exceeded) in [(false, 0), (true, 1)] {
            let recorder = DebuggingRecorder::new();
            let snapshotter = recorder.snapshotter();
            metrics::with_local_recorder(&recorder, || {
                with_executor(|mut executor| {
                    for _ in 0..64 {
                        executor
                            .record_zero_fee_soft_failure(B256::ZERO, None)
                            .expect("soft-failure within budget");
                    }
                    if try_over_cap {
                        let hash = B256::repeat_byte(1);
                        assert!(matches!(
                            executor.record_zero_fee_soft_failure(hash, None),
                            Err(BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                                hash: rejected_hash, ..
                            })) if rejected_hash == hash
                        ));
                    }
                    assert_eq!(executor.zero_fee_soft_failures, 64);
                });
            });
            let snapshot = snapshotter.snapshot().into_vec();
            for (name, expected) in [
                ("outbe_evm_zero_fee_soft_failures_total", 64),
                (
                    "outbe_evm_zero_fee_cap_exceeded_total",
                    expected_cap_exceeded,
                ),
            ] {
                let value = snapshot
                    .iter()
                    .find(|(key, _, _, _)| key.key().name() == name)
                    .map(|(_, _, _, value)| match value {
                        DebugValue::Counter(count) => *count,
                        other => panic!("expected counter {name}, got {other:?}"),
                    })
                    .unwrap_or(0);
                assert_eq!(value, expected, "{name}");
            }
        }
    }

    #[test]
    fn zero_fee_hook_caps_are_atomic_and_isolated() {
        with_executor(|mut executor| {
            let hook = ZeroFeeHookTag::new(3, "SlashEquivocation", Some(1))
                .with_max_soft_failures_per_block(4);
            let candidate = ZeroFeeCandidate::new(hook, Address::ZERO);
            for _ in 0..4 {
                executor
                    .record_zero_fee_soft_failure(B256::ZERO, Some(candidate))
                    .unwrap();
            }
            let hash = B256::repeat_byte(1);
            assert!(matches!(
                executor.record_zero_fee_soft_failure(hash, Some(candidate)),
                Err(BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                    hash: rejected_hash, ..
                })) if rejected_hash == hash
            ));
            assert_eq!(executor.zero_fee_soft_failures, 4);
            assert_eq!(executor.zero_fee_hook_soft_failures.get(&3), Some(&4));
            for tag in [
                ZeroFeeHookTag::new(4, "OtherCapped", None).with_max_soft_failures_per_block(4),
                ZeroFeeHookTag::new(5, "Uncapped", None),
            ] {
                executor
                    .record_zero_fee_soft_failure(
                        B256::ZERO,
                        Some(ZeroFeeCandidate::new(tag, Address::ZERO)),
                    )
                    .unwrap();
                assert_eq!(
                    executor.zero_fee_hook_soft_failures.get(&tag.id()),
                    Some(&1)
                );
            }
            for _ in 6..64 {
                executor
                    .record_zero_fee_soft_failure(B256::ZERO, None)
                    .unwrap();
            }
            let before = executor.zero_fee_hook_soft_failures.clone();
            let fresh = ZeroFeeCandidate::new(ZeroFeeHookTag::new(6, "Fresh", None), Address::ZERO);
            assert!(executor
                .record_zero_fee_soft_failure(hash, Some(fresh))
                .is_err());
            assert_eq!(executor.zero_fee_soft_failures, 64);
            assert_eq!(executor.zero_fee_hook_soft_failures, before);
        });
        with_executor(|executor| {
            assert_eq!(executor.zero_fee_soft_failures, 0);
            assert!(executor.zero_fee_hook_soft_failures.is_empty());
        });
    }

    pub(crate) fn with_executor(f: impl FnOnce(OutbeBlockExecutor<'_>)) {
        let chain_spec = Arc::new(
            reth_chainspec::MAINNET
                .as_ref()
                .clone()
                .map_header(OutbeHeader::new),
        );
        let receipt_builder = RethReceiptBuilder::default();
        let evm = crate::OutbeEvmFactory::new(crate::host::stub_host()).create_evm(
            EmptyDB::default(),
            EthEvmEnv::new(
                SpecId::SHANGHAI,
                BlockEnvExt {
                    gas_limit: U256::from(30_000_000_u64),
                    ..Default::default()
                },
                1,
            ),
        );
        let inner = EthBlockExecutor::new(
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
            chain_spec.as_ref(),
            &receipt_builder,
        );
        f(OutbeBlockExecutor::new(
            inner,
            Arc::clone(&chain_spec),
            None,
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
                    proposer_evm_address: None,
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
                    evm_signer: None,
                    runtime_host: crate::host::stub_host(),
                },
            },
        ));
    }
}
