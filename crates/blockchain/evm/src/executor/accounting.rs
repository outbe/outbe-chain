use super::*;

/// Structural sanity checks for finalized-parent consensus metadata.
///
/// `metadata.ordered_committee` is the canonical historical committee for the
/// finalized-parent certificate, already verified by the consensus/application
/// layer. This validation enforces post-exec invariants that do not require
/// the live active set:
///
/// - signer bitmap length matches committee length
/// - committee has no duplicate addresses
/// - every committee member is a registered validator (not necessarily a
///   current consensus participant - historical EXITING/UNBONDING is fine)
/// - every `missed_proposer` is a member of `metadata.ordered_committee`
/// - bitmap entries are 0 or 1 only
pub(crate) fn validate_finalized_metadata(
    storage: StorageHandle,
    metadata: &CertifiedParentAccountingMetadata,
) -> outbe_primitives::error::Result<()> {
    if metadata.signer_bitmap.len() != metadata.ordered_committee.len() {
        return Err(PrecompileError::Fatal(
            "consensus metadata signer bitmap length mismatch".into(),
        ));
    }

    let committee_set: BTreeSet<Address> = metadata.ordered_committee.iter().copied().collect();
    if committee_set.len() != metadata.ordered_committee.len() {
        return Err(PrecompileError::Fatal(
            "consensus metadata committee contains duplicate addresses".into(),
        ));
    }

    let vs_check = outbe_validatorset::contract::ValidatorSet::new(storage);
    for addr in &metadata.ordered_committee {
        if !vs_check.is_validator(*addr)? {
            return Err(PrecompileError::Fatal(format!(
                "consensus metadata committee member is not a registered validator: {addr}"
            )));
        }
    }

    for missed in &metadata.missed_proposers {
        if !committee_set.contains(&missed.validator) {
            return Err(PrecompileError::Fatal(format!(
                "consensus metadata missed proposer is not in finalized committee: {} (view {})",
                missed.validator, missed.view,
            )));
        }
    }

    for entry in &metadata.signer_bitmap {
        if *entry > 1 {
            return Err(PrecompileError::Fatal(
                "consensus metadata signer bitmap contains non-binary entry".into(),
            ));
        }
    }

    Ok(())
}

/// parent-block execution artifact (`ExecutionSummaryArtifact`
/// from `header.extra_data`) paired with the parent block's timestamp.
/// Returned by [`AccountedParentArtifactProvider`] and consumed by the
/// Phase 1 `CertifiedParentAccounting` precompile via
/// `PreloadedSystemTxContext.finalized_summary`. Renamed from
/// `FinalizedExecutionSummary` because under V2 the parent need not be
/// finalized - it only needs to be the certified-parent of the block
/// being executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountedParentArtifact {
    pub summary: ExecutionSummaryArtifact,
    pub timestamp: u64,
    /// State root committed by the same exact parent header.
    ///
    /// Legacy/test bridge entries may omit it. An OCOMP finality transition
    /// requires this value and fails closed when it is unavailable.
    pub state_root: Option<B256>,
}

