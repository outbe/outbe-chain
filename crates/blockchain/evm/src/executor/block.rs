use super::*;

/// Outbe block executor.
///
/// Wraps the standard [`EthBlockExecutor`] and routes Outbe system transactions
/// through the same ordered transaction/receipt path as user transactions.
/// `apply_pre_execution_changes()` only performs pre-block setup; begin-zone
/// phases execute when their reserved-address body transaction reaches the loop.
pub struct OutbeBlockExecutor<'a, Evm> {
    /// Inner Ethereum execution strategy.
    pub inner: EthBlockExecutor<'a, Evm, &'a Arc<ChainSpec<OutbeHeader>>, &'a RethReceiptBuilder>,
    /// Immutable chain identity sourced from the executor's canonical ChainSpec.
    pub(super) genesis_hash: B256,
    /// Optional bridge to the consensus layer for finalization data.
    pub bridge: Option<ConsensusExecutionBridge>,
    /// Header-carried consensus artifact bytes (`extra_data`) used by begin-zone phases.
    pub(super) block_extra_data: Bytes,
    /// Canonical final header `extra_data` bytes. On the verifier path this is
    /// initialized from the sealed block header; on the proposer path the block
    /// builder overwrites it after injecting the execution summary and timestamp
    /// millis but before `finish()`.
    pub(super) final_extra_data: Bytes,
    /// Historical header artifact reader used for finalized-block settlement.
    pub(super) accounted_parent_artifact_provider: Option<Arc<dyn AccountedParentArtifactProvider>>,
    /// Whether this executor is validating an already-built block and must
    /// compare the header-carried execution summary to local execution output.
    pub(super) validate_execution_summary: bool,
    /// Hash of the block being validated, when execution is for an existing block.
    pub(super) block_hash: Option<B256>,
    /// State root committed by the block being validated. It is cached with the
    /// execution summary so the immediate child can bind OCOMP finality even
    /// during the Reth in-memory-tree/provider visibility window.
    block_state_root: Option<B256>,
    /// Hash of this block's parent header.
    pub(super) parent_hash: B256,
    /// Priority/coinbase fees collected by user transactions in this block.
    current_block_validator_fees: U256,
    /// Internal gas consumed by begin-zone system transactions under the
    /// Outbe-only 100M execution lane. The Ethereum-visible block counters use
    /// each system tx envelope's visible intrinsic gas instead.
    pub(super) system_tx_execution_gas: u64,
    /// Validator-mode signer used by proposer path to sign system-tx artifacts.
    pub(super) evm_signer: Option<SharedOutbeEvmSigner>,
    pub(super) expected_begin_system_txs: Vec<Recovered<TransactionSigned>>,
    pub(super) expected_end_system_txs: Vec<Recovered<TransactionSigned>>,
    pub(super) ocomp_lifecycle_active: bool,
    pub(super) ocomp_terminal_request_consumed: bool,
    /// Standard Ethereum post-execution output captured before CE seal and
    /// OSR2. Active OCOMP blocks must not call the inner executor's `finish`
    /// afterward because that would create semantic writes after OSR2.
    ethereum_post_execution_requests: Option<Requests>,
    pub(super) system_layout_error: Option<String>,
    pub(super) parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
    pub(super) proposer_evm_address: Option<Address>,
    pub(super) execute_outbe_block_hooks: bool,
    /// cursor that drives begin-zone phase routing inside
    /// `execute_transaction_with_commit_condition` instead of
    /// `self.inner.receipts.len()`. Set to the per-block initial value when
    /// the executor enters `apply_pre_execution_changes` and advanced once
    /// per consumed begin-zone system tx.
    pub(super) system_tx_phase_cursor: crate::system_tx::SystemTxPhase,
    /// proposer-side prebuilt Phase 1 body[0] tx. Set by the payload
    /// builder before `apply_pre_execution_changes`; consumed inside
    /// `apply_phase1_commit_in_preexec` as the canonical witness whose
    /// `signature_hash` is cached in the phase cursor. `None` on the validator
    /// path (witness comes from `expected_begin_system_txs.first()`) and for
    /// `block_number <= GENESIS_BOOTSTRAP_BLOCK_NUMBER`.
    pub(super) prebuilt_phase1_tx: Option<Recovered<TransactionSigned>>,
    /// optional accounted-parent artifact hint supplied by the
    /// payload builder. Consumed by
    /// [`Self::accounted_parent_artifact_for_metadata`] when the
    /// [`AccountedParentArtifactProvider`] returns `None`. Accepted only if
    /// the metadata's `(finalized_block_number, finalized_block_hash)`
    /// matches `(self.parent_block_number(), self.parent_hash)`.
    pub(super) parent_artifact_hint: Option<AccountedParentArtifact>,
    /// canonical VRF proof hash captured by
    /// `verify_phase1_in_preexec` from the verified parent certificate
    /// (`outbe_consensus::proof::VerifiedProof::vrf_proof_hash`).
    /// Consumed by `apply_phase1_commit_in_preexec` and the main-tx-loop
    /// Phase 1 path to populate `PreloadedSystemTxContext.canonical_vrf_proof_hash`,
    /// which the V3 Rewards fingerprint binds. `None` until the preflight
    /// has run; remains `None` for skip paths (block 0 / 1, test opt-out).
    pub(super) verified_phase1_vrf_proof_hash: Option<B256>,
    /// Proposer-only one-time Phase 3b `TeeBootstrap` payload. When `Some` on the
    /// proposer path, `begin_block_system_tx_inputs` injects the bootstrap system
    /// tx after `BoundaryOutcome` - identically to `build_begin_system_txs` so the
    /// body the proposer signs and the inputs the executor expects match. `None`
    /// on the validator path (the body carries it via `expected_begin_system_txs`)
    /// and until the tribute-DKG bootstrap producer supplies a payload.
    pub(super) pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
    /// Whitelisted pre-exec hook logs published through the mandatory
    /// `HookEvents` system tx receipt at the end of the begin zone.
    whitelisted_hook_event_logs: Vec<Log>,
    /// Number of zero-fee soft-failure receipts emitted in THIS
    /// block. Bounds block-stuffing by zero-cost 21k soft-failures (see
    /// [`Self::record_zero_fee_soft_failure`]). The executor is constructed
    /// fresh per block, so this resets per block; it is identical on the
    /// proposer (build) and validator (re-execution) paths.
    pub(super) zero_fee_soft_failures: u32,
    /// Least-authority off-chain readers used by lifecycle body reads.
    runtime_body_readers: Option<RuntimeBodyReaders>,
    execution_read_budget_guard: Option<ExecutionReadBudgetGuard>,
    /// One lifecycle capability shared with every precompile in this EVM.
    pub(super) compressed_entities_scope: Arc<ExecutionScope>,
    pub(super) compressed_entities_started: bool,
    pub(super) compressed_entities_seal_output: Option<outbe_compressed_entities::SealOutput>,
    pub(super) compressed_tree_service:
        Option<Arc<outbe_compressed_entities::CompressedTreeService>>,
}

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        inner: EthBlockExecutor<'a, Evm, &'a Arc<ChainSpec<OutbeHeader>>, &'a RethReceiptBuilder>,
        bridge: Option<ConsensusExecutionBridge>,
        block_extra_data: Bytes,
        accounted_parent_artifact_provider: Option<Arc<dyn AccountedParentArtifactProvider>>,
        validate_execution_summary: bool,
        block_hash: Option<B256>,
        parent_hash: B256,
        evm_signer: Option<SharedOutbeEvmSigner>,
        expected_begin_system_txs: Vec<Recovered<TransactionSigned>>,
        expected_end_system_txs: Vec<Recovered<TransactionSigned>>,
        system_layout_error: Option<String>,
        parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
        proposer_evm_address: Option<Address>,
        execute_outbe_block_hooks: bool,
        prebuilt_phase1_tx: Option<Recovered<TransactionSigned>>,
        parent_artifact_hint: Option<AccountedParentArtifact>,
    ) -> Self {
        let genesis_hash = inner.spec.genesis_hash();
        Self {
            inner,
            genesis_hash,
            bridge,
            final_extra_data: block_extra_data.clone(),
            block_extra_data,
            accounted_parent_artifact_provider,
            validate_execution_summary,
            block_hash,
            block_state_root: None,
            parent_hash,
            current_block_validator_fees: U256::ZERO,
            system_tx_execution_gas: 0,
            evm_signer,
            expected_begin_system_txs,
            expected_end_system_txs,
            ocomp_lifecycle_active: false,
            ocomp_terminal_request_consumed: false,
            ethereum_post_execution_requests: None,
            system_layout_error,
            parent_consensus_metadata,
            proposer_evm_address,
            execute_outbe_block_hooks,
            // placeholder; the real initial value is computed in
            // `apply_pre_execution_changes` once `block_number` is known and
            // the Phase 1 preflight has (or has not) been performed.
            system_tx_phase_cursor: crate::system_tx::SystemTxPhase::UserTxs,
            prebuilt_phase1_tx,
            parent_artifact_hint,
            // populated by `verify_phase1_in_preexec` on real
            // verify; remains `None` for skip paths.
            verified_phase1_vrf_proof_hash: None,
            // proposer-only; set via `with_pending_tee_bootstrap` from the
            // execution ctx. `None` keeps the begin-zone unchanged.
            pending_tee_bootstrap: None,
            whitelisted_hook_event_logs: Vec::new(),
            zero_fee_soft_failures: 0,
            runtime_body_readers: None,
            execution_read_budget_guard: None,
            compressed_entities_scope: Arc::new(ExecutionScope::new()),
            compressed_entities_started: false,
            compressed_entities_seal_output: None,
            compressed_tree_service: None,
        }
    }

    pub(crate) fn with_compressed_entities_scope(mut self, scope: Arc<ExecutionScope>) -> Self {
        self.compressed_entities_scope = scope;
        self
    }

    pub(crate) fn with_block_state_root(mut self, state_root: Option<B256>) -> Self {
        self.block_state_root = state_root;
        self
    }

    pub(crate) fn with_ocomp_lifecycle_active(mut self, active: bool) -> Self {
        self.ocomp_lifecycle_active = active;
        self
    }

    pub(crate) fn with_compressed_tree_service(
        mut self,
        service: Option<Arc<outbe_compressed_entities::CompressedTreeService>>,
    ) -> Self {
        self.compressed_tree_service = service;
        self
    }

    pub(crate) fn with_runtime_body_readers(
        mut self,
        readers: Option<RuntimeBodyReaders>,
        budget: Option<outbe_primitives::projection::ExecutionReadBudget>,
    ) -> Self {
        self.execution_read_budget_guard = readers
            .as_ref()
            .zip(budget)
            .map(|(readers, budget)| readers.enter_execution_budget(budget));
        self.runtime_body_readers = readers;
        self
    }

    /// Proposer-path builder: attach the one-time `TeeBootstrap` payload the
    /// executor injects after `BoundaryOutcome`. No-op (stays `None`) on the
    /// validator path. Mirrors `OutbeEvmConfig::build_begin_system_txs`.
    pub(crate) fn with_pending_tee_bootstrap(
        mut self,
        pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
    ) -> Self {
        self.pending_tee_bootstrap = pending_tee_bootstrap;
        self
    }
}

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
    pub(crate) fn current_execution_summary(&self) -> ExecutionSummaryArtifact
    where
        Evm: reth_ethereum::evm::primitives::Evm,
    {
        // ExecutionSummaryArtifact wire format v0x04 carries
        // only `validator_fee_sum`; the per-block emission field has
        // been removed because daily emission is computed by the Cycle
        // handler from the closed-form formula and does not need to
        // travel in `extra_data`.
        ExecutionSummaryArtifact {
            validator_fee_sum: self.current_block_validator_fees,
        }
    }

    /// Canonical final header `extra_data` bytes used by `finish()` for
    /// execution-summary validation and bridge recording.
    pub(crate) fn final_extra_data(&self) -> &Bytes {
        &self.final_extra_data
    }

    pub(crate) fn set_final_extra_data(&mut self, bytes: Bytes) {
        self.final_extra_data = bytes;
    }

    /// Pre-encodes the final execution-produced artifact fields after CE seal.
    /// The block-builder adapter owns the encoding; this executor entry point
    /// lets the opaque Reth builder path invoke it before parallel root freeze.
    pub fn prepare_final_header_artifacts(
        &mut self,
        timestamp_millis_part: u64,
    ) -> Result<(), BlockExecutionError>
    where
        Evm: reth_ethereum::evm::primitives::Evm,
    {
        let seal = self
            .compressed_entities_seal_output
            .as_ref()
            .ok_or_else(|| BlockExecutionError::msg("missing compressed-entities SealOutput"))?;
        self.final_extra_data = crate::builder::encode_final_header_artifacts(
            self.final_extra_data.as_ref(),
            self.current_execution_summary(),
            timestamp_millis_part,
            seal.new_root,
        )?;
        Ok(())
    }

    // Half C-parlia step 11: `set_pending_consensus_metadata` and
    // `ingest_consensus_metadata_tx` are deleted. Finalized-parent
    // metadata now lives in the begin-zone Phase 1 system transaction input;
    // the pre-exec dispatch arm at `execute_transaction_with_commit_condition`
    // no longer accepts consensus metadata transactions, and the proposer no
    // longer produces them.
}

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Error: std::fmt::Display,
{
    /// Runs the standard Ethereum post-execution phase before the OCOMP
    /// terminal boundary.
    ///
    /// This intentionally mirrors [`EthBlockExecutor::finish`]'s semantic
    /// writes. The active OCOMP lifecycle requires a stricter order than the
    /// upstream executor exposes:
    ///
    /// `Ethereum post-execution -> CE preview -> OSR2 -> final CE seal`.
    ///
    /// The resulting EIP-7685 requests are retained for [`BlockExecutor::finish`],
    /// which assembles the result without invoking the upstream phase again.
    pub(in crate::executor) fn apply_outbe_ethereum_post_execution(
        &mut self,
    ) -> Result<(), BlockExecutionError> {
        if self.ethereum_post_execution_requests.is_some() {
            return Err(BlockExecutionError::msg(
                "standard Ethereum post-execution changes already applied",
            ));
        }

        validate_outbe_withdrawals(self.inner.ctx.withdrawals.as_deref())
            .map_err(|error| BlockExecutionError::msg(error.to_string()))?;

        let requests = if self
            .inner
            .spec
            .is_prague_active_at_timestamp(self.inner.evm.block().timestamp().saturating_to())
        {
            let deposit_requests =
                eip6110::parse_deposits_from_receipts(self.inner.spec, &self.inner.receipts)?;
            let mut requests = Requests::default();
            if !deposit_requests.is_empty() {
                requests.push_request_with_type(eip6110::DEPOSIT_REQUEST_TYPE, deposit_requests);
            }
            self.inner
                .system_caller
                .append_post_execution_changes(&mut self.inner.evm, &mut requests)?;
            requests
        } else {
            Requests::default()
        };

        let mut balance_increments = post_block_balance_increments(
            self.inner.spec,
            self.inner.evm.block(),
            self.inner.ctx.ommers,
            None,
        );

        if self
            .inner
            .spec
            .ethereum_fork_activation(EthereumHardfork::Dao)
            .transitions_at_block(self.inner.evm.block().number().saturating_to())
        {
            let drained_balance: u128 = self
                .inner
                .evm
                .db_mut()
                .drain_balances(dao_fork::DAO_HARDFORK_ACCOUNTS)
                .map_err(|_| BlockValidationError::IncrementBalanceFailed)?
                .into_iter()
                .sum();
            *balance_increments
                .entry(dao_fork::DAO_HARDFORK_BENEFICIARY)
                .or_default() += drained_balance;
        }

        self.inner
            .evm
            .db_mut()
            .increment_balances(balance_increments.clone())
            .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;

        self.ethereum_post_execution_requests = Some(requests);
        Ok(())
    }
}

