use super::super::*;

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
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
        if !self.ocomp_lifecycle_active {
            return Err(BlockExecutionError::msg(
                "OCOMP terminal request is not active for this block",
            ));
        }
        if self.system_tx_phase_cursor != crate::system_tx::SystemTxPhase::UserTxs {
            return Err(BlockExecutionError::msg(
                "OCOMP terminal request arrived before the begin zone completed",
            ));
        }
        if self.expected_end_system_txs.len() > 1 {
            return Err(BlockExecutionError::msg(
                "OCOMP lifecycle permits exactly one end-zone system transaction",
            ));
        }

        let tx = recovered.tx();
        if let Some(expected) = self.expected_end_system_txs.first() {
            if expected.tx().tx_hash() != tx.tx_hash() {
                return Err(BlockExecutionError::msg(
                    "terminal system transaction differs from the validated block suffix",
                ));
            }
        }
        let input = SystemTxInputV2::decode(tx.input().as_ref()).map_err(|error| {
            BlockExecutionError::msg(format!("decode terminal system tx input: {error}"))
        })?;
        if input != SystemTxInputV2::OcompTerminalRequest {
            return Err(BlockExecutionError::msg(
                "end-zone system transaction is not OcompTerminalRequest",
            ));
        }

        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let block_artifacts = decode_outbe_block_artifacts(self.block_extra_data.as_ref())
            .map_err(|error| BlockExecutionError::msg(error.to_string()))?;
        let begin_count = self
            .begin_block_system_tx_inputs(block_number, &block_artifacts)?
            .len();
        let ordinal = begin_count.try_into().map_err(|_| {
            BlockExecutionError::msg(format!(
                "terminal system tx ordinal {begin_count} exceeds u8 range"
            ))
        })?;
        let unsigned = build_unsigned_system_tx(
            SystemTxKind::OcompTerminalRequest,
            ordinal,
            block_number,
            self.inner.evm.chain_id(),
            tx.input().clone(),
        )
        .map_err(|error| {
            BlockExecutionError::msg(format!("build expected terminal system tx: {error}"))
        })?;
        if tx.signature_hash() != unsigned.signature_hash() {
            return Err(BlockExecutionError::msg(
                "terminal system tx signature hash mismatch",
            ));
        }
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());
        let signer = *recovered.signer();
        if signer != proposer {
            return Err(BlockExecutionError::msg(format!(
                "terminal system tx signer mismatch: expected proposer {proposer}, got {signer}"
            )));
        }

        let tx_type = tx.tx_type();
        let signed_gas_limit = tx.gas_limit();
        let intrinsic_gas = crate::system_tx::system_tx_intrinsic_gas(tx.input().as_ref())
            .map_err(|error| {
                BlockExecutionError::msg(format!("terminal system tx intrinsic gas: {error}"))
            })?;

        // All ordinary and Ethereum post-execution changes are complete. The
        // terminal request consumes exact provisional roots while CE remains
        // active, so its failure path can retire the WWD Tribute partition as
        // the last CE mutation. The sole committed seal follows the terminal
        // decision.
        self.apply_outbe_ethereum_post_execution()?;
        self.preview_compressed_entities()?;

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
        .map_err(|error| {
            BlockExecutionError::msg(format!(
                "terminal system tx execution failed at block {block_number}: {error}"
            ))
        })?;
        if !result.result.is_success() {
            return Err(BlockExecutionError::msg(format!(
                "critical terminal system tx did not succeed at block {block_number}: {:?}",
                result.result
            )));
        }

        let output = EthTxResult {
            result,
            blob_gas_used: 0,
            tx_type,
        };
        if !commit(&output).should_commit() {
            return Err(BlockExecutionError::msg(
                "terminal system transaction cannot execute without commit",
            ));
        }
        let gas = self.commit_system_transaction(output, intrinsic_gas, 0, signed_gas_limit)?;
        self.finalize_compressed_entities()?;
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
}
