//! Protocol fee waivers, bootstrap admission and delegation lookup.

use super::*;
use revm::state::AccountInfo;

pub(super) fn bootstrap_fee_waiver_authorized(
    tx: &TransactionSigned,
    signer: Address,
    chain_id: u64,
    account: &AccountInfo,
) -> bool {
    let candidate = BootstrapTransactionView::from_transaction(tx, signer, chain_id)
        .and_then(|view| outbe_zerofee::classify_bootstrap(&view));
    candidate.is_some_and(|candidate| {
        outbe_zerofee::authorize_bootstrap(
            candidate,
            outbe_zerofee::BootstrapAccountView {
                balance: account.balance,
                nonce: account.nonce,
                code_empty: account.is_empty_code_hash(),
            },
        )
    })
}

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason> + ZeroFeeCfgAccess,
    E::Spec: Into<revm::primitives::hardfork::SpecId>,
    E::Error: std::fmt::Display,
{
    pub(super) fn execute_protocol_fee_waiver<R, F>(
        &mut self,
        candidate: outbe_zerofee::ZeroFeeCandidate,
        tx_env: TxEnv,
        recovered: R,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        // Release the storage borrow before mutating receipt/cap state.
        let ctx = self.fee_route_context();
        let authorize_outcome = {
            let db = self.inner.evm.db_mut();
            let mut provider = DirectStorageProvider::new(db, ctx);
            let storage = StorageHandle::new(&mut provider);
            outbe_zerofee::registry()
                .authorize_fee_waiver(storage, candidate)
                .map(|_| ())
        };
        if let Err(error) = authorize_outcome {
            return self.include_zero_fee_soft_failure(recovered.tx(), error);
        }
        self.execute_without_user_fee(tx_env, recovered, f)
    }

    /// Restore the exact previous configuration after every ordinary Result
    /// exit, including an execution error or a declined commit.
    pub(super) fn execute_without_user_fee<R, F>(
        &mut self,
        mut tx_env: TxEnv,
        recovered: R,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        let snapshot = self.inner.evm.enable_zero_fee_overrides();
        tx_env.gas_price = 0;
        tx_env.gas_priority_fee = Some(0);
        let result = self.inner.execute_transaction_with_commit_condition(
            WithTxEnv {
                tx_env,
                tx: Arc::new(recovered),
            },
            f,
        );
        self.inner.evm.restore_zero_fee_overrides(snapshot);
        result
    }

    pub(super) fn read_fee_route_account(
        &mut self,
        signer: Address,
        ctx: BlockContext,
    ) -> Result<AccountInfo, BlockExecutionError> {
        let db = self.inner.evm.db_mut();
        let mut provider = DirectStorageProvider::new(db, ctx);
        let storage = StorageHandle::new(&mut provider);
        storage
            .with_account_info(signer, |info| Ok(info.clone()))
            .map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!("free-tx signer account read failed: {error}").into(),
                ))
            })
    }

    pub(super) fn fee_route_delegation(
        &mut self,
        account: AccountInfo,
    ) -> Result<Option<Address>, BlockExecutionError> {
        if let Some(code) = account.code {
            return Ok(code.eip7702_address());
        }
        if account.code_hash == revm::primitives::KECCAK_EMPTY {
            return Ok(None);
        }
        // basic() can omit bytecode for an account unchanged since a prior
        // block. Fetch it by hash before deciding whether it is delegated.
        self.inner
            .evm
            .db_mut()
            .code_by_hash(account.code_hash)
            .map(|code| code.eip7702_address())
            .map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!("free-tx signer code lookup failed: {error}").into(),
                ))
            })
    }
}
