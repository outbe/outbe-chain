//! Block open: Ethereum pre-execution, runtime markers, and Outbe begin hooks.

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
    pub(in crate::executor) fn apply_outbe_pre_execution(
        &mut self,
    ) -> Result<(), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        self.inner.evm.set_executor_owned(true);
        crate::executor::witness::clear_detached_trace_witness();
        let timestamp = outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(ExecutorValidationError::from)?;
        validate_outbe_withdrawals(self.inner.ctx.withdrawals.as_deref())
            .map_err(ExecutorValidationError::invalid_withdrawals)?;

        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let beneficiary = self.inner.evm.block().beneficiary();
        let regime = outbe_primitives::block::BlockHeightRegime::for_block(block_number);

        self.decoded_block_artifacts = None;
        self.cached_phase1_input = None;
        self.resolved_begin_zone = None;
        self.verified_late_credit_signers = None;
        self.fsm = outbe_primitives::system_tx::BlockExecutionFsm::for_block(
            block_number,
            false,
            false,
            false,
        );

        // Initialise the begin-zone phase cursor for this block
        // BEFORE any pre-exec mutation that could affect routing. Block 1
        // Genesis bootstrap skips Phase 1 and starts at BoundaryOutcome.
        // Block `n` with `n > GENESIS_BOOTSTRAP_BLOCK_NUMBER` enters Phase 1
        // with a zero placeholder tx_hash. The Phase 1 preflight overwrites it
        // once `verify_v2_proof` returns Ok and the system tx is committed in
        // pre-execution.
        self.system_tx_phase_cursor = crate::system_tx::SystemTxPhase::initial_for_regime(regime);
        crate::genesis::validate_block_beneficiary(regime, beneficiary)?;
        if let Some(error) = &self.system_layout_error {
            return Err(ExecutorValidationError::invalid_system_tx_layout(error).into());
        }

        // 1. Standard Ethereum pre-execution (blockhashes, beacon root, state clear flag).
        self.inner.apply_pre_execution_changes()?;

        // 2. Deploy 0xEF marker bytecode to all Outbe runtime addresses.
        //    Without bytecode these accounts are "empty" under EIP-161 and their
        //    storage is silently discarded during state root calculation.
        //    State::commit notifies reth's parallel state root task.
        self.preserve_runtime_markers()?;

        // Pending-block RPC executes against its isolated EVM state.
        if !self.execute_outbe_block_hooks {
            if regime.has_begin_zone_system_txs() {
                self.init_rpc_direct_witness_state();
                self.inner.evm.set_witness_state(self.witness_state.clone());
                crate::executor::witness::install_detached_trace_witness_with_meta(
                    self.chain_id,
                    block_number,
                    timestamp,
                    self.witness_state.clone(),
                    crate::executor::witness::DetachedTraceExecutionMeta::default(),
                );
            }
            return Ok(());
        }

        // 4. Extract block context before taking a mutable DB borrow.
        let block_artifacts = decode_outbe_block_artifacts(self.block_extra_data.as_ref())
            .map_err(ExecutorValidationError::block_artifacts)?;
        self.decoded_block_artifacts = Some(block_artifacts.clone());
        let has_boundary_outcome = matches!(
            block_artifacts.consensus_header_artifact,
            Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
        );
        let has_tee_bootstrap = self.block_has_tee_bootstrap();
        let has_publication = block_artifacts.radicle_publication.is_some();
        self.fsm = outbe_primitives::system_tx::BlockExecutionFsm::for_block(
            block_number,
            has_boundary_outcome,
            has_tee_bootstrap,
            has_publication,
        );
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());
        let allow_boundary_proposer = self.boundary_allows_proposer(&block_artifacts, proposer);
        crate::genesis::validate_genesis_enclave_witness(regime, &block_artifacts)?;
        if regime.has_begin_zone_system_txs() {
            self.validate_proposer_identity(proposer, allow_boundary_proposer)?;
            self.init_block_witness_state(block_number, proposer, &block_artifacts)?;
        }

        // Phase 1 `verify_v2_proof` preflight.
        self.verify_phase1_in_preexec(block_number, &block_artifacts)?;

        // Late-finalize-credit BLS aggregates are FATAL-verified here.
        self.verify_late_finalize_credits_in_preexec(block_number, &block_artifacts)?;

        let prefix_witness_scope = self.enter_prefix_witness_scope()?;
        let preexec_outcome = (|| -> Result<(), BlockExecutionError> {
            self.apply_phase1_commit_in_preexec(block_number, &block_artifacts)?;

            // 4. Fresh bootstrap validation data from consensus config.
            let genesis_validators = self
                .bridge
                .as_ref()
                .and_then(|b| b.peek_genesis_validators());

            // 5. Run boundary epoch counter preparation and genesis state validation.
            let witness_state = self.witness_state.clone();
            let base_extra_data = self.block_extra_data.clone();
            let ctx = self.block_context(block_number, timestamp, proposer);
            let db = self.inner.evm.db_mut();
            let ctx = build_block_context(db, ctx)?;
            let _ = run_atomic_storage_hooks_with_pre_flush(
                db,
                ctx,
                |hook_ctx| -> outbe_primitives::error::Result<()> {
                    if let Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) =
                        block_artifacts.consensus_header_artifact.as_ref()
                    {
                        prepare_boundary_epoch_counters(
                            hook_ctx.storage.clone(),
                            boundary,
                            block_number,
                        )?;
                    }
                    crate::genesis::validate_genesis_state_for_regime(
                        hook_ctx.block.regime(),
                        hook_ctx.storage.clone(),
                        genesis_validators.as_ref(),
                    )
                },
                || {
                    crate::executor::witness::finalize_prefix_witness_with(
                        witness_state.as_ref(),
                        base_extra_data.as_ref(),
                    )
                },
            )?;
            Ok(())
        })();
        drop(prefix_witness_scope);
        self.check_no_witness_fault()?;
        preexec_outcome?;
        // Provider dropped here - mutable DB borrow released.

        let phase1_preexecuted = matches!(
            self.system_tx_phase_cursor,
            crate::system_tx::SystemTxPhase::Phase1Preexecuted { tx_hash, .. } if !tx_hash.is_zero()
        );
        crate::executor::witness::install_detached_trace_witness_with_meta(
            self.chain_id,
            block_number,
            timestamp,
            self.witness_state.clone(),
            crate::executor::witness::DetachedTraceExecutionMeta {
                expected_begin_inputs: self
                    .expected_begin_system_txs
                    .iter()
                    .map(|tx| tx.tx().input().clone())
                    .collect(),
                expected_tx_count: self.inner.ctx.tx_count_hint,
                phase1_preexecuted,
                system_tx_context: Some(PreloadedSystemTxContext {
                    proposer,
                    finalized_summary: None,
                    allow_boundary_proposer,
                    canonical_vrf_proof_hash: self
                        .verified_phase1_vrf_proof_hash
                        .unwrap_or(B256::ZERO),
                    verified_late_credit_signers: self.verified_late_credit_signers.clone(),
                    gas_limit_override: None,
                    remaining_system_gross_gas:
                        outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT
                            .saturating_sub(self.system_tx_gross_gas),
                    block_gas_limit: self.inner.evm.block().gas_limit(),
                    user_gas_used: 0,
                }),
            },
        );

        Ok(())
    }

    fn preserve_runtime_markers(&mut self) -> Result<(), BlockExecutionError> {
        use revm::state::{Account, Bytecode, EvmState};
        // Single source of truth (see `marker_addresses` + its superset test).
        let precompile_addresses = marker_addresses::OUTBE_RUNTIME_MARKER_ADDRESSES;

        let db = self.inner.evm.db_mut();
        let mut marker_state = EvmState::default();

        for addr in precompile_addresses {
            let info = db
                .basic(addr)
                .map_err(|e| {
                    BlockExecutionError::other(
                        crate::errors::ExecutorStorageError::load_precompile_account(addr, e),
                    )
                })?
                .unwrap_or_default();
            if info.is_empty_code_hash() {
                let code = Bytecode::new_legacy([0xef].into());
                let mut new_info = info;
                new_info.code_hash = code.hash_slow();
                new_info.code = Some(code);
                let mut account: Account = new_info.into();
                account.mark_touch();
                marker_state.insert(addr, account);
            }
        }

        if !marker_state.is_empty() {
            self.inner.evm.db_mut().commit(marker_state);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(in crate::executor) fn run_atomic_storage_hooks<DB, F>(
    db: &mut DB,
    ctx: BlockContext,
    hooks: F,
) -> Result<(AddressMap<Account>, Vec<Log>), BlockExecutionError>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    F: FnOnce(&BlockRuntimeContext) -> outbe_primitives::error::Result<()>,
{
    run_atomic_storage_hooks_with_pre_flush(db, ctx, hooks, || Ok(()))
}

pub(in crate::executor) fn run_atomic_storage_hooks_with_pre_flush<DB, F, G>(
    db: &mut DB,
    ctx: BlockContext,
    hooks: F,
    pre_flush: G,
) -> Result<(AddressMap<Account>, Vec<Log>), BlockExecutionError>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    F: FnOnce(&BlockRuntimeContext) -> outbe_primitives::error::Result<()>,
    G: FnOnce() -> Result<(), BlockExecutionError>,
{
    let mut provider = DirectStorageProvider::new(db, ctx.clone());
    let storage = StorageHandle::new(&mut provider);
    let runtime_ctx = BlockRuntimeContext::new(ctx, storage.clone());

    let result = storage.with_checkpoint(|| hooks(&runtime_ctx));

    result.map_err(|error| match error {
        PrecompileError::Revert(_)
        | PrecompileError::RevertBytes(_)
        | PrecompileError::Halt(_)
        | PrecompileError::OutOfGas
        | PrecompileError::WriteProtection
        | PrecompileError::BodyReadCorruption(_) => BlockExecutionError::from(
            crate::errors::ExecutorValidationError::hook_precompile(error),
        ),
        error
            if error.is_node_local()
                || matches!(
                    error,
                    PrecompileError::Fatal(_)
                        | PrecompileError::FatalWithCause(_)
                        | PrecompileError::SubCall(_)
                        | PrecompileError::Unsupported
                ) =>
        {
            BlockExecutionError::other(error)
        }
        error => BlockExecutionError::from(
            crate::errors::ExecutorValidationError::hook_precompile(error),
        ),
    })?;

    pre_flush()?;

    provider.flush().map_err(|e| {
        BlockExecutionError::other(crate::errors::ExecutorStorageError::hook_flush(e))
    })?;

    let changes = provider.take_committed_changes();
    let events = provider.take_events();
    Ok((changes, events))
}