/// exact-hash-first lookup of an accounted-parent's
/// [`ExecutionSummaryArtifact`].
///
/// Replaces `FinalizedExecutionSummaryProvider`. The return type is
/// [`AccountedParentArtifact`] (artifact + parent header timestamp) because
/// `outbe_rewards::on_finalized_metadata` consumes the parent timestamp
/// downstream and the timestamp is available from the same
/// `sealed_header_by_hash` lookup at zero extra cost.
///
/// Required lookup priority (impls must follow):
/// 1. Exact cache lookup keyed by `(block_number, block_hash)`.
/// 2. `HeaderProvider::sealed_header_by_hash(block_hash)` with
///    `header.number == block_number` asserted before decoding
///    `OutbeBlockArtifacts.execution_summary`.
/// 3. Canonical-by-number fallback is allowed ONLY after
///    `sealed_header(block_number).hash() == block_hash` (explicit
///    double-check).
/// 4. On `(block_number, block_hash)` mismatch the impl MUST return
///    `Ok(None)` or `Err(...)`, never the canonical-at-number artifact
///    silently.
pub trait AccountedParentArtifactProvider: Send + Sync {
    fn execution_summary_by_hash(
        &self,
        block_number: u64,
        block_hash: B256,
    ) -> Result<Option<AccountedParentArtifact>, reth_evm::execute::ProviderError>;
}

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Spec: Into<revm::primitives::hardfork::SpecId>,
    E::Error: std::fmt::Display,
{
    /// resolve the accounted-parent artifact for the given Phase 1
    /// metadata.
    ///
    /// Resolution order:
    /// 1. Provider-backed exact-hash lookup via [`AccountedParentArtifactProvider::execution_summary_by_hash`].
    ///    This covers the validator path (sealed block in MDBX) and the
    ///    proposer path when the bridge cache or tree-state is populated.
    /// 2. Payload-builder-supplied [`AccountedParentArtifact`] hint.
    ///    Accepted only when the metadata's
    ///    `(finalized_block_number, finalized_block_hash)` matches
    ///    `(block_number - 1, self.parent_hash)` - i.e., the hint must be for
    ///    the actual parent of the block being executed. The proposer payload
    ///    builder decodes this from `parent_header.extra_data` at build time,
    ///    so the hint inherits the integrity of the parent block hash chain.
    ///
    /// Returns an error only on real provider I/O failure. `HeaderNotFound`
    /// is a visibility miss (e.g. the FCU-Valid -> MDBX-commit race), so the
    /// executor treats it like `Ok(None)` and lets the checked
    /// `parent_artifact_hint` fallback engage. A provider miss with no usable
    /// hint is fatal - the executor never silently accepts a
    /// canonical-by-number artifact.
    pub(in crate::executor) fn accounted_parent_artifact_for_metadata(
        &self,
        metadata: &CertifiedParentAccountingMetadata,
    ) -> Result<AccountedParentArtifact, BlockExecutionError> {
        if let Some(provider) = self.accounted_parent_artifact_provider.as_ref() {
            match provider.execution_summary_by_hash(
                metadata.finalized_block_number,
                metadata.finalized_block_hash,
            ) {
                Ok(Some(resolved)) => return Ok(resolved),
                Ok(None) | Err(reth_evm::execute::ProviderError::HeaderNotFound(_)) => {}
                Err(error) => {
                    return Err(BlockExecutionError::Internal(
                        InternalBlockExecutionError::Other(
                            format!("read accounted-parent artifact: {error}").into(),
                        ),
                    ));
                }
            }
        }

        // accept the payload-builder-supplied hint only when it matches
        // this block's actual parent. The metadata's parent
        // `(finalized_block_number, finalized_block_hash)` must equal
        // `(block_number - 1, self.parent_hash)`; any other value is a stale
        // or competing-branch artifact and must be rejected.
        if let Some(hint) = self.parent_artifact_hint.as_ref() {
            let block_number = self.inner.evm.block().number().saturating_to::<u64>();
            let parent_block_number = block_number.saturating_sub(1);
            if metadata.finalized_block_hash == self.parent_hash
                && metadata.finalized_block_number == parent_block_number
            {
                return Ok(*hint);
            }
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!(
                        "parent_artifact_hint mismatch: metadata=({}, {}), actual parent=({parent_block_number}, {})",
                        metadata.finalized_block_number, metadata.finalized_block_hash, self.parent_hash,
                    )
                    .into(),
                ),
            ));
        }

        Err(BlockExecutionError::Internal(
            InternalBlockExecutionError::Other(
                format!(
                    "missing execution summary artifact for accounted-parent block {} ({})",
                    metadata.finalized_block_number, metadata.finalized_block_hash
                )
                .into(),
            ),
        ))
    }

    /// V2 Phase 1 preflight.
    ///
    /// For block `n >= 2` (greenfield, where `GENESIS_BOOTSTRAP_BLOCK_NUMBER`
    /// equals `1`) this verifies the `CertifiedParentAccounting` metadata
    /// via `outbe_consensus::proof::verify_v2_proof` BEFORE any begin-zone
    /// state mutation is committed. The verifier is a synchronous pure
    /// function; on `Err` the executor returns `BlockExecutionError` with
    /// no state changes (no soft receipt because Phase 1 failures are
    /// fatal).
    ///
    /// Block `0` and block `1` (genesis bootstrap) skip Phase 1 entirely
    /// and return `Ok(())` without reading any storage.
    ///
    /// safety contract: the preflight runs in `apply_pre_execution_changes`
    /// AFTER marker preservation plus pending-RPC short-circuit AND BEFORE
    /// `run_outbe_pre_execution_hooks` plus the main tx loop. Marker
    /// preservation commit is the only state-root signal that precedes
    /// Phase 1 verify. The lifecycle hook commits and the Phase 1
    /// commit itself (still in the main tx loop pending 's
    /// gating consumer) only happen after a successful verify.
    pub(in crate::executor) fn verify_phase1_in_preexec(
        &mut self,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        use outbe_consensus::proof::verify_v2_proof;
        use outbe_validatorset::state::{committee_snapshot_key, read_committee_snapshot};

        if block_number <= crate::system_tx::GENESIS_BOOTSTRAP_BLOCK_NUMBER {
            return Ok(());
        }
        #[cfg(test)]
        if PHASE1_VERIFY_DISABLED.with(|cell| cell.get()) {
            // Test-only opt-out: legacy unit tests that exercise pre-exec
            // without seeding a committee snapshot. Production paths never
            // disable verification.
            return Ok(());
        }

        // Reuse the existing builder to produce the canonical Phase 1 input
        // for this block (validator-mode: proposer-supplied; proposer-mode:
        // derived from `parent_consensus_metadata`). The metadata struct
        // carries the V2 wire fields the verifier needs.
        let system_txs = self.begin_block_system_tx_inputs(block_number, block_artifacts)?;
        let Some((kind, input, _summary)) = system_txs.into_iter().next() else {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!(
                        "missing Phase 1 system tx for block {block_number} in pre-exec verifier"
                    )
                    .into(),
                ),
            ));
        };
        if !matches!(kind, SystemTxKind::CertifiedParentAccounting) {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!(
                        "Phase 1 pre-exec verifier expected CertifiedParentAccounting, got {kind:?}"
                    )
                    .into(),
                ),
            ));
        }
        let SystemTxInputV2::CertifiedParentAccounting { metadata } = &input else {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    "Phase 1 pre-exec verifier expected CertifiedParentAccounting input".into(),
                ),
            ));
        };

        // Resolve the active committee snapshot for the parent's epoch via
        // 's `CommitteeSnapshotStore`. The `(epoch, committee_set_hash)`
        // pair from the metadata yields the canonical storage key.
        let snapshot_key =
            committee_snapshot_key(metadata.finalized_epoch, metadata.committee_set_hash);
        let timestamp = self.inner.evm.block().timestamp().saturating_to::<u64>();
        let chain_id = self.inner.evm.chain_id();
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());
        let parent_hash = self.parent_hash;
        let cert_bytes = metadata.proof.clone();
        let metadata_for_verify = metadata.clone();

        let snapshot = {
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
            read_committee_snapshot(storage, snapshot_key).map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!(
                        "Phase 1 pre-exec: read committee snapshot for epoch={} key={}: {error}",
                        metadata_for_verify.finalized_epoch, snapshot_key
                    )
                    .into(),
                ))
            })?
        };
        let Some(snapshot) = snapshot else {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!(
                        "Phase 1 pre-exec: missing committee snapshot for epoch={} key={}",
                        metadata_for_verify.finalized_epoch, snapshot_key
                    )
                    .into(),
                ),
            ));
        };

        let verified = verify_v2_proof(
            &metadata_for_verify,
            &snapshot,
            cert_bytes.as_ref(),
            parent_hash,
        )
        .map_err(|error| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!(
                    "Phase 1 pre-exec: verify_v2_proof rejected metadata for block {block_number}: {error}"
                )
                .into(),
            ))
        })?;

        // cache the canonical VRF proof hash so
        // `apply_phase1_commit_in_preexec` (and the main-loop body[0]
        // path) can populate the V3 Rewards fingerprint without
        // re-decoding the certificate.
        self.verified_phase1_vrf_proof_hash = Some(verified.vrf_proof_hash);

        Ok(())
    }

    /// Phase 1 commit move: physically execute the Phase 1
    /// system tx and commit its state diff BEFORE
    /// `run_outbe_pre_execution_hooks` runs. Hooks then observe
    /// post-Phase-1 accounting state (consumer Cycle Phase 2
    /// gating on `AccountingProgressStore`).
    ///
    /// The commit is performed via `inner.commit_transaction`, which is the
    /// same code path the main tx loop uses for system txs - it pushes the
    /// Phase 1 receipt at `receipts[0]`, commits state via `db.commit`,
    /// signals Reth's parallel state-root task via `State::commit`,
    /// and updates the executor's gas accumulators. State-root ordering is
    /// preserved because `verify_phase1_in_preexec` ran (and accepted) the
    /// proof before this method is called.
    ///
    /// The proposer-supplied body[0] arrives later in the main tx loop. The
    /// `execute_transaction_with_commit_condition` intercept (cursor
    /// variant `Phase1Preexecuted` with non-zero `tx_hash`) validates the
    /// body[0] tx matches the cached `signature_hash` and returns `Ok(None)`
    /// without re-executing or re-committing - receipt and state are
    /// already in place from this pre-exec call.
    ///
    /// Skip conditions:
    /// - Block 0 / block 1 (genesis bootstrap): no Phase 1.
    /// - Test-only opt-out via `with_phase1_verify_disabled` (legacy unit
    ///   tests that exercise pre-exec without seeding a snapshot).
    pub(in crate::executor) fn apply_phase1_commit_in_preexec(
        &mut self,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        if block_number <= crate::system_tx::GENESIS_BOOTSTRAP_BLOCK_NUMBER {
            return Ok(());
        }
        #[cfg(test)]
        if PHASE1_VERIFY_DISABLED.with(|cell| cell.get()) {
            return Ok(());
        }

        // Resolve canonical Phase 1 input + finalized summary for this block.
        let system_txs = self.begin_block_system_tx_inputs(block_number, block_artifacts)?;
        let Some((kind, input, finalized_summary)) = system_txs.into_iter().next() else {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!(
                        "Phase 1 commit pre-exec: missing Phase 1 system tx for block {block_number}"
                    )
                    .into(),
                ),
            ));
        };
        if !matches!(kind, SystemTxKind::CertifiedParentAccounting) {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!(
                        "Phase 1 commit pre-exec: expected CertifiedParentAccounting, got {kind:?}"
                    )
                    .into(),
                ),
            ));
        }
        let calldata = input.encode().map_err(|error| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!("Phase 1 commit pre-exec: encode SystemTxInputV2: {error}").into(),
            ))
        })?;

        // Resolve proposer first - `begin_zone_proposer` is `Option`-aware
        // and may consult `expected_begin_system_txs` or the configured EVM
        // signer; the prebuilt validation below pins
        // `prebuilt.signer()` against this address.
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());

        // Build the canonical signed Phase 1 tx (witness for body[0]
        // validation). Priority:
        // 1. prebuilt witness handed in by the payload builder
        //      (proposer mode). Cached in `OutbeBlockExecutionCtx` BEFORE
        //      `apply_pre_execution_changes`. Validated: calldata bytes,
        //      signer matches resolved proposer.
        //   2. Validator-mode body[0] arriving through
        //      `expected_begin_system_txs.first()` from the sealed block.
        //   3. Legacy proposer fallback that re-signs the artifact through
        //      `evm_signer`. Determinism preserved because the signer is
        //      RFC 6979 (see `crates/blockchain/evm/src/signer.rs`).
        let chain_id = self.inner.evm.chain_id();
        let (cached_tx_hash, signed_gas_limit) = if let Some(prebuilt) = &self.prebuilt_phase1_tx {
            let tx_hash = validate_phase1_witness_against(
                prebuilt.tx(),
                calldata.as_ref(),
                proposer,
                chain_id,
                block_number,
            )
            .map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!("Phase 1 commit pre-exec: invalid prebuilt witness: {error}").into(),
                ))
            })?;
            (tx_hash, prebuilt.tx().gas_limit())
        } else if let Some(expected) = self.expected_begin_system_txs.first() {
            let tx_hash = validate_phase1_witness_against(
                expected.tx(),
                calldata.as_ref(),
                proposer,
                chain_id,
                block_number,
            )
            .map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!("Phase 1 commit pre-exec: invalid body[0] witness: {error}").into(),
                ))
            })?;
            (tx_hash, expected.tx().gas_limit())
        } else if let Some(signer) = &self.evm_signer {
            let unsigned = build_unsigned_system_tx(
                SystemTxKind::CertifiedParentAccounting,
                0,
                block_number,
                chain_id,
                calldata.clone(),
            )
            .map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!("Phase 1 commit pre-exec: build unsigned witness: {error}").into(),
                ))
            })?;
            let signed = signer.sign_unsigned(unsigned).map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!("Phase 1 commit pre-exec: sign witness: {error}").into(),
                ))
            })?;
            let signed_gas_limit = signed.gas_limit();
            let tx_hash = validate_phase1_witness_against(
                &signed,
                calldata.as_ref(),
                proposer,
                chain_id,
                block_number,
            )
            .map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!("Phase 1 commit pre-exec: invalid signed witness: {error}").into(),
                ))
            })?;
            (tx_hash, signed_gas_limit)
        } else {
            // No witness source. Skip the commit move; the legacy main-loop
            // path will run Phase 1 like before. The commit move only binds when a
            // witness source is available.
            return Ok(());
        };
        let phase_context = PreloadedSystemTxContext {
            proposer,
            finalized_summary,
            allow_boundary_proposer: self.boundary_allows_proposer(block_artifacts, proposer),
            // feed the verified parent certificate's VRF
            // proof hash into the precompile so the V3 Rewards
            // fingerprint can bind it. `B256::ZERO` only when the
            // preflight was skipped (genesis bootstrap), in which case
            // the Phase 1 precompile path itself is also skipped.
            canonical_vrf_proof_hash: self.verified_phase1_vrf_proof_hash.unwrap_or(B256::ZERO),
        };

        // Execute Phase 1 precompile. Only explicit CE charges inside this
        // system-call boundary are added to the public envelope gas.
        let gas_window = self
            .compressed_entities_scope
            .begin_explicit_gas_window(0)
            .map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!("Phase 1 commit pre-exec: open CE gas window: {error}").into(),
                ))
            })?;
        let transact_outcome = with_preloaded_system_tx_context(phase_context, || {
            self.inner.evm.transact_system_call(
                outbe_primitives::addresses::SYSTEM_ADDRESS,
                outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                calldata,
            )
        });
        let result = match transact_outcome {
            Ok(result) => result,
            Err(error) => {
                let reason =
                    format!("Phase 1 commit pre-exec: transact_system_call failed: {error}");
                tracing::error!(target: "outbe::executor", %reason);
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(reason.into()),
                ));
            }
        };
        let compressed_entities_gas = gas_window.gas_used().map_err(|error| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!("Phase 1 commit pre-exec: read CE gas window: {error}").into(),
            ))
        })?;
        drop(gas_window);
        if !result.result.is_success() {
            // Phase 1 (CertifiedParentAccounting) is consensus-critical
            // (`SystemTxKind::revert_fails_block()` is true for it), so a revert here
            // is a hard block failure, not a soft-receipt skip - its finalized-parent
            // accounting is one-shot and never retried. The revert is deterministic in
            // committed chain state, so every validator rejects the same block.
            let reason = format!(
                "critical system tx CertifiedParentAccounting did not succeed (revert/halt) in \
                 Phase 1 pre-exec commit: {:?}",
                result.result
            );
            tracing::error!(target: "outbe::executor", %reason, "critical begin-zone phase did not succeed; failing block");
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(reason.into()),
            ));
        }
        // Commit state + push receipt[0] + signal state-root task via the
        // standard EthBlockExecutor machinery. This holds because
        // `verify_phase1_in_preexec` returned `Ok` before this call.
        let output = EthTxResult {
            result,
            blob_gas_used: 0,
            tx_type: alloy_consensus::TxType::Legacy,
        };
        self.commit_system_transaction(
            output,
            signed_gas_limit,
            compressed_entities_gas,
            signed_gas_limit,
        )?;

        // Update the cursor with the cached witness hash. The
        // `execute_transaction_with_commit_condition` intercept reads
        // `Phase1Preexecuted.tx_hash` to detect the proposer-supplied body[0]
        // arrival and validate-without-reexec.
        self.system_tx_phase_cursor = crate::system_tx::SystemTxPhase::Phase1Preexecuted {
            body_index: 0,
            tx_hash: cached_tx_hash,
            receipt_index: 0,
        };
        Ok(())
    }
}
