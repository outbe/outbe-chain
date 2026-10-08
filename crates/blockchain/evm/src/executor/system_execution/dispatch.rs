//! Reserved begin-zone system transactions and OCOMP system carriers.

use super::super::*;

struct ValidatedReservedSystemTx {
    body_index: usize,
    expected_phase: SystemTxKind,
    finalized_summary: Option<AccountedParentArtifact>,
    visible_base_gas: u64,
    visible_gas_limit: u64,
    proposer: Address,
}

struct ReservedSystemReceiptContext {
    body_index: usize,
    expected_phase: SystemTxKind,
    block_number: u64,
    visible_base_gas: u64,
    visible_gas_limit: u64,
    ce_gas_limit: u64,
    compressed_entities_gas: u64,
    has_boundary_outcome: bool,
    has_tee_bootstrap: bool,
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
    pub(in crate::executor) fn execute_reserved_system_tx<R, F>(
        &mut self,
        recovered: R,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        let tx = recovered.tx();
        let signer = *recovered.signer();
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let block_artifacts = decode_outbe_block_artifacts(self.block_extra_data.as_ref())
            .map_err(|error| BlockExecutionError::msg(error.to_string()))?;

        // Witness validate-without-reexec: if Phase 1
        // was already committed in `apply_pre_execution_changes::apply_phase1_commit_in_preexec`,
        // the cursor carries the cached witness `tx_hash`. Body[0] in the
        // main tx loop is the proposer-supplied Phase 1 tx. Validate that it
        // matches the cache (signature hash) and skip re-execution.
        // Receipt + state already exist from the pre-exec commit.
        if self.consume_preexecuted_phase1_witness(tx, &block_artifacts)? {
            return Ok(None);
        }

        // cursor-driven phase routing replaces the previous
        // `self.inner.receipts.len()` derivation. The cursor was
        // initialised in `apply_pre_execution_changes` and advances
        // exactly once per consumed begin-zone system tx (see the
        // `advance_after_commit` call below).
        let ValidatedReservedSystemTx {
            body_index,
            expected_phase,
            finalized_summary,
            visible_base_gas,
            visible_gas_limit,
            proposer,
        } = self.validate_reserved_system_tx((tx, signer), block_number, &block_artifacts)?;

        if expected_phase == SystemTxKind::HookEvents {
            return self.commit_reserved_hook_receipt(tx, &block_artifacts, visible_base_gas);
        }

        let phase_context = PreloadedSystemTxContext {
            proposer,
            finalized_summary,
            allow_boundary_proposer: self.boundary_allows_proposer(&block_artifacts, proposer),
            // same VRF-proof-hash plumbing as the
            // pre-exec commit path. The preflight caches it. The fallback
            // value `B256::ZERO` is used only when the preflight was
            // skipped (which never co-occurs with this main-loop
            // path entering Phase 1 in production).
            canonical_vrf_proof_hash: self.verified_phase1_vrf_proof_hash.unwrap_or(B256::ZERO),
        };
        // An EVM result failure (`Revert` / `Halt`) in a phase where
        // `revert_fails_block()` is true fails the whole block. In a soft phase
        // (RewardsGemDelivery, OracleSlashWindow), the failure becomes a
        // `status=0` synthetic receipt with one `OutbeFailure(code, reason)`
        // log emitted from `OUTBE_SYSTEM_TX_ADDRESS`. revm did not commit the call, so
        // no state change leaks. Raw `Err` from the system-call engine remains fatal
        // because upstream revm documents that the journal may be inconsistent on that
        // path. Body-parity validation above (decode / phase / calldata / signature /
        // signer) also remains fatal: those are validator-side checks that the proposer
        // never produces for itself.
        let (ce_gas_limit, gas_window) = open_reserved_gas_window(
            &self.compressed_entities_scope,
            body_index,
            expected_phase,
            (visible_base_gas, visible_gas_limit),
        )?;
        let transact_outcome = with_preloaded_system_tx_context(phase_context, || {
            self.inner.evm.transact_system_call(
                outbe_primitives::addresses::SYSTEM_ADDRESS,
                outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                tx.input().clone(),
            )
        });
        // precompute the boundary-outcome flag so the cursor
        // advance below stays consistent with the resolved expected set
        // for this block (block 1 always carries the boundary outcome
        // under V2, and other blocks depend on the header artifact).
        let has_boundary_outcome = matches!(
            block_artifacts.consensus_header_artifact,
            Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
        );
        let has_tee_bootstrap = self.block_has_tee_bootstrap();
        // Only EVM result failures use the soft-failure receipt path.
        // Raw engine/provider `Err` was handled above as fatal.
        let result = transact_outcome.map_err(|error| {
            let reason = format!(
                "system tx {expected_phase:?} execution failed at body_index={body_index}: {error}"
            );
            if outbe_primitives::projection::ExecutionReadCancelled::find(&error).is_none() {
                tracing::error!(target: "outbe::executor", %reason);
            }
            BlockExecutionError::other(error)
        })?;
        let compressed_entities_gas = gas_window.gas_used().map_err(|error| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!(
                    "read CE gas window for {expected_phase:?} at body_index={body_index}: {error}"
                )
                .into(),
            ))
        })?;
        drop(gas_window);
        let receipt_context = ReservedSystemReceiptContext {
            body_index,
            expected_phase,
            block_number,
            visible_base_gas,
            visible_gas_limit,
            ce_gas_limit,
            compressed_entities_gas,
            has_boundary_outcome,
            has_tee_bootstrap,
        };
        if !result.result.is_success() {
            return self.commit_reserved_failure_receipt(tx, &result.result, receipt_context);
        }
        let output = EthTxResult {
            result,
            blob_gas_used: 0,
            tx_type: tx.tx_type(),
        };
        self.commit_reserved_success_receipt(output, receipt_context, f)
    }

    fn commit_reserved_hook_receipt(
        &mut self,
        tx: &TransactionSigned,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
        visible_base_gas: u64,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let has_boundary_outcome = matches!(
            block_artifacts.consensus_header_artifact,
            Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
        );
        let has_tee_bootstrap = self.block_has_tee_bootstrap();
        let logs = std::mem::take(&mut self.whitelisted_hook_event_logs);
        let commit_outcome = self
            .push_hook_events_receipt(tx.tx_type(), logs, visible_base_gas)
            .map(Some);
        if commit_outcome.is_ok() {
            self.system_tx_phase_cursor =
                self.system_tx_phase_cursor.advance_after_commit_with_ocomp(
                    has_boundary_outcome,
                    has_tee_bootstrap,
                    self.ocomp_lifecycle_active,
                );
        }
        commit_outcome
    }

    fn commit_reserved_success_receipt<F>(
        &mut self,
        output: EthTxResult<E::HaltReason, reth_ethereum::TxType>,
        context: ReservedSystemReceiptContext,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        let ReservedSystemReceiptContext {
            visible_base_gas,
            compressed_entities_gas,
            visible_gas_limit,
            has_boundary_outcome,
            has_tee_bootstrap,
            ..
        } = context;
        if !f(&output).should_commit() {
            // Cursor does not advance: the caller chose not to commit,
            // so the body-index slot remains owned by this phase.
            return Ok(None);
        }
        let commit_outcome = self
            .commit_system_transaction(
                output,
                visible_base_gas,
                compressed_entities_gas,
                visible_gas_limit,
            )
            .map(Some);
        if commit_outcome.is_ok() {
            self.system_tx_phase_cursor =
                self.system_tx_phase_cursor.advance_after_commit_with_ocomp(
                    has_boundary_outcome,
                    has_tee_bootstrap,
                    self.ocomp_lifecycle_active,
                );
        }
        commit_outcome
    }

    fn commit_reserved_failure_receipt(
        &mut self,
        tx: &TransactionSigned,
        result: &ExecutionResult<HaltReason>,
        context: ReservedSystemReceiptContext,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let ReservedSystemReceiptContext {
            body_index,
            expected_phase,
            block_number,
            visible_base_gas,
            visible_gas_limit,
            ce_gas_limit,
            compressed_entities_gas,
            has_boundary_outcome,
            has_tee_bootstrap,
        } = context;
        tracing::error!(
            target: "outbe::executor",
            ?expected_phase,
            body_index,
            block_number,
            gas_used = result.tx_gas_used(),
            gas_limit = tx.gas_limit(),
            result = ?result,
            "system tx failed"
        );
        let code = system_tx_failure_code_for_result(result);
        // a revert/halt in a consensus- or economic-critical
        // begin-zone phase is a hard block failure, not a soft-receipt
        // skip. Their work is one-shot and never retried, so swallowing a
        // revert permanently loses it (stranded fee escrow, dropped
        // emission/reshare, unrecorded parent accounting). The revert is a
        // deterministic function of committed chain state, so every
        // validator rejects the same block identically. There is no
        // state-root split. Non-critical phases (RewardsGemDelivery,
        // OracleSlashWindow) keep the soft-receipt skip for failures that fit within the
        // aggregate internal-work budget. An OOG consumes the full
        // system-call gas limit and therefore remains a hard aggregate
        // budget failure once earlier mandatory phases have run.
        if expected_phase.revert_fails_block() {
            let reason = format!(
                "critical system tx {expected_phase:?} did not succeed (revert/halt) at \
             body_index={body_index}, block_number={block_number}, \
             failure_code={code}: {:?}",
                result
            );
            tracing::error!(target: "outbe::executor", %reason, "critical begin-zone phase did not succeed; failing block");
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(reason.into()),
            ));
        }
        let reason = format!(
            "system tx {expected_phase:?} did not succeed at body_index={body_index}: {:?}",
            result
        );
        let tx_type = tx.tx_type();
        let receipt_ce_gas = if matches!(
            result,
            ExecutionResult::Halt {
                reason: HaltReason::OutOfGas(_),
                ..
            }
        ) {
            ce_gas_limit
        } else {
            compressed_entities_gas
        };
        let gas_output = self.push_system_failure_receipt(SystemFailureReceiptInput {
            tx_type,
            log_address: outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            code,
            reason,
            visible_base_gas,
            compressed_entities_gas: receipt_ce_gas,
            signed_gas_limit: visible_gas_limit,
            internal_gas_used: result.tx_gas_used(),
        })?;
        self.system_tx_phase_cursor = self.system_tx_phase_cursor.advance_after_commit_with_ocomp(
            has_boundary_outcome,
            has_tee_bootstrap,
            self.ocomp_lifecycle_active,
        );
        Ok(Some(gas_output))
    }

    fn consume_preexecuted_phase1_witness(
        &mut self,
        tx: &TransactionSigned,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<bool, BlockExecutionError> {
        if let crate::system_tx::SystemTxPhase::Phase1Preexecuted {
            tx_hash: cached_hash,
            ..
        } = self.system_tx_phase_cursor
        {
            if !cached_hash.is_zero() {
                if tx.signature_hash() != cached_hash {
                    return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!(
                            "Phase 1 body[0] witness signature_hash mismatch: expected {cached_hash}, got {}",
                            tx.signature_hash()
                        )
                        .into(),
                    ),
                ));
                }
                // Advance cursor past Phase 1. The mandatory LateFinalizeCredits
                // phase (body_index=1) is next.
                let has_boundary_outcome = matches!(
                    block_artifacts.consensus_header_artifact,
                    Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
                );
                let has_tee_bootstrap = self.block_has_tee_bootstrap();
                self.system_tx_phase_cursor =
                    self.system_tx_phase_cursor.advance_after_commit_with_ocomp(
                        has_boundary_outcome,
                        has_tee_bootstrap,
                        self.ocomp_lifecycle_active,
                    );
                // Returning true makes the caller skip a further commit: pre-exec
                // already pushed receipt[0] and committed state. The block
                // builder still keeps this validated witness in body[0].
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn validate_reserved_system_tx(
        &self,
        transaction: (&TransactionSigned, Address),
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<ValidatedReservedSystemTx, BlockExecutionError> {
        let (tx, signer) = transaction;
        let (
            body_index,
            expected_phase,
            expected_input,
            finalized_summary,
            visible_base_gas,
            planned_gas_limit,
        ) = self.expected_system_tx_for_cursor(block_number, block_artifacts)?;
        validate_reserved_system_input(tx, body_index, (expected_phase, &expected_input))?;

        self.validate_reserved_system_envelope(
            tx,
            (body_index, expected_phase, planned_gas_limit),
            block_number,
        )?;
        let visible_gas_limit = tx.gas_limit();

        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());
        if signer != proposer {
            return Err(BlockExecutionError::Internal(
            InternalBlockExecutionError::Other(
                format!(
                    "system tx signer mismatch at body_index={body_index} for {expected_phase:?}: expected proposer {proposer}, got {signer}"
                )
                .into(),
            ),
        ));
        }

        Ok(ValidatedReservedSystemTx {
            body_index,
            expected_phase,
            finalized_summary,
            visible_base_gas,
            visible_gas_limit,
            proposer,
        })
    }

    fn validate_reserved_system_envelope(
        &self,
        tx: &TransactionSigned,
        expected: (usize, SystemTxKind, u64),
        block_number: u64,
    ) -> Result<(), BlockExecutionError> {
        let (body_index, expected_phase, planned_gas_limit) = expected;
        let ordinal = body_index.try_into().map_err(|_| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!("system tx body_index {body_index} exceeds u8 range").into(),
            ))
        })?;
        let unsigned = build_unsigned_system_tx_with_gas_limit(
            outbe_primitives::system_tx::SystemTxEnvelopeInput {
                kind: expected_phase,
                ordinal,
                block_number,
                chain_id: self.inner.evm.chain_id(),
                calldata: tx.input().clone(),
                gas_limit: planned_gas_limit,
            },
        )
        .map_err(|error| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!("build expected system tx at body_index={body_index}: {error}").into(),
            ))
        })?;
        if tx.signature_hash() != unsigned.signature_hash() {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!(
                        "system tx signature_hash mismatch at body_index={body_index} for {expected_phase:?}"
                    )
                    .into(),
                ),
            ));
        }
        Ok(())
    }

    /// `Ok(None)` is an ordinary transaction. A classified carrier is authorized
    /// here, before execution changes its fee fields.
    pub(in crate::executor) fn ocomp_system_carrier_candidate<R>(
        &mut self,
        recovered: &R,
    ) -> Result<Option<OcompSystemCarrierCandidate>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
    {
        let tx = recovered.tx();
        let signer = *recovered.signer();
        let Some(candidate) = classify_ocomp_system_carrier(
            OcompSystemCarrierView {
                is_eip1559: tx.tx_type() == alloy_consensus::TxType::Eip1559,
                to: tx.to(),
                value: tx.value(),
                input: tx.input().as_ref(),
                gas_limit: tx.gas_limit(),
                max_fee_per_gas: tx.max_fee_per_gas(),
                max_priority_fee_per_gas: tx.max_priority_fee_per_gas(),
            },
            &outbe_ocomp_protocol::profile::poc_schema_limits(),
        )
        .map_err(|error| {
            BlockExecutionError::msg(format!("invalid OCOMP system carrier: {error}"))
        })?
        else {
            return Ok(None);
        };
        if !self.ocomp_lifecycle_active {
            return Err(BlockExecutionError::msg(
                "OCOMP system carrier is not active for this block",
            ));
        }
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let timestamp = self.inner.evm.block().timestamp().saturating_to::<u64>();
        let chain_id = self.inner.evm.chain_id();
        let proposer = self.inner.evm.block().beneficiary();
        let authorized = {
            let db = self.inner.evm.db_mut();
            let ctx =
                BlockContext::new_with_genesis_hash(outbe_primitives::block::BlockContextInput {
                    block_number,
                    timestamp,
                    chain_id,
                    genesis_hash: self.genesis_hash,
                    proposer,
                    validators: Vec::new(),
                });
            let mut provider = DirectStorageProvider::new(db, ctx);
            let storage = StorageHandle::new(&mut provider);
            match candidate {
                OcompSystemCarrierCandidate::ResultVote { prefix } => {
                    outbe_metadosis::resolve_historical_result_vote_carrier_signer(
                        storage,
                        &prefix,
                        signer,
                        &outbe_ocomp_protocol::profile::poc_schema_limits(),
                    )
                }
                OcompSystemCarrierCandidate::NodMaterialization { .. } => {
                    outbe_validatorset::contract::ValidatorSet::new(storage)
                        .resolve_validator_for_role(
                            signer,
                            outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp,
                        )
                }
            }
        }
        .map_err(|error| {
            BlockExecutionError::msg(format!(
                "OCOMP system carrier authorization failed: {error}"
            ))
        })?;
        if authorized.is_none() {
            return Err(BlockExecutionError::msg(
                "OCOMP system carrier signer is not authorized",
            ));
        }
        Ok(Some(candidate))
    }

    pub(in crate::executor) fn execute_ocomp_system_carrier<R, F>(
        &mut self,
        candidate: OcompSystemCarrierCandidate,
        mut tx_env: TxEnv,
        recovered: R,
        f: F,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        R: RecoveredTx<TransactionSigned>,
        F: FnOnce(&EthTxResult<E::HaltReason, reth_ethereum::TxType>) -> CommitChanges,
    {
        let tx = recovered.tx();
        let signed_gas_limit = tx.gas_limit();
        let tx_type = tx.tx_type();
        let snapshot = self.inner.evm.enable_zero_fee_overrides();
        tx_env.gas_limit = OCOMP_SYSTEM_CARRIER_INTERNAL_GAS_LIMIT;
        tx_env.gas_price = 0;
        tx_env.gas_priority_fee = Some(0);
        let execution = self.inner.execute_transaction_without_commit(WithTxEnv {
            tx_env,
            tx: Arc::new(recovered),
        });
        self.inner.evm.restore_zero_fee_overrides(snapshot);
        let output = execution?;
        let allowed_failed_receipt = match candidate {
            OcompSystemCarrierCandidate::ResultVote { .. } => {
                is_ocomp_deadline_passed_revert(&output.result.result)
            }
            OcompSystemCarrierCandidate::NodMaterialization { .. } => {
                is_nod_materialization_soft_revert(&output.result.result)
            }
        };
        if !output.result.result.is_success() && !allowed_failed_receipt {
            return Err(BlockExecutionError::msg(format!(
                "OCOMP system carrier execution did not succeed: {:?}",
                output.result.result
            )));
        }
        if !f(&output).should_commit() {
            return Ok(None);
        }
        debug_assert_eq!(output.tx_type, tx_type);
        self.commit_system_transaction(output, 0, 0, signed_gas_limit)
            .map(Some)
    }
}

