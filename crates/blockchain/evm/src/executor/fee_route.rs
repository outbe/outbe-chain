//! User transaction fee route: oracle waiver, delegation bootstrap, sponsorship, paid.

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
    pub(in crate::executor) fn route_user_transaction<R, F>(
        &mut self,
        mut tx_env: TxEnv,
        recovered: R,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        let tx = recovered.tx();
        let signer = *recovered.signer();
        if tx.gas_limit() < Self::SOFT_FAILURE_GAS {
            return Err(BlockExecutionError::msg(format!(
                "transaction gas limit {} is below intrinsic gas floor {}",
                tx.gas_limit(),
                Self::SOFT_FAILURE_GAS
            )));
        }

        // a zero-fee policy rejection used to be `BlockExecutionError::msg(.)`,
        // which payload_builder turned into a fatal `PayloadBuilderError::evm(...)` and
        // aborted block build - see EPIC for the halt of 2026-05-15. The tx is now
        // included with a `status=0` synthetic receipt carrying an `OutbeFailure(code, reason)`
        // log. Mempool eviction happens via Reth's standard `on_canonical_state_change` once
        // the block becomes canonical (`pool.remove_transactions(block.body)`), so no custom
        // side-channel is required (see Won't Do).
        let zero_fee_tx = zero_fee_transaction(tx, signer);
        let zero_fee = match outbe_zerofee::registry().classify(&zero_fee_tx) {
            Ok(value) => value,
            Err(err) => {
                // account for this zero-fee soft-failure and reject
                // it past the per-block cap (skipped on build, block rejected on
                // validate) so it cannot stuff the block with zero-cost 21k
                // soft-failures.
                self.record_zero_fee_soft_failure(*tx.tx_hash())?;
                let tx_type = tx.tx_type();
                let code = err.code();
                self.push_failure_receipt(
                    tx_type,
                    outbe_primitives::addresses::ZERO_FEE_POLICY_LOG_ADDRESS,
                    code,
                    err.to_string(),
                );
                return Ok(Some(GasOutput::new(Self::SOFT_FAILURE_GAS)));
            }
        };

        if let Some(candidate) = zero_fee {
            let block_number = self.inner.evm.block().number().saturating_to::<u64>();

            let timestamp = self.inner.evm.block().timestamp().saturating_to::<u64>();
            let chain_id = self.inner.evm.chain_id();
            let proposer = self.inner.evm.block().beneficiary();
            let ctx = BlockContext::new_with_genesis_hash(
                block_number,
                timestamp,
                chain_id,
                self.genesis_hash,
                proposer,
                Vec::new(),
            );

            // Same soft-failure path as `classify`: stateful authorization rejection becomes a
            // `status=0` receipt rather than a hard block error. We borrow `db` only inside the
            // scope that calls `authorize_fee_waiver`, then drop it before mutating the
            // executor's own state (push_failure_receipt).
            let authorize_outcome = {
                let db = self.inner.evm.db_mut();
                let mut provider = DirectStorageProvider::new(db, ctx);
                let storage = StorageHandle::new(&mut provider);
                outbe_zerofee::registry()
                    .authorize_fee_waiver(storage, candidate)
                    .map(|_| ())
            };
            if let Err(err) = authorize_outcome {
                // account for this zero-fee soft-failure and reject
                // it past the per-block cap (skipped on build, block rejected on
                // validate) so it cannot stuff the block with zero-cost 21k
                // soft-failures.
                self.record_zero_fee_soft_failure(*tx.tx_hash())?;
                let tx_type = tx.tx_type();
                let code = err.code();
                self.push_failure_receipt(
                    tx_type,
                    outbe_primitives::addresses::ZERO_FEE_POLICY_LOG_ADDRESS,
                    code,
                    err.to_string(),
                );
                return Ok(Some(GasOutput::new(Self::SOFT_FAILURE_GAS)));
            }

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
            return result;
        }

        // EIP-7702 sponsored free-tx path. Oracle hook had its chance via
        // `classify` above; this branch handles the second source of fee
        // waivers - EOAs that have delegated to [`outbe_zerofee::ZEROFEE_ADDRESS`]
        // via a Pectra set-code authorization. The same `disable_balance_check
        // + disable_base_fee + disable_fee_charge` cfg snapshot is applied.
        // The daily counter is written only after the inner transaction is
        // included, and an included revert still burns the slot.
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let timestamp = self.inner.evm.block().timestamp().saturating_to::<u64>();
        let chain_id = self.inner.evm.chain_id();
        let proposer = self.inner.evm.block().beneficiary();

        // Pull `(code_hash, maybe_code)` from the
        // provider. `State<DB>::basic()` (the underlying source) only
        // populates `info.code` for accounts that have had recent
        // changes; otherwise the bytecode lives behind `code_by_hash`
        // and `info.code` is None. The fix below performs the second
        // lookup when needed so the EIP-7702 delegation probe sees the
        // real bytecode in steady state.
        let signer_state = {
            let db = self.inner.evm.db_mut();
            let ctx = BlockContext::new_with_genesis_hash(
                block_number,
                timestamp,
                chain_id,
                self.genesis_hash,
                proposer,
                Vec::new(),
            );
            let mut provider = DirectStorageProvider::new(db, ctx);
            let storage = StorageHandle::new(&mut provider);
            storage.with_account_info(signer, |info| {
                Ok((
                    info.balance,
                    info.nonce,
                    info.is_empty_code_hash(),
                    info.code_hash,
                    info.code.clone(),
                ))
            })
        };

        let (balance, nonce, code_empty, code_hash, maybe_code) = match signer_state {
            Ok(state) => state,
            Err(err) => {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!("free-tx signer account read failed: {err}").into(),
                    ),
                ));
            }
        };

        let bootstrap_candidate = bootstrap_transaction(tx, signer, chain_id)
            .and_then(|view| outbe_zerofee::classify_bootstrap(&view));
        let bootstrap_authorized = bootstrap_candidate.is_some_and(|candidate| {
            outbe_zerofee::authorize_bootstrap(
                candidate,
                outbe_zerofee::BootstrapAccountView {
                    balance,
                    nonce,
                    code_empty,
                },
            )
        });

        if bootstrap_authorized {
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
            return result;
        }

        let delegated_to = if let Some(code) = maybe_code {
            code.eip7702_address()
        } else if code_hash != revm::primitives::KECCAK_EMPTY {
            // basic() did not populate `code` - fetch bytecode by
            // hash directly. This is the steady-state path for any
            // account whose code was set in a prior block.
            match self.inner.evm.db_mut().code_by_hash(code_hash) {
                Ok(code) => code.eip7702_address(),
                Err(err) => {
                    return Err(BlockExecutionError::Internal(
                        InternalBlockExecutionError::Other(
                            format!("free-tx signer code lookup failed: {err}").into(),
                        ),
                    ));
                }
            }
        } else {
            None
        };

        // A delegated account opts into sponsorship ONLY by sending the
        // exact free-tx envelope (`classify_sponsorship` Ok: value == 0,
        // priority_fee == 0, gas <= cap, calldata <= cap, to in
        // whitelist). If the envelope does not match - most importantly
        // `priority_fee > 0` ("I am paying") - the transaction is NOT a
        // sponsorship request and falls through to the normal fee path
        // below, even though the account is delegated. This keeps
        // EIP-7702 delegation ADDITIVE: delegating to the paymaster never
        // jails an account into free-only mode, and once a signer's daily
        // quota is exhausted they simply set a tip and pay as usual.
        //
        // The stateful `authorize_sponsorship` inside the branch still
        // soft-fails a correctly-shaped attempt with code 110 (quota
        // exhausted) or 107 (self) - those
        // are zero-tip requests that explicitly asked for free and must
        // not be silently charged.
        let wants_sponsorship = delegated_to == Some(outbe_zerofee::ZEROFEE_ADDRESS)
            && outbe_zerofee::classify_sponsorship(&zero_fee_tx).is_ok();

        if wants_sponsorship {
            // Authorization is a read. The counter write waits until the
            // inner transaction is included, so an execution error leaves
            // the quota slot untouched.
            let authorized = {
                let db = self.inner.evm.db_mut();
                let ctx = BlockContext::new_with_genesis_hash(
                    block_number,
                    timestamp,
                    chain_id,
                    self.genesis_hash,
                    proposer,
                    Vec::new(),
                );
                let mut provider = DirectStorageProvider::new(db, ctx);
                let storage = StorageHandle::new(&mut provider);
                outbe_zerofee::authorize_sponsorship(storage, signer, timestamp)
            };
            let current_day = match authorized {
                Ok(auth) => auth.current_day,
                Err(err) => {
                    // account for this zero-fee soft-failure and reject
                    // it past the per-block cap (skipped on build, block rejected on
                    // validate) so it cannot stuff the block with zero-cost 21k
                    // soft-failures.
                    self.record_zero_fee_soft_failure(*tx.tx_hash())?;
                    let tx_type = tx.tx_type();
                    let code = err.code();
                    self.push_failure_receipt(
                        tx_type,
                        outbe_primitives::addresses::ZERO_FEE_POLICY_LOG_ADDRESS,
                        code,
                        err.to_string(),
                    );
                    return Ok(Some(GasOutput::new(Self::SOFT_FAILURE_GAS)));
                }
            };

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
            // Ok(None) commits nothing and must not append a sponsorship log
            // onto the previous receipt. An included revert is Ok(Some) and
            // still burns the slot.
            if !matches!(result, Ok(Some(_))) {
                return result;
            }

            let sponsorship_events = {
                let db = self.inner.evm.db_mut();
                let ctx = BlockContext::new_with_genesis_hash(
                    block_number,
                    timestamp,
                    chain_id,
                    self.genesis_hash,
                    proposer,
                    Vec::new(),
                );
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
            return result;
        }

        let base_fee_per_gas = self.inner.evm.block().basefee() as u128;
        let max_fee_per_gas = tx.max_fee_per_gas();
        let max_priority_fee_per_gas = tx.max_priority_fee_per_gas();

        let result = self.inner.execute_transaction_with_commit_condition(
            WithTxEnv {
                tx_env,
                tx: Arc::new(recovered),
            },
            f,
        )?;

        if let Some(gas_used) = result {
            let validator_fee = validator_fee_for_gas(
                max_fee_per_gas,
                max_priority_fee_per_gas,
                gas_used.tx_gas_used(),
                base_fee_per_gas,
            );
            self.current_block_validator_fees = self
                .current_block_validator_fees
                .checked_add(validator_fee)
                .ok_or_else(|| {
                    BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                        "validator fee accumulator overflow".into(),
                    ))
                })?;
        }

        Ok(result)
    }
}
