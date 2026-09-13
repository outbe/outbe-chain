use super::*;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ZeroFeeCfgSnapshot {
    disable_balance_check: bool,
    disable_base_fee: bool,
    disable_fee_charge: bool,
}

pub(crate) trait ZeroFeeCfgAccess {
    fn enable_zero_fee_overrides(&mut self) -> ZeroFeeCfgSnapshot;
    fn restore_zero_fee_overrides(&mut self, snapshot: ZeroFeeCfgSnapshot);
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
}

pub(in crate::executor) fn zero_fee_transaction<'a, T>(
    tx: &'a T,
    signer: Address,
) -> ZeroFeeTransaction<'a>
where
    T: alloy_consensus::Transaction + ?Sized,
{
    ZeroFeeTransaction {
        signer,
        to: tx.to(),
        value: tx.value(),
        input: tx.input().as_ref(),
        gas_limit: tx.gas_limit(),
        max_fee_per_gas: tx.max_fee_per_gas(),
        max_priority_fee_per_gas: tx.max_priority_fee_per_gas(),
    }
}

pub(in crate::executor) fn bootstrap_transaction<'a, T>(
    tx: &'a T,
    signer: Address,
    network_chain_id: u64,
) -> Option<BootstrapTransactionView<'a>>
where
    T: alloy_consensus::Transaction + ?Sized,
{
    let authorization_list = tx.authorization_list()?;
    Some(BootstrapTransactionView {
        signer,
        tx_chain_id: tx.chain_id(),
        network_chain_id,
        nonce: tx.nonce(),
        to: tx.to(),
        value: tx.value(),
        input: tx.input().as_ref(),
        gas_limit: tx.gas_limit(),
        max_fee_per_gas: tx.max_fee_per_gas(),
        max_priority_fee_per_gas: tx.max_priority_fee_per_gas(),
        access_list_empty: tx.access_list().is_some_and(|list| list.is_empty()),
        authorization_list,
    })
}

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
    /// Intrinsic gas accounted on the synthetic receipt that replaces a
    /// hard `BlockExecutionError` when the executor rejects a user transaction
    /// outside the EVM (zero-fee policy). The value mirrors the
    /// 21_000 intrinsic-gas baseline of every EVM transaction.
    pub(in crate::executor) const SOFT_FAILURE_GAS: u64 = 21_000;

    /// Maximum number of zero-fee soft-failure receipts a single
    /// block may carry.
    ///
    /// Quota-exhausted EIP-7702-sponsored txs and duplicate/losing
    /// zero-fee oracle votes are soft-receipted (`status=0`, 21k gas) so
    /// they land in the block rather than aborting the build (the 2026-05-15
    /// halt). Without a bound, an attacker can stuff a whole block with
    /// thousands of zero-cost 21k soft-failures, crowding out real transactions.
    /// 64 is far above the handful of soft-failures honest operation produces
    /// per block, yet caps stuffing at `64 * 21k ~= 1.34M` gas - under ~5% of a
    /// 30M-gas block. Protocol constant: both the proposer (build) and validator
    /// (re-execution) read it, so they agree on the bound.
    const MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK: u32 = 64;

    /// Account for one zero-fee soft-failure and enforce the
    /// per-block cap.
    ///
    /// Returns `Ok(())` when the soft-failure is within the per-block budget (and
    /// records it); past [`Self::MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK`] it
    /// returns `Err(BlockValidationError::InvalidTx)`, which the payload builder
    /// SKIPS (`mark_invalid` + continue - the tx is excluded from the block and
    /// evicted from the pool) while a validator REJECTS a block that exceeds the
    /// cap (the `?` on the re-execution path propagates it as a block failure).
    /// The counter is the number of zero-fee soft-receipts in the block and is
    /// identical on both paths, so an honest block (`<= cap`) never trips the
    /// validator and a byzantine over-cap block is rejected deterministically by
    /// every validator. `InvalidTransaction::Str` is a tx-level validation error
    /// (not nonce-too-low, so the builder marks it invalid rather than retrying),
    /// keeping it out of the fatal `BlockExecutionError::Internal` class that
    /// would abort the build.
    pub(in crate::executor) fn record_zero_fee_soft_failure(
        &mut self,
        tx_hash: B256,
    ) -> Result<(), BlockExecutionError> {
        if self.zero_fee_soft_failures >= Self::MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK {
            let reason = format!(
                "zero-fee soft-failure cap ({}) exceeded for this block; tx rejected to bound \
                 block stuffing",
                Self::MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK
            );
            return Err(BlockExecutionError::Validation(
                BlockValidationError::InvalidTx {
                    hash: tx_hash,
                    error: Box::new(InvalidTransaction::Str(std::borrow::Cow::Owned(reason))),
                },
            ));
        }
        self.zero_fee_soft_failures = self.zero_fee_soft_failures.saturating_add(1);
        Ok(())
    }
}