fn validate_reserved_system_input(
    tx: &TransactionSigned,
    body_index: usize,
    expected: (SystemTxKind, &SystemTxInputV2),
) -> Result<(), BlockExecutionError> {
    let (expected_phase, expected_input) = expected;
    let actual_input = SystemTxInputV2::decode(tx.input().as_ref()).map_err(|error| {
        BlockExecutionError::Internal(InternalBlockExecutionError::Other(
            format!("decode system tx at body_index={body_index}: {error}").into(),
        ))
    })?;
    let actual_phase = actual_input.kind();
    if actual_phase != expected_phase {
        return Err(BlockExecutionError::Internal(
            InternalBlockExecutionError::Other(
                format!(
                    "system tx phase mismatch at body_index={body_index}: expected {expected_phase:?}, got {actual_phase:?}"
                )
                .into(),
            ),
        ));
    }
    if &actual_input != expected_input {
        return Err(BlockExecutionError::Internal(
            InternalBlockExecutionError::Other(
                format!(
                    "system tx calldata mismatch at body_index={body_index} for {expected_phase:?}"
                )
                .into(),
            ),
        ));
    }

    Ok(())
}

fn open_reserved_gas_window(
    scope: &ExecutionScope,
    body_index: usize,
    expected_phase: SystemTxKind,
    visible_gas: (u64, u64),
) -> Result<(u64, outbe_compressed_entities::ExplicitGasWindow<'_>), BlockExecutionError> {
    let (visible_base_gas, visible_gas_limit) = visible_gas;
    let ce_gas_limit = visible_gas_limit
        .checked_sub(visible_base_gas)
        .ok_or_else(|| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!(
                    "system tx signed gas below visible base at body_index={body_index}: \
             signed={visible_gas_limit}, visible_base={visible_base_gas}"
                )
                .into(),
            ))
        })?;
    let gas_window = scope
        .begin_explicit_gas_window(ce_gas_limit)
        .map_err(|error| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!(
                    "open CE gas window for {expected_phase:?} at body_index={body_index}: {error}"
                )
                .into(),
            ))
        })?;
    Ok((ce_gas_limit, gas_window))
}
