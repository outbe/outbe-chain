//! Sponsorship authorization and quota/log commit after transaction inclusion.

use super::*;

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason> + ZeroFeeCfgAccess,
    E::Spec: Into<revm::primitives::hardfork::SpecId>,
    E::Error: std::fmt::Display,
{
    pub(super) fn execute_sponsored_transaction<R, F>(
        &mut self,
        ctx: BlockContext,
        tx_env: TxEnv,
        recovered: R,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        let signer = *recovered.signer();
        // Authorization is a read. Persist the counter only after inclusion.
        let authorized = {
            let db = self.inner.evm.db_mut();
            let mut provider = DirectStorageProvider::new(db, ctx.clone());
            let storage = StorageHandle::new(&mut provider);
            outbe_zerofee::authorize_sponsorship(storage, signer, ctx.timestamp)
        };
        let current_day = match authorized {
            Ok(auth) => auth.current_day,
            Err(error) => return self.include_zero_fee_soft_failure(recovered.tx(), error),
        };
        let result = self.execute_without_user_fee(tx_env, recovered, f);
        // An included revert consumes a slot. Err/Ok(None) neither writes the
        // quota nor appends sponsorship logs to a previous receipt.
        if !matches!(result, Ok(Some(_))) {
            return result;
        }

        let sponsorship_events = {
            let db = self.inner.evm.db_mut();
            let mut provider = DirectStorageProvider::new(db, ctx);
            let recorded = {
                let storage = StorageHandle::new(&mut provider);
                outbe_zerofee::record_sponsorship_use(storage, signer, current_day)
            };
            if let Err(err) = recorded {
                return Err(BlockExecutionError::msg(format!(
                    "sponsored quota write failed after inclusion: {err}"
                )));
            }
            provider.flush().map_err(|err| {
                BlockExecutionError::msg(format!(
                    "sponsored quota flush failed after inclusion: {err}"
                ))
            })?;
            let events = provider.take_events();
            // State::commit already notified the parallel state root hook.
            let _changes = provider.take_committed_changes();
            events
        };
        if !sponsorship_events.is_empty() {
            if let Some(receipt) = self.inner.receipts.last_mut() {
                receipt.logs.extend(sponsorship_events);
            }
        }
        result
    }
}
