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
        validate_outbe_withdrawals(self.inner.ctx.withdrawals.as_deref())
            .map_err(|error| BlockExecutionError::msg(error.to_string()))?;

        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let beneficiary = self.inner.evm.block().beneficiary();
        if self.block_hash.is_some() && block_number > 0 {
            let artifacts = decode_outbe_block_artifacts(self.block_extra_data.as_ref())
                .map_err(|error| BlockExecutionError::msg(error.to_string()))?;
            validate_compressed_entities_root_scheme(artifacts.compressed_entities_root)?;
        }
        // Initialise the begin-zone phase cursor for this block
        // BEFORE any pre-exec mutation that could affect routing. Block 1
        // (genesis bootstrap) skips Phase 1 and starts at CycleTick. Block
        // `n` with `n > GENESIS_BOOTSTRAP_BLOCK_NUMBER` enters Phase 1 with
        // a zero placeholder tx_hash. The Phase 1 preflight (Batch 3)
        // overwrites it once `verify_v2_proof` returns Ok and the system tx
        // is committed in pre-execution.
        self.system_tx_phase_cursor = crate::system_tx::SystemTxPhase::initial_for_block_with_ocomp(
            block_number,
            crate::system_tx::GENESIS_BOOTSTRAP_BLOCK_NUMBER,
            self.ocomp_lifecycle_active,
        );
        if block_number > 0 && beneficiary != outbe_primitives::addresses::REWARDS_ADDRESS {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!(
                        "non-genesis block beneficiary must be REWARDS_ADDRESS {}: got {}",
                        outbe_primitives::addresses::REWARDS_ADDRESS,
                        beneficiary
                    )
                    .into(),
                ),
            ));
        }
        if let Some(error) = &self.system_layout_error {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!("invalid system tx layout: {error}").into(),
                ),
            ));
        }

        // 1. Standard Ethereum pre-execution (blockhashes, beacon root, state clear flag).
        self.inner.apply_pre_execution_changes()?;

        // 2. Deploy 0xEF marker bytecode to all Outbe runtime addresses.
        //    Without bytecode these accounts are "empty" under EIP-161 and their
        //    storage is silently discarded during state root calculation.
        //    State::commit notifies reth's parallel state root task.
        self.preserve_runtime_markers()?;

        // 3. Open the block-scoped compressed-body overlay before any user or
        // system transaction can perform a body read or mutation. This also
        // applies to Reth's local pending-block construction. That path executes
        // txpool transactions against an isolated State. It therefore needs a
        // complete CE begin/end lifecycle, even though consensus-only Outbe
        // hooks remain disabled. The provisional tree batch is not published
        // without a final block hash.
        self.begin_compressed_entities(block_number)?;

        // Pending-block RPC has no proposer certificate or consensus system
        // transactions. Its isolated CE scope is active now, so user
        // transactions can be simulated faithfully. Skip only the
        // consensus-specific hooks below.
        if !self.execute_outbe_block_hooks {
            return Ok(());
        }

        // 4. Extract block context before taking a mutable DB borrow.
        let timestamp = self.inner.evm.block().timestamp().saturating_to::<u64>();
        let chain_id = self.inner.evm.chain_id();
        let block_artifacts = decode_outbe_block_artifacts(self.block_extra_data.as_ref())
            .map_err(|error| BlockExecutionError::msg(error.to_string()))?;
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());
        let allow_boundary_proposer = self.boundary_allows_proposer(&block_artifacts, proposer);
        if block_number > 0 {
            self.validate_proposer_identity(proposer, allow_boundary_proposer)?;
        }

        // Phase 1 `verify_v2_proof`
        // preflight. It runs AFTER marker preservation + pending-RPC short-
        // circuit + proposer identity validation. It runs BEFORE
        // `run_outbe_pre_execution_hooks` and BEFORE the main tx loop.
        // The verifier is a synchronous pure function with no state
        // mutation. On `Err`, the executor returns `BlockExecutionError`
        // without signalling any begin-zone state diff to Reth's state-
        // root background task. Block 0 / block 1 skip Phase 1.
        self.verify_phase1_in_preexec(block_number, &block_artifacts)?;

        // Late-finalize-credit BLS aggregates are FATAL-verified
        // here, on the same pre-exec path as Phase 1. This occurs before any
        // begin-zone state diff is signalled to Reth's state-root task. Proposer and
        // validator both verify. A bad aggregate, an out-of-window target, or a
        // missing committee snapshot aborts the block deterministically.
        self.verify_late_finalize_credits_in_preexec(block_number, &block_artifacts)?;

        // Phase 1 commit physical move. After
        // verify Ok, execute + commit the Phase 1 precompile so
        // `run_outbe_pre_execution_hooks` (Cycle / Rewards / Oracle) observe
        // post-Phase-1 accounting state. The proposer-supplied body[0] in
        // the main tx loop is validated against the cached witness hash and
        // skipped (validate-without-reexec). Receipt + state are already
        // in place from this call. Reth state-root ordering is preserved
        // because the preceding `verify_phase1_in_preexec` returned `Ok`.
        self.apply_phase1_commit_in_preexec(block_number, &block_artifacts)?;

        // 4. Fresh bootstrap validation data from consensus config.
        let genesis_validators = self
            .bridge
            .as_ref()
            .and_then(|b| b.peek_genesis_validators());

        // 5. Run Outbe block hooks. The provider flush commits through State,
        //    which notifies the parallel state root hook.
        let (_hook_changes, hook_events) = {
            let db = self.inner.evm.db_mut();
            let ctx = build_block_context(
                db,
                BlockContext {
                    block_number,
                    timestamp,
                    chain_id,
                    genesis_hash: self.genesis_hash,
                    proposer,
                    validators: Vec::new(),
                },
            )?;
            run_atomic_storage_hooks(db, ctx, |hook_ctx| -> outbe_primitives::error::Result<()> {
                if let Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) =
                    block_artifacts.consensus_header_artifact.as_ref()
                {
                    prepare_boundary_epoch_counters(
                        hook_ctx.storage.clone(),
                        boundary,
                        block_number,
                    )?;
                }
                let result = match self.runtime_body_readers.as_ref() {
                    Some(readers) => run_outbe_pre_execution_hooks_with_readers(
                        hook_ctx,
                        genesis_validators.as_ref(),
                        readers,
                        self.compressed_entities_scope.as_ref(),
                    ),
                    None => run_outbe_pre_execution_hooks(hook_ctx, genesis_validators.as_ref()),
                };
                if let (Some(readers), Err(error)) = (self.runtime_body_readers.as_ref(), &result) {
                    readers.report_precompile_error(error);
                }
                result
            })?
        };
        // Provider dropped here - mutable DB borrow released.

        // Log hook events via tracing for operator observability.
        // Whitelisted addresses are published through the mandatory HookEvents
        // system tx receipt. Non-whitelisted hook events stay tracing-only.
        for event in &hook_events {
            tracing::info!(
                target: "outbe::hooks",
                address = %event.address,
                topics = event.data.topics().len(),
                data_len = event.data.data.len(),
                "hook event emitted"
            );
        }

        let (whitelisted_hook_logs, _tracing_only_hook_logs) = partition_hook_events(&hook_events);
        self.whitelisted_hook_event_logs = whitelisted_hook_logs;

        // 6. Receipt-visible begin-zone system phases are real transactions in
        // the block body and execute in the normal tx loop before user txs.
        // Oracle slash-window work is part of that OracleSlashWindow system tx,
        // so there are no direct post-system storage hooks here.

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
                    BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                        format!("load precompile account {addr}: {e}").into(),
                    ))
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

    fn begin_compressed_entities(&mut self, block_number: u64) -> Result<(), BlockExecutionError> {
        let timestamp = self.inner.evm.block().timestamp().saturating_to::<u64>();
        let chain_id = self.inner.evm.chain_id();
        let proposer = self.inner.evm.block().beneficiary();
        let scope = self.compressed_entities_scope.clone();
        let (_changes, events) = {
            let db = self.inner.evm.db_mut();
            let ctx = build_block_context(
                db,
                BlockContext {
                    block_number,
                    timestamp,
                    chain_id,
                    genesis_hash: self.genesis_hash,
                    proposer,
                    validators: Vec::new(),
                },
            )?;
            run_atomic_storage_hooks(db, ctx, |hook_ctx| {
                let lifecycle = outbe_compressed_entities::CompressedEntitiesLifecycleContext::new(
                    hook_ctx.clone(),
                    scope.as_ref(),
                );
                <outbe_compressed_entities::CompressedEntitiesLifecycle as BlockLifecycle>::begin_block(
                    &lifecycle,
                )
            })?
        };
        if !events.is_empty() {
            return Err(BlockExecutionError::msg(
                "compressed-entity begin_block emitted an unexpected event",
            ));
        }

        self.compressed_entities_started = true;
        Ok(())
    }
}
