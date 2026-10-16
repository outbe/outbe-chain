use super::super::*;

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Spec: Into<revm::primitives::hardfork::SpecId>,
    E::Error: std::fmt::Display,
{
    pub(in crate::executor) fn execute_ocomp_terminal_request<R>(
        &mut self,
        recovered: R,
        commit: impl FnOnce(&EthTxResult<E::HaltReason, alloy_consensus::TxType>) -> CommitChanges,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
    {
        use crate::errors::ExecutorValidationError;

        self.validate_terminal_phase()?;

        let tx = recovered.tx();
        self.validate_terminal_input(tx)?;

        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let block_artifacts = decode_outbe_block_artifacts(self.block_extra_data.as_ref())
            .map_err(ExecutorValidationError::block_artifacts)?;
        let begin_count = self
            .begin_block_system_tx_inputs(block_number, &block_artifacts)?
            .len();
        let ordinal = begin_count.try_into().map_err(|_| {
            ExecutorValidationError::TerminalSystemTxOrdinalExceedsU8 {
                ordinal: begin_count,
            }
        })?;
        let unsigned = build_unsigned_system_tx(
            SystemTxKind::OcompTerminalRequest,
            ordinal,
            block_number,
            self.inner.evm.chain_id(),
            tx.input().clone(),
        )
        .map_err(ExecutorValidationError::build_expected_terminal_system_tx)?;
        if tx.signature_hash() != unsigned.signature_hash() {
            return Err(ExecutorValidationError::TerminalSystemTxSignatureHashMismatch.into());
        }
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());
        let signer = *recovered.signer();
        if signer != proposer {
            return Err(ExecutorValidationError::TerminalSystemTxSignerMismatch {
                proposer,
                signer,
            }
            .into());
        }

        let tx_type = tx.tx_type();
        let signed_gas_limit = tx.gas_limit();
        let intrinsic_gas = crate::system_tx::system_tx_intrinsic_gas(tx.input().as_ref())
            .map_err(ExecutorValidationError::terminal_system_tx_intrinsic_gas)?;

        // Complete ordinary state changes before the terminal request freezes
        // the day inputs. The finalized block state root authenticates them.
        self.apply_outbe_ethereum_post_execution()?;

        let phase_context = PreloadedSystemTxContext {
            proposer,
            finalized_summary: None,
            allow_boundary_proposer: self.boundary_allows_proposer(&block_artifacts, proposer),
            canonical_vrf_proof_hash: B256::ZERO,
        };
        let result = with_preloaded_system_tx_context(phase_context, || {
            self.inner.evm.transact_system_call(
                outbe_primitives::addresses::SYSTEM_ADDRESS,
                outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                tx.input().clone(),
            )
        })
        .map_err(BlockExecutionError::other)?;
        if !result.result.is_success() {
            return Err(
                ExecutorValidationError::critical_terminal_system_tx_did_not_succeed(
                    block_number,
                    &result.result,
                )
                .into(),
            );
        }

        let output = EthTxResult {
            result,
            blob_gas_used: 0,
            tx_type,
        };
        if !commit(&output).should_commit() {
            return Err(ExecutorValidationError::TerminalSystemTxWithoutCommit.into());
        }
        let gas = self.commit_system_transaction(output, intrinsic_gas, 0, signed_gas_limit)?;
        self.ocomp_terminal_request_consumed = true;
        let execution_origin = if self.block_hash.is_some() {
            "canonical"
        } else {
            "proposal"
        };
        tracing::info!(
            target: "outbe::ocomp::trace",
            "OCOMP_TRACE_V1 kind=terminal_request_committed origin={execution_origin} \
             block={block_number} tx={:#x}",
            tx.tx_hash()
        );
        Ok(Some(gas))
    }

    fn validate_terminal_phase(&self) -> Result<(), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        if !self.ocomp_lifecycle_active {
            return Err(ExecutorValidationError::OcompTerminalRequestNotActive.into());
        }
        if self.system_tx_phase_cursor != crate::system_tx::SystemTxPhase::UserTxs {
            return Err(
                ExecutorValidationError::OcompTerminalRequestBeforeBeginZoneCompleted.into(),
            );
        }
        if self.expected_end_system_txs.len() > 1 {
            return Err(ExecutorValidationError::MultipleEndZoneSystemTransactions.into());
        }

        Ok(())
    }

    fn validate_terminal_input(&self, tx: &TransactionSigned) -> Result<(), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        if let Some(expected) = self.expected_end_system_txs.first() {
            if expected.tx().tx_hash() != tx.tx_hash() {
                return Err(
                    ExecutorValidationError::TerminalSystemTxDiffersFromValidatedSuffix.into(),
                );
            }
        }
        let input = SystemTxInputV2::decode(tx.input().as_ref())
            .map_err(ExecutorValidationError::decode_terminal_system_tx_input)?;
        if input != SystemTxInputV2::OcompTerminalRequest {
            return Err(ExecutorValidationError::EndZoneSystemTxNotOcompTerminalRequest.into());
        }

        Ok(())
    }
}
