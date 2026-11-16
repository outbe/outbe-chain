use super::*;
use outbe_primitives::zero_fee_types::ZeroFeeCandidate;

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

/// Rejects inclusion without consuming nonce, daily quota, or a soft-failure slot.
pub fn sponsored_block_budget_deferral(tx_hash: B256) -> BlockExecutionError {
    BlockExecutionError::Validation(BlockValidationError::InvalidTx {
        hash: tx_hash,
        error: Box::new(InvalidTransaction::Str(
            SPONSORED_BLOCK_BUDGET_EXCEEDED.into(),
        )),
    })
}

/// Identifies only the block-local sponsorship deferral, not other invalid transactions.
pub fn is_sponsored_block_budget_deferral(error: &dyn alloy_evm::InvalidTxError) -> bool {
    matches!(
        error.as_invalid_tx_err(),
        Some(InvalidTransaction::Str(reason)) if reason == SPONSORED_BLOCK_BUDGET_EXCEEDED
    )
}

/// Exact transaction-validation reason used to defer a transaction whose enclave witness entries
/// would exceed the current block's witness size budget.
pub const ENCLAVE_WITNESS_BUDGET_EXCEEDED: &str = "enclave witness block budget exceeded";

/// Rejects inclusion into the current block without evicting the transaction from the pool.
pub fn enclave_witness_budget_deferral(tx_hash: B256) -> BlockExecutionError {
    BlockExecutionError::Validation(BlockValidationError::InvalidTx {
        hash: tx_hash,
        error: Box::new(InvalidTransaction::Str(
            ENCLAVE_WITNESS_BUDGET_EXCEEDED.into(),
        )),
    })
}

/// Identifies only the block-local enclave witness budget deferral.
pub fn is_enclave_witness_budget_deferral(error: &dyn alloy_evm::InvalidTxError) -> bool {
    matches!(
        error.as_invalid_tx_err(),
        Some(InvalidTransaction::Str(reason)) if reason == ENCLAVE_WITNESS_BUDGET_EXCEEDED
    )
}

