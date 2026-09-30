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
    pub(super) current_block_validator_fees: U256,
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
    pub(super) ethereum_post_execution_requests: Option<Requests>,
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
    pub(super) whitelisted_hook_event_logs: Vec<Log>,
    /// Number of zero-fee soft-failure receipts emitted in THIS
    /// block. Bounds block-stuffing by zero-cost 21k soft-failures (see
    /// [`Self::record_zero_fee_soft_failure`]). The executor is constructed
    /// fresh per block, so this resets per block; it is identical on the
    /// proposer (build) and validator (re-execution) paths.
    pub(super) zero_fee_soft_failures: u32,
    /// Least-authority off-chain readers used by lifecycle body reads.
    pub(super) runtime_body_readers: Option<RuntimeBodyReaders>,
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
impl<DB, E> BlockExecutor for OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    // outbe-evm is pinned to revm's standard `HaltReason`; this constraint
    // is what lets [`system_tx_failure_code_for_result`] pattern-match the
    // halt variants for soft-failure code assignment.
    E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason> + ZeroFeeCfgAccess,
    E::Spec: Into<revm::primitives::hardfork::SpecId>,
    E::Error: std::fmt::Display,
{
    type Transaction = TransactionSigned;
    type Receipt = Receipt;
    type Evm = E;
    type Result = EthTxResult<E::HaltReason, reth_ethereum::TxType>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        self.apply_outbe_pre_execution()
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
        let (tx_env, recovered) = tx.into_parts();
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
            if is_reserved_system_tx(recovered.tx()) {
                return self.execute_reserved_system_tx(recovered, f);
            }
            match self.execute_ocomp_system_carrier(tx_env, recovered, f)? {
                system_execution::dispatch::OcompCarrierStep::Done(result) => return result,
                system_execution::dispatch::OcompCarrierStep::Continue {
                    tx_env,
                    recovered,
                    commit: f,
                } => self.route_user_transaction(tx_env, recovered, f),
            }
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
