//! User transaction fee route: protocol waiver, delegation bootstrap, sponsorship, paid.

use super::*;

mod paid;
mod sponsorship;
mod waiver;

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason> + ZeroFeeCfgAccess,
    E::Spec: Into<revm::primitives::hardfork::SpecId>,
    E::Error: std::fmt::Display,
{
    pub(in crate::executor) fn route_user_transaction<R, F>(
        &mut self,
        tx_env: TxEnv,
        recovered: R,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        let ctx = self.fee_route_context()?;
        let tx = recovered.tx();
        let signer = *recovered.signer();
        if tx.gas_limit() < Self::SOFT_FAILURE_GAS {
            return Err(
                crate::errors::ExecutorValidationError::TransactionGasLimitBelowFloor {
                    gas_limit: tx.gas_limit(),
                    floor: Self::SOFT_FAILURE_GAS,
                }
                .into(),
            );
        }

        let zero_fee_tx = zero_fee_transaction(tx, signer);
        let candidate = match outbe_zerofee::registry().classify(&zero_fee_tx) {
            Ok(candidate) => candidate,
            Err(error) => return self.include_zero_fee_soft_failure(tx, error),
        };
        if let Some(candidate) = candidate {
            return self.execute_protocol_fee_waiver(candidate, tx_env, recovered, f);
        }

        // Preserve the signer read before both bootstrap and paid routing.
        let account = self.read_fee_route_account(signer, ctx.clone())?;
        if waiver::bootstrap_fee_waiver_authorized(tx, signer, ctx.chain_id, &account) {
            let gas_limit = tx.gas_limit();
            if !self.sponsored_gas_fits(gas_limit) {
                return Err(crate::sponsored_block_budget_deferral(*tx.tx_hash()));
            }
            let result = self.execute_without_user_fee(tx_env, recovered, f);
            if matches!(result, Ok(Some(_))) {
                self.charge_sponsored_gas(gas_limit);
            }
            return result;
        }

        let delegated_to = self.fee_route_delegation(account)?;
        // Delegation is additive: an envelope that opts into paid execution
        // still takes the normal fee path, even when sponsorship is exhausted.
        let wants_sponsorship = delegated_to == Some(outbe_zerofee::ZEROFEE_ADDRESS)
            && outbe_zerofee::classify_sponsorship(&zero_fee_tx).is_ok();
        if wants_sponsorship {
            return self.execute_sponsored_transaction(ctx, tx_env, recovered, f);
        }

        self.execute_paid_transaction(tx_env, recovered, f)
    }

    /// The executor includes policy rejections with fixed gas and a failure log,
    /// up to the shared per-block cap. Account for the cap before appending a receipt.
    fn include_zero_fee_soft_failure(
        &mut self,
        tx: &TransactionSigned,
        error: outbe_zerofee::ZeroFeePolicyError,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        self.record_zero_fee_soft_failure(*tx.tx_hash())?;
        self.push_failure_receipt(
            tx.tx_type(),
            outbe_primitives::addresses::ZERO_FEE_POLICY_LOG_ADDRESS,
            error.code(),
            error.to_string(),
        );
        Ok(Some(GasOutput::new(Self::SOFT_FAILURE_GAS)))
    }

    fn fee_route_context(&self) -> Result<BlockContext, BlockExecutionError> {
        let timestamp = outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(crate::errors::ExecutorValidationError::from)?;
        Ok(BlockContext::new_with_genesis_hash(
            outbe_primitives::block::BlockContextInput {
                block_number: self.inner.evm.block().number().saturating_to::<u64>(),
                timestamp,
                chain_id: self.inner.evm.chain_id(),
                genesis_hash: self.genesis_hash,
                proposer: self.inner.evm.block().beneficiary(),
                validators: Vec::new(),
            },
        ))
    }
}