#[derive(Debug, Clone, Copy)]
pub struct ZeroFeeCfgSnapshot {
    disable_balance_check: bool,
    disable_base_fee: bool,
    disable_fee_charge: bool,
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

impl<DB, I, P> ZeroFeeCfgAccess for OutbeEvm<DB, I, P>
where
    DB: Database,
{
    fn enable_zero_fee_overrides(&mut self) -> super::ZeroFeeCfgSnapshot {
        let cfg = &mut self.ctx_mut().cfg;
        let snapshot = ZeroFeeCfgSnapshot {
            disable_balance_check: cfg.disable_balance_check,
            disable_base_fee: cfg.disable_base_fee,
            disable_fee_charge: cfg.disable_fee_charge,
        };
        cfg.disable_balance_check = true;
        cfg.disable_base_fee = true;
        cfg.disable_fee_charge = true;
        snapshot
    }

    fn restore_zero_fee_overrides(&mut self, snapshot: ZeroFeeCfgSnapshot) {
        let cfg = &mut self.ctx_mut().cfg;
        cfg.disable_balance_check = snapshot.disable_balance_check;
        cfg.disable_base_fee = snapshot.disable_base_fee;
        cfg.disable_fee_charge = snapshot.disable_fee_charge;
    }

    fn set_executor_owned(&mut self, owned: bool) {
        self.set_owned_by_executor(owned);
    }

    fn set_witness_state(
        &mut self,
        state: Option<Arc<std::sync::Mutex<outbe_tee::witness::WitnessState>>>,
    ) {
        self.set_attached_witness_state(state);
    }
}

pub fn zero_fee_transaction<'a, T>(tx: &'a T, signer: Address) -> ZeroFeeTransaction<'a>
where
    T: alloy_consensus::Transaction + ?Sized,
{
    ZeroFeeTransaction::from_transaction(tx, signer)
}

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
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
    /// outside the EVM (zero-fee policy). The value mirrors the
    /// 21_000 intrinsic-gas baseline of every EVM transaction.
    pub const SOFT_FAILURE_GAS: u64 = 21_000;

    /// Maximum number of zero-fee soft-failure receipts a single
    /// block may carry.
    ///
    /// The executor gives a soft receipt (`status=0`, 21k gas) to
    /// quota-exhausted EIP-7702-sponsored txs and to duplicate or losing
    /// zero-fee oracle votes. Thus these txs land in the block and do not abort
    /// the build (the 2026-05-15 halt). Without a bound, an attacker can stuff a
    /// whole block with thousands of zero-cost 21k soft-failures. These
    /// soft-failures displace real transactions.
    /// 64 is far above the handful of soft-failures that honest operation
    /// produces per block. It caps stuffing at `64 * 21k ~= 1.34M` gas, which is
    /// under ~5% of a 30M-gas block. Protocol constant: both the proposer
    /// (build) and validator (re-execution) read it, so they agree on the bound.
    const MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK: u32 = 64;

    /// Account for one zero-fee soft-failure and enforce the
    /// global and per-hook block caps, checking both before mutating either counter.
    ///
    /// Returns `Ok(())` when the soft-failure is within the per-block budget, and
    /// records it. Past [`Self::MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK`] it
    /// returns `Err(BlockValidationError::InvalidTx)`:
    /// - The payload builder SKIPS the tx (`mark_invalid` + continue). The tx is
    ///   excluded from the block and evicted from the pool.
    /// - A validator REJECTS a block that exceeds the cap. The `?` on the
    ///   re-execution path propagates the error as a block failure.
    ///
    /// The counter is the number of zero-fee soft-receipts in the block. It is
    /// identical on both paths. Thus an honest block (`<= cap`) never trips the
    /// validator, and every validator rejects a byzantine over-cap block
    /// deterministically. `InvalidTransaction::Str` is a tx-level validation
    /// error (not nonce-too-low, so the builder marks it invalid rather than
    /// retrying). This keeps it out of the fatal `BlockExecutionError::Internal`
    /// class that would abort the build.
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
            let reason = format!(
                "zero-fee soft-failure cap ({}) exceeded for this block; tx rejected to bound \
                 block stuffing",
                cap
            );
            return Err(BlockExecutionError::Validation(
                BlockValidationError::InvalidTx {
                    hash: tx_hash,
                    error: Box::new(InvalidTransaction::Str(std::borrow::Cow::Owned(reason))),
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
    use alloy_evm::{EvmEnv, EvmFactory};
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use outbe_primitives::zero_fee_types::{ZeroFeeCandidate, ZeroFeeHookTag};
    use revm::{
        context::{BlockEnv, CfgEnv},
        database::{EmptyDB, State},
        inspector::NoOpInspector,
        primitives::hardfork::SpecId,
    };

    #[test]
    fn zero_fee_soft_failure_metrics_respect_block_cap() {
        // DebuggingRecorder snapshots drain counters, so inspect each scenario once.
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

    pub(crate) type TestEvm =
        <crate::OutbeEvmFactory as EvmFactory>::Evm<State<EmptyDB>, NoOpInspector>;

    pub(crate) fn with_executor(f: impl FnOnce(OutbeBlockExecutor<'_, TestEvm>)) {
        let chain_spec = Arc::new(
            reth_chainspec::MAINNET
                .as_ref()
                .clone()
                .map_header(OutbeHeader::new),
        );
        let receipt_builder = RethReceiptBuilder::default();
        let evm = crate::OutbeEvmFactory::new(crate::host::stub_host()).create_evm(
            State::builder().with_database(EmptyDB::default()).build(),
            EvmEnv {
                cfg_env: CfgEnv::new()
                    .with_chain_id(1)
                    .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
                block_env: BlockEnv {
                    gas_limit: 30_000_000,
                    ..Default::default()
                },
            },
        );
        let inner = EthBlockExecutor::new(
            evm,
            alloy_evm::eth::EthBlockExecutionCtx {
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
        f(OutbeBlockExecutor::new(
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