#[allow(private_bounds)]
impl<DB, E> BlockExecutor for OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    // outbe-evm is pinned to revm's standard `HaltReason`; this constraint
    // is what lets [`system_tx_failure_code_for_result`] pattern-match the
    // halt variants for soft-failure code assignment.
    E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason> + ZeroFeeCfgAccess,
    E::Error: std::fmt::Display,
{
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type Evm = E;
    type Result = EthTxResult<E::HaltReason, reth_ethereum::TxType>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        validate_outbe_withdrawals(self.inner.ctx.withdrawals.as_deref())
            .map_err(|error| BlockExecutionError::msg(error.to_string()))?;

        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let beneficiary = self.inner.evm.block().beneficiary();
        if self.block_hash.is_some() && block_number > 0 {
            let artifacts = decode_outbe_block_artifacts(self.block_extra_data.as_ref())
                .map_err(|error| BlockExecutionError::msg(error.to_string()))?;
            validate_compressed_entities_root_scheme(artifacts.compressed_entities_root)?;
        }
        // initialise the begin-zone phase cursor for this block
        // BEFORE any pre-exec mutation that could affect routing. Block 1
        // (genesis bootstrap) skips Phase 1 and starts at CycleTick; block
        // `n` with `n > GENESIS_BOOTSTRAP_BLOCK_NUMBER` enters Phase 1 with
        // a zero placeholder tx_hash that the Phase 1 preflight (Batch 3)
        // overwrites once `verify_v2_proof` returns Ok and the system tx
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
        {
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
        }

        // 3. Open the block-scoped compressed-body overlay before any user or
        // system transaction can perform a body read or mutation. This also
        // applies to Reth's local pending-block construction: it executes
        // txpool transactions against an isolated State and therefore needs a
        // complete CE begin/end lifecycle even though consensus-only Outbe
        // hooks remain disabled. The provisional tree batch is not published
        // without a final block hash.
        {
            let timestamp = self.inner.evm.block().timestamp().saturating_to::<u64>();
            let chain_id = self.inner.evm.chain_id();
            let proposer = self.inner.evm.block().beneficiary();
            let scope = self.compressed_entities_scope.clone();
            let (_changes, events) = {
                let db = self.inner.evm.db_mut();
                let ctx = build_block_context(
                    db,
                    block_number,
                    timestamp,
                    chain_id,
                    self.genesis_hash,
                    proposer,
                )?;
                run_atomic_storage_hooks(db, ctx, |hook_ctx| {
                    // Local readiness: reconstruct the committed parent before
                    // executing any receipt-producing transaction. This writes
                    // no chain state; missing/authentication-failed history is fatal.
                    outbe_tee::pledge_ledger::synchronize(&hook_ctx.storage)?;
                    let lifecycle =
                        outbe_compressed_entities::CompressedEntitiesLifecycleContext::new(
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
        }

        // Pending-block RPC has no proposer certificate or consensus system
        // transactions. Its isolated CE scope is active now, so user
        // transactions can be simulated faithfully; skip only the
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
        // preflight. Runs AFTER marker preservation + pending-RPC short-
        // circuit + proposer identity validation, BEFORE
        // `run_outbe_pre_execution_hooks` and BEFORE the main tx loop.
        // The verifier is a synchronous pure function with no state
        // mutation; on `Err` the executor returns `BlockExecutionError`
        // without signalling any begin-zone state diff to Reth's state-
        // root background task. Block 0 / block 1 skip Phase 1.
        self.verify_phase1_in_preexec(block_number, &block_artifacts)?;

        // late-finalize-credit BLS aggregates are FATAL-verified
        // here, on the same pre-exec path as Phase 1 and before any begin-zone
        // state diff is signalled to Reth's state-root task. Proposer and
        // validator both verify; a bad aggregate, an out-of-window target, or a
        // missing committee snapshot aborts the block deterministically.
        self.verify_late_finalize_credits_in_preexec(block_number, &block_artifacts)?;

        // Phase 1 commit physical move. After
        // verify Ok, execute + commit the Phase 1 precompile so
        // `run_outbe_pre_execution_hooks` (Cycle / Rewards / Oracle) observe
        // post-Phase-1 accounting state. The proposer-supplied body[0] in
        // the main tx loop is validated against the cached witness hash and
        // skipped (validate-without-reexec) - receipt + state are already
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
                block_number,
                timestamp,
                chain_id,
                self.genesis_hash,
                proposer,
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
        // system tx receipt; non-whitelisted hook events stay tracing-only.
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

    fn receipts(&self) -> &[Self::Receipt] {
        self.inner.receipts()
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::Result, BlockExecutionError> {
        let (tx_env, recovered) = tx.into_parts();
        if is_reserved_system_tx(recovered.tx()) {
            return Err(BlockExecutionError::msg(
                "reserved system transaction cannot execute without commit",
            ));
        }
        self.inner.execute_transaction_without_commit(WithTxEnv {
            tx: Arc::new(recovered),
            tx_env,
        })
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&Self::Result) -> CommitChanges,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let (mut tx_env, recovered) = tx.into_parts();
        if self.ocomp_terminal_request_consumed {
            return Err(BlockExecutionError::msg(
                "transaction follows the terminal OCOMP system transaction",
            ));
        }
        let is_ocomp_terminal_request = is_reserved_system_tx(recovered.tx())
            && matches!(
                SystemTxInputV2::decode(recovered.tx().input().as_ref()),
                Ok(SystemTxInputV2::OcompTerminalRequest)
            );
        if is_ocomp_terminal_request {
            return self.execute_ocomp_terminal_request(recovered, f);
        }
        let ce_scope = self.compressed_entities_scope.clone();
        let ce_checkpoint = ce_scope
            .ce_work_checkpoint()
            .map_err(BlockExecutionError::other)?;
        ce_scope
            .begin_ce_work_transaction()
            .map_err(BlockExecutionError::other)?;
        let outcome = (|| {
            let tx = recovered.tx();
            let signer = *recovered.signer();

            if is_reserved_system_tx(tx) {
                let block_number = self.inner.evm.block().number().saturating_to::<u64>();
                let block_artifacts = decode_outbe_block_artifacts(self.block_extra_data.as_ref())
                    .map_err(|error| BlockExecutionError::msg(error.to_string()))?;

                // Witness validate-without-reexec: if Phase 1
                // was already committed in `apply_pre_execution_changes::apply_phase1_commit_in_preexec`,
                // the cursor carries the cached witness `tx_hash`. Body[0] in the
                // main tx loop is the proposer-supplied Phase 1 tx - validate it
                // matches the cache (signature hash) and skip re-execution.
                // Receipt + state already exist from the pre-exec commit.
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
                        // Advance cursor past Phase 1; CycleTick body_index=1 next.
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
                        // Ok(None) signals "no further commit" - pre-exec already
                        // pushed receipt[0] and committed state. The block builder
                        // still keeps this validated witness in body[0].
                        return Ok(None);
                    }
                }

                // cursor-driven phase routing replaces the previous
                // `self.inner.receipts.len()` derivation. The cursor was
                // initialised in `apply_pre_execution_changes` and advances
                // exactly once per consumed begin-zone system tx (see the
                // `advance_after_commit` call below). This is the only
                // production reader of `self.system_tx_phase_cursor`.
                let (
                    body_index,
                    expected_phase,
                    expected_input,
                    finalized_summary,
                    visible_base_gas,
                    planned_gas_limit,
                ) = self.expected_system_tx_for_cursor(block_number, &block_artifacts)?;
                let actual_input =
                    SystemTxInputV2::decode(tx.input().as_ref()).map_err(|error| {
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
                if actual_input != expected_input {
                    return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!(
                            "system tx calldata mismatch at body_index={body_index} for {expected_phase:?}"
                        )
                        .into(),
                    ),
                ));
                }

                let ordinal = body_index.try_into().map_err(|_| {
                    BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                        format!("system tx body_index {body_index} exceeds u8 range").into(),
                    ))
                })?;
                let unsigned = build_unsigned_system_tx_with_gas_limit(
                    expected_phase,
                    ordinal,
                    block_number,
                    self.inner.evm.chain_id(),
                    tx.input().clone(),
                    planned_gas_limit,
                )
                .map_err(|error| {
                    BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                        format!("build expected system tx at body_index={body_index}: {error}")
                            .into(),
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

                if expected_phase == SystemTxKind::HookEvents {
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
                    return commit_outcome;
                }

                let phase_context = PreloadedSystemTxContext {
                    proposer,
                    finalized_summary,
                    allow_boundary_proposer: self
                        .boundary_allows_proposer(&block_artifacts, proposer),
                    // same VRF-proof-hash plumbing as the
                    // pre-exec commit path. Cached by the preflight; falls
                    // back to `B256::ZERO` only when the preflight was
                    // skipped (which never co-occurs with this main-loop
                    // path entering Phase 1 in production).
                    canonical_vrf_proof_hash: self
                        .verified_phase1_vrf_proof_hash
                        .unwrap_or(B256::ZERO),
                };
                // Phase 1-4 EVM result failures (`Revert` / `Halt`) are converted
                // into a `status=0` synthetic receipt with one `OutbeFailure(code, reason)`
                // log emitted from `OUTBE_SYSTEM_TX_ADDRESS`; revm did not commit the call so no
                // state change leaks. Raw `Err` from the system-call engine remains fatal because
                // upstream revm documents that the journal may be inconsistent on that path.
                // Body-parity validation above (decode / phase / calldata / signature / signer)
                // also remains fatal: those are validator-side checks that the proposer never
                // produces for itself.
                let ce_gas_limit =
                    visible_gas_limit
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
                let gas_window = self
                .compressed_entities_scope
                .begin_explicit_gas_window(ce_gas_limit)
                .map_err(|error| {
                    BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                        format!(
                            "open CE gas window for {expected_phase:?} at body_index={body_index}: {error}"
                        )
                        .into(),
                    ))
                })?;
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
                // under V2; other blocks depend on the header artifact).
                let has_boundary_outcome = matches!(
                    block_artifacts.consensus_header_artifact,
                    Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
                );
                let has_tee_bootstrap = self.block_has_tee_bootstrap();
                // Only EVM result failures use the soft-failure receipt path.
                // Raw engine/provider `Err` was handled above as fatal.
                let result = match transact_outcome {
                    Ok(value) => value,
                    Err(error) => {
                        let reason = format!(
                            "system tx {expected_phase:?} execution failed at body_index={body_index}: {error}"
                        );
                        tracing::error!(target: "outbe::executor", %reason);
                        return Err(BlockExecutionError::Internal(
                            InternalBlockExecutionError::Other(reason.into()),
                        ));
                    }
                };
                let compressed_entities_gas = gas_window.gas_used().map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!(
                        "read CE gas window for {expected_phase:?} at body_index={body_index}: {error}"
                    )
                    .into(),
                ))
            })?;
                drop(gas_window);
                if !result.result.is_success() {
                    tracing::error!(
                        target: "outbe::executor",
                        ?expected_phase,
                        body_index,
                        block_number,
                        gas_used = result.result.tx_gas_used(),
                        gas_limit = tx.gas_limit(),
                        result = ?result.result,
                        "system tx failed"
                    );
                    let code = system_tx_failure_code_for_result(&result.result);
                    // a revert/halt in a consensus- or economic-critical
                    // begin-zone phase is a hard block failure, not a soft-receipt
                    // skip. Their work is one-shot and never retried, so swallowing a
                    // revert permanently loses it (stranded fee escrow, dropped
                    // emission/reshare, unrecorded parent accounting). The revert is a
                    // deterministic function of committed chain state, so every
                    // validator rejects the same block identically - no state-root
                    // split. Non-critical phases (OracleSlashWindow, HookEvents)
                    // keep the soft-receipt skip for failures that fit within the
                    // aggregate internal-work budget. An OOG consumes the full
                    // system-call gas limit and therefore remains a hard aggregate
                    // budget failure once earlier mandatory phases have run.
                    if expected_phase.revert_fails_block() {
                        let reason = format!(
                            "critical system tx {expected_phase:?} did not succeed (revert/halt) at \
                         body_index={body_index}, block_number={block_number}, \
                         failure_code={code}: {:?}",
                            result.result
                        );
                        tracing::error!(target: "outbe::executor", %reason, "critical begin-zone phase did not succeed; failing block");
                        return Err(BlockExecutionError::Internal(
                            InternalBlockExecutionError::Other(reason.into()),
                        ));
                    }
                    let reason = format!(
                        "system tx {expected_phase:?} did not succeed at body_index={body_index}: {:?}",
                        result.result
                    );
                    let tx_type = tx.tx_type();
                    let receipt_ce_gas = if matches!(
                        result.result,
                        ExecutionResult::Halt {
                            reason: HaltReason::OutOfGas(_),
                            ..
                        }
                    ) {
                        ce_gas_limit
                    } else {
                        compressed_entities_gas
                    };
                    let gas_output =
                        self.push_system_failure_receipt(SystemFailureReceiptInput {
                            tx_type,
                            log_address: outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                            code,
                            reason,
                            visible_base_gas,
                            compressed_entities_gas: receipt_ce_gas,
                            signed_gas_limit: visible_gas_limit,
                            internal_gas_used: result.result.tx_gas_used(),
                        })?;
                    self.system_tx_phase_cursor =
                        self.system_tx_phase_cursor.advance_after_commit_with_ocomp(
                            has_boundary_outcome,
                            has_tee_bootstrap,
                            self.ocomp_lifecycle_active,
                        );
                    return Ok(Some(gas_output));
                }

                let output = EthTxResult {
                    result,
                    blob_gas_used: 0,
                    tx_type: tx.tx_type(),
                };
                if !f(&output).should_commit() {
                    // Cursor does not advance: caller has chosen not to commit,
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
                return commit_outcome;
            }

            let ocomp_system_carrier = classify_ocomp_system_carrier(
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
            })?;

            if let Some(candidate) = ocomp_system_carrier {
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
                return self
                    .commit_system_transaction(output, 0, 0, signed_gas_limit)
                    .map(Some);
            }

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
            // + disable_base_fee + disable_fee_charge` cfg snapshot is applied;
            // the counter increment is committed to the persistent state through
            // `DirectStorageProvider::flush` BEFORE the inner tx runs, so a
            // revert inside the tx does not un-burn the daily slot.
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
                // Stateful authorize + record_use under a single
                // `DirectStorageProvider` scope, then `flush()` so the counter
                // increment lands in `State<DB>` BEFORE the inner tx runs.
                // A REVERT inside the tx affects only its own journal frame
                // and cannot undo the flushed counter write.
                let (authorize_outcome, sponsorship_events, _sponsorship_changes) = {
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
                    let outcome = {
                        let storage = StorageHandle::new(&mut provider);
                        outbe_zerofee::authorize_sponsorship(storage.clone(), signer, timestamp)
                            .and_then(|auth| {
                                outbe_zerofee::record_sponsorship_use(
                                    storage,
                                    signer,
                                    auth.current_day,
                                )
                                .map(|_| auth)
                            })
                    };
                    let result = match outcome {
                        Ok(auth) => provider
                            .flush()
                            .map(|_| auth)
                            .map_err(outbe_zerofee::ZeroFeePolicyError::from),
                        Err(err) => Err(err),
                    };
                    // Drain the `SponsorshipAuthorized` logs that
                    // `record_sponsorship_use` pushed through the storage
                    // handle. They are kept aside even on Err so a future
                    // failure-path that emits diagnostic events still
                    // surfaces them; today the only writer pushes on
                    // success and is gated by `.and_then`.
                    let events = provider.take_events();
                    // State::commit already notified the parallel state root hook.
                    let changes = provider.take_committed_changes();
                    (result, events, changes)
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
                // Attach the `SponsorshipAuthorized` log(s) to the receipt
                // the inner tx just pushed. Without this the event the
                // module README and `record_sponsorship_use` doc promise
                // would never reach `eth_getLogs` filters. We only mutate
                // the receipt on a successful execute; on inner-tx
                // bail-out the inner builder did not push a receipt and
                // there is nothing to attach to (the counter was already
                // burned, which matches the anti-revert-drain contract).
                if result.is_ok() && !sponsorship_events.is_empty() {
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
        })();

        let ce_failure = ce_scope.take_ce_work_failure();
        ce_scope
            .end_ce_work_transaction()
            .map_err(BlockExecutionError::other)?;
        if !matches!(&outcome, Ok(Some(_))) {
            ce_scope
                .restore_ce_work_checkpoint(ce_checkpoint)
                .map_err(BlockExecutionError::other)?;
        }
        if let Some(error) = ce_failure {
            return Err(BlockExecutionError::other(error));
        }
        outcome
    }

    fn commit_transaction(&mut self, output: Self::Result) -> GasOutput {
        self.inner.commit_transaction(output)
    }

    fn execute_block(
        mut self,
        transactions: impl IntoIterator<Item = impl ExecutableTx<Self>>,
    ) -> Result<BlockExecutionResult<Self::Receipt>, BlockExecutionError>
    where
        Self: Sized,
    {
        self.apply_pre_execution_changes()?;

        for tx in transactions {
            self.execute_transaction_with_commit_condition(tx, |_| CommitChanges::Yes)?;
        }

        self.apply_post_execution_changes()
    }

    fn finish(mut self) -> Result<(Self::Evm, BlockExecutionResult<Receipt>), BlockExecutionError> {
        if self.ocomp_lifecycle_active {
            if !self.ocomp_terminal_request_consumed {
                return Err(BlockExecutionError::msg(
                    "active OCOMP block is missing its terminal system transaction",
                ));
            }
        } else {
            self.finalize_compressed_entities()?;
        }
        let current_summary = self.current_execution_summary();
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let block_timestamp = self.inner.evm.block().timestamp().saturating_to::<u64>();
        let block_artifacts = decode_outbe_block_artifacts(self.final_extra_data().as_ref())
            .map_err(|error| BlockExecutionError::msg(error.to_string()))?;
        if block_number > 0 {
            let seal_output = self
                .compressed_entities_seal_output
                .as_ref()
                .ok_or_else(|| {
                    BlockExecutionError::msg("missing compressed-entities SealOutput")
                })?;
            validate_compressed_entities_root_after_seal(
                block_artifacts.compressed_entities_root,
                seal_output.new_root,
            )?;
        }
        validate_execution_summary_artifact(
            self.validate_execution_summary,
            block_number,
            block_artifacts.execution_summary,
            current_summary,
        )?;

        // OCOMP applies this phase before its terminal request so that the CE
        // seal remains the final semantic writer. The normal path applies the
        // same Outbe-owned phase here. Neither path passes withdrawals to the
        // upstream Ethereum Gwei-to-wei conversion.
        if !self.ocomp_lifecycle_active {
            self.apply_outbe_ethereum_post_execution()?;
        }
        let requests = self
            .ethereum_post_execution_requests
            .take()
            .ok_or_else(|| BlockExecutionError::msg("missing Outbe post-execution output"))?;
        let gas_used = if self.inner.evm.cfg_env().enable_amsterdam_eip8037 {
            self.inner.max_block_gas_used()
        } else {
            self.inner.cumulative_tx_gas_used
        };
        let result = BlockExecutionResult {
            receipts: std::mem::take(&mut self.inner.receipts),
            requests,
            gas_used,
            blob_gas_used: self.inner.blob_gas_used,
        };
        let evm = self.inner.evm;
        // Validator/import execution ends before Reth validates receipt and
        // state roots, so it must not publish speculative CE state here. The
        // proposer publishes only after block assembly supplies the final hash;
        // a finalized validator block is reconstructed from durable canonical
        // receipts after the DB-only persistence barrier.
        if let (Some(bridge), Some(block_hash), Some(summary)) = (
            self.bridge.as_ref(),
            self.block_hash,
            block_artifacts.execution_summary,
        ) {
            if let Some(state_root) = self.block_state_root {
                bridge.record_execution_summary_with_state_root(
                    block_number,
                    block_hash,
                    summary,
                    block_timestamp,
                    state_root,
                );
            } else {
                bridge.record_execution_summary(block_number, block_hash, summary, block_timestamp);
            }
        }

        Ok((evm, result))
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        self.inner.evm_mut()
    }

    fn evm(&self) -> &Self::Evm {
        self.inner.evm()
    }
}
