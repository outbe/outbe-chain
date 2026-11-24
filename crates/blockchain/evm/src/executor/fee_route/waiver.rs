//! Protocol fee waivers, bootstrap admission and delegation lookup.

use super::*;
use alloy_primitives::KECCAK256_EMPTY;
use evm2::evm::AccountInfo;

pub fn bootstrap_fee_waiver_authorized(
    policy: &dyn crate::UserTxFeePolicy,
    tx: &TransactionSigned,
    signer: Address,
    chain_id: u64,
    account: &AccountInfo,
) -> bool {
    use outbe_primitives::zero_fee_types::BootstrapAccountView;

    let candidate = BootstrapTransactionView::from_transaction(tx, signer, chain_id)
        .and_then(|view| policy.classify_bootstrap(&view));
    candidate.is_some_and(|candidate| {
        policy.authorize_bootstrap(
            candidate,
            BootstrapAccountView {
                balance: account.balance,
                nonce: account.nonce,
                code_empty: account.is_empty_code_hash(),
            },
        )
    })
}

fn zero_envelope_fees(tx: &mut TxEnvelope) {
    match tx {
        TxEnvelope::Legacy(tx) => tx.gas_price = 0,
        TxEnvelope::Eip2930(tx) => tx.gas_price = 0,
        TxEnvelope::Eip1559(tx) => {
            tx.max_fee_per_gas = 0;
            tx.max_priority_fee_per_gas = 0;
        }
        TxEnvelope::Eip4844(tx) => {
            let inner: &mut alloy_consensus::TxEip4844 = tx.as_mut();
            inner.max_fee_per_gas = 0;
            inner.max_priority_fee_per_gas = 0;
        }
        TxEnvelope::Eip7702(tx) => {
            tx.max_fee_per_gas = 0;
            tx.max_priority_fee_per_gas = 0;
        }
    }
}

impl OutbeBlockExecutor<'_> {
    pub fn execute_protocol_fee_waiver<R, F>(
        &mut self,
        candidate: outbe_primitives::zero_fee_types::ZeroFeeCandidate,
        tx_env: Recovered<TxEnvelope>,
        recovered: R,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
        F: FnOnce(&TxResult<OutbeEvmTypes>, TxStateChanges<'_>) -> CommitChanges,
    {
        // Release the storage borrow before mutating receipt/cap state.
        let ctx = self.fee_route_context()?;
        let host = self.runtime_host.clone();
        let authorize_outcome = {
            let tx = outbe_primitives::zero_fee_types::ZeroFeeTransaction::from_transaction(
                recovered.tx(),
                candidate.signer,
            );
            let db = self.inner.evm_mut().overlay_db_mut();
            let mut provider = DirectStorageProvider::new(db, ctx);
            let storage = StorageHandle::new(&mut provider);
            host.user_fee_policy()
                .authorize_fee_waiver(storage, &tx, candidate)
                .map(|_| ())
        };
        if let Err(error) = authorize_outcome {
            return self.include_zero_fee_soft_failure(recovered.tx(), error, Some(candidate));
        }
        self.execute_without_user_fee(tx_env, recovered, f)
    }

    /// Restore the exact previous configuration after every ordinary Result
    /// exit, including an execution error or a declined commit.
    pub fn execute_without_user_fee<R, F>(
        &mut self,
        tx_env: Recovered<TxEnvelope>,
        recovered: R,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
        F: FnOnce(&TxResult<OutbeEvmTypes>, TxStateChanges<'_>) -> CommitChanges,
    {
        let snapshot = self.inner.evm_mut().enable_zero_fee_overrides();
        let (mut envelope, signer) = tx_env.into_parts();
        zero_envelope_fees(&mut envelope);
        let zeroed_tx_env = Recovered::new_unchecked(envelope, signer);
        let result = self
            .inner
            .execute_transaction_with_commit_condition((zeroed_tx_env, recovered), f);
        self.inner.evm_mut().restore_zero_fee_overrides(snapshot);
        result
    }

    pub fn read_fee_route_account(
        &mut self,
        signer: Address,
        ctx: BlockContext,
    ) -> Result<AccountInfo, BlockExecutionError> {
        let db = self.inner.evm_mut().overlay_db_mut();
        let mut provider = DirectStorageProvider::new(db, ctx);
        let storage = StorageHandle::new(&mut provider);
        storage
            .with_account_info(signer, |info| Ok(info.clone()))
            .map_err(|error| {
                if error.is_node_local() || matches!(error, PrecompileError::Fatal(_)) {
                    BlockExecutionError::other(error)
                } else {
                    crate::errors::ExecutorValidationError::free_tx_signer_account_read(error)
                        .into()
                }
            })
    }

    pub fn fee_route_delegation(
        &mut self,
        account: AccountInfo,
    ) -> Result<Option<Address>, BlockExecutionError> {
        if let Some(code) = account.code {
            return Ok(code.eip7702_address());
        }
        if account.code_hash == KECCAK256_EMPTY {
            return Ok(None);
        }
        // Account info can omit bytecode for an account unchanged since a prior
        // block. Fetch it by hash before deciding whether it is delegated.
        self.inner
            .evm_mut()
            .overlay_db_mut()
            .get_code_by_hash(&account.code_hash)
            .map(|code| code.eip7702_address())
            .map_err(|error| {
                BlockExecutionError::other(
                    crate::errors::ExecutorStorageError::free_tx_signer_code_lookup(error),
                )
            })
    }
}
