use super::super::*;

type BeginSystemTransaction = (SystemTx, Option<AccountedParentArtifact>);

type ExpectedSystemTransaction = (usize, SystemTx, Option<AccountedParentArtifact>);

#[derive(Debug, Clone)]
pub(crate) struct ResolvedBeginSystemTx {
    pub input: SystemTx,
    pub finalized_summary: Option<AccountedParentArtifact>,
}

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
    /// read the current begin-zone system-tx phase cursor.
    /// Test-only introspection point. The production driver is internal.
    /// Consumer (cursor-driven routing) lands Batch 3.
    #[allow(dead_code)]
    pub(crate) fn system_tx_phase_cursor(&self) -> crate::system_tx::SystemTxPhase {
        self.system_tx_phase_cursor
    }

    pub(crate) fn is_preexecuted_phase1_witness(&self, tx: &TransactionSigned) -> bool {
        let crate::system_tx::SystemTxPhase::Phase1Preexecuted {
            tx_hash: cached_hash,
            ..
        } = self.system_tx_phase_cursor
        else {
            return false;
        };

        !cached_hash.is_zero() && is_reserved_system_tx(tx) && tx.signature_hash() == cached_hash
    }
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
    fn expected_begin_input(&self, ordinal: usize) -> Result<SystemTx, BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let recovered = self
            .expected_begin_system_txs
            .get(ordinal)
            .ok_or(ExecutorValidationError::MissingExpectedBeginSystemTx { ordinal })?;
        SystemTx::decode(recovered.tx().input().as_ref()).map_err(|error| {
            ExecutorValidationError::decode_expected_begin_system_tx_input(error).into()
        })
    }

    /// Layout-signaled flag for the one-time Phase 3b `TeeBootstrap`:
    /// true iff this block carries that system tx in the begin zone. Verifier
    /// mode reads it from `expected_begin_system_txs` (the body). Proposer mode
    /// reads it from the injected `pending_tee_bootstrap` payload. Both feed the
    /// same `has_tee_bootstrap` cursor signal so the phase cursor matches the
    /// actual begin-zone on both paths.
    pub(in crate::executor) fn block_has_tee_bootstrap(&self) -> bool {
        if self.pending_tee_bootstrap.is_some() {
            return true;
        }
        self.expected_begin_system_txs.iter().any(|tx| {
            matches!(
                SystemTx::decode(tx.input().as_ref()),
                Ok(SystemTx::TeeBootstrap { .. })
            )
        })
    }

    /// Advances the begin-zone system-transaction phase cursor and 4-stage FSM
    /// after committing a begin-zone system transaction receipt (or consuming a
    /// pre-executed Phase 1 witness).
    pub(in crate::executor) fn advance_system_tx_phase_cursor(
        &mut self,
        kind: outbe_primitives::system_tx::SystemTxKind,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        let has_boundary_outcome = matches!(
            block_artifacts.consensus_header_artifact,
            Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
        );
        let has_tee_bootstrap = self.block_has_tee_bootstrap();
        let has_publication = block_artifacts.radicle_publication.is_some();
        if self.execute_outbe_block_hooks {
            self.fsm
                .on_system_tx(kind)
                .map_err(crate::errors::ExecutorValidationError::from)?;
        }
        self.system_tx_phase_cursor = self
            .system_tx_phase_cursor
            .advance_after_commit_with_publication(
                has_boundary_outcome,
                has_tee_bootstrap,
                has_publication,
            );
        Ok(())
    }

    pub(in crate::executor) fn begin_block_system_tx_inputs(
        &self,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<Vec<BeginSystemTransaction>, BlockExecutionError> {
        let regime = BlockHeightRegime::for_block(block_number);
        // Genesis returns before inspecting queued bootstrap or body inputs.
        if !regime.has_begin_zone_system_txs() {
            return Ok(Vec::new());
        }
        let mut system_txs = Vec::new();
        if regime.has_parent_finalization_accounting() {
            self.append_parent_accounting_inputs(&mut system_txs, block_artifacts)?;
        }
        self.append_boundary_and_bootstrap_inputs(
            &mut system_txs,
            regime,
            block_number,
            block_artifacts,
        )?;
        self.append_begin_input(&mut system_txs, SystemTx::ValidatorLiveness)?;
        if let Some(batch) = &block_artifacts.radicle_publication {
            self.append_begin_input(
                &mut system_txs,
                SystemTx::RadiclePublication {
                    batch: batch.clone(),
                },
            )?;
        }
        for kind in [
            SystemTx::Vote,
            SystemTx::Update,
            SystemTx::EnclaveUpgradeDeadline,
            SystemTx::Rewards,
            SystemTx::Staking,
            SystemTx::Oracle,
            SystemTx::OracleSlash,
            SystemTx::HyperlaneSlash,
            SystemTx::CycleTick,
        ] {
            self.append_begin_input(&mut system_txs, kind)?;
        }
        Ok(system_txs)
    }

    fn append_parent_accounting_inputs(
        &self,
        system_txs: &mut Vec<BeginSystemTransaction>,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        let (input, summary) = match self.cached_phase1_input.clone() {
            Some(cached) => cached,
            None => self.certified_parent_begin_input(0)?,
        };
        system_txs.push((input, Some(summary)));
        self.append_begin_input(
            system_txs,
            SystemTx::LateFinalizeCredits {
                artifact: block_artifacts
                    .late_finalize_credits
                    .clone()
                    .unwrap_or_default(),
            },
        )
    }

    fn append_boundary_and_bootstrap_inputs(
        &self,
        system_txs: &mut Vec<BeginSystemTransaction>,
        regime: BlockHeightRegime,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        if let Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)) =
            &block_artifacts.consensus_header_artifact
        {
            let input = self.boundary_begin_input(system_txs.len(), artifact)?;
            system_txs.push((input, None));
        }
        if regime.requires_genesis_bootstrap_txs() {
            let input = self.bootstrap_begin_input(system_txs.len())?;
            system_txs.push((input, None));
        } else if self.pending_tee_bootstrap.is_some() {
            // Keep late OST3 rejection after boundary, before decoding Oracle.
            return Err(
                ExecutorValidationError::Ost3BootstrapForbiddenAtBlock { block_number }.into(),
            );
        }
        Ok(())
    }

    /// Admit a simple phase and advance its ordinal only after validation.
    fn append_begin_input(
        &self,
        system_txs: &mut Vec<BeginSystemTransaction>,
        expected_input: SystemTx,
    ) -> Result<(), BlockExecutionError> {
        let input = if self.expected_begin_system_txs.is_empty() {
            expected_input
        } else {
            let ordinal = system_txs.len();
            self.expected_begin_input(ordinal)?
        };
        system_txs.push((input, None));
        Ok(())
    }

    pub(in crate::executor) fn certified_parent_begin_input(
        &self,
        ordinal: usize,
    ) -> Result<(SystemTx, AccountedParentArtifact), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let verifier_mode = !self.expected_begin_system_txs.is_empty();
        let input = if verifier_mode {
            self.expected_begin_input(ordinal)?
        } else {
            let metadata = self
                .parent_consensus_metadata
                .clone()
                .ok_or(ExecutorValidationError::MissingParentConsensusMetadata)?;
            SystemTx::CertifiedParentAccounting { metadata }
        };
        let SystemTx::CertifiedParentAccounting { metadata } = &input else {
            return Err(
                ExecutorValidationError::ExpectedCertifiedParentAccountingAtOrdinal0.into(),
            );
        };
        if metadata.finalized_block_hash != self.parent_hash {
            return Err(
                ExecutorValidationError::CertifiedParentAccountingHashMismatch {
                    expected: self.parent_hash,
                    actual: metadata.finalized_block_hash,
                }
                .into(),
            );
        }
        let summary = self.accounted_parent_artifact_for_metadata(metadata)?;
        Ok((input, summary))
    }

    fn boundary_begin_input(
        &self,
        ordinal: usize,
        artifact: &outbe_primitives::consensus::DkgBoundaryArtifact,
    ) -> Result<SystemTx, BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let verifier_mode = !self.expected_begin_system_txs.is_empty();
        let input = if verifier_mode {
            self.expected_begin_input(ordinal)?
        } else {
            SystemTx::BoundaryOutcome {
                artifact: artifact.clone(),
            }
        };
        match &input {
            SystemTx::BoundaryOutcome {
                artifact: input_artifact,
            } if input_artifact == artifact => {}
            SystemTx::BoundaryOutcome { .. } => {
                return Err(
                    ExecutorValidationError::BoundaryOutcomeArtifactMismatch { ordinal }.into(),
                );
            }
            _ => {
                return Err(
                    ExecutorValidationError::ExpectedBoundaryOutcomeAtOrdinal { ordinal }.into(),
                );
            }
        }
        Ok(input)
    }

    fn bootstrap_begin_input(&self, ordinal: usize) -> Result<SystemTx, BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let verifier_mode = !self.expected_begin_system_txs.is_empty();
        let input = if verifier_mode {
            self.expected_begin_input(ordinal)?
        } else {
            let payload = self
                .pending_tee_bootstrap
                .clone()
                .ok_or(ExecutorValidationError::MissingBlock1Ost3BootstrapPayload)?;
            SystemTx::TeeBootstrap { payload }
        };
        if !matches!(input, SystemTx::TeeBootstrap { .. }) {
            return Err(ExecutorValidationError::ExpectedMandatoryOst3AtOrdinal { ordinal }.into());
        }
        Ok(input)
    }

    fn ensure_resolved_begin_zone(
        &mut self,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<&[ResolvedBeginSystemTx], BlockExecutionError> {
        if self.resolved_begin_zone.is_none() {
            let resolved = self.build_resolved_begin_zone(block_number, block_artifacts)?;
            return Ok(self.resolved_begin_zone.insert(resolved).as_slice());
        }
        Ok(self.resolved_begin_zone.as_deref().unwrap_or(&[]))
    }

    fn build_resolved_begin_zone(
        &self,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<Vec<ResolvedBeginSystemTx>, BlockExecutionError> {
        let system_txs = self.begin_block_system_tx_inputs(block_number, block_artifacts)?;
        Ok(system_txs
            .into_iter()
            .map(|(input, finalized_summary)| ResolvedBeginSystemTx {
                input,
                finalized_summary,
            })
            .collect())
    }

    fn expected_system_tx_at_body_index(
        &mut self,
        body_index: usize,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<(SystemTx, Option<AccountedParentArtifact>), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let resolved = self.ensure_resolved_begin_zone(block_number, block_artifacts)?;
        resolved
            .get(body_index)
            .map(|entry| (entry.input.clone(), entry.finalized_summary))
            .ok_or_else(|| {
                ExecutorValidationError::VisibleGasPlanMissingIntrinsicGas { body_index }.into()
            })
    }

    /// resolve the expected system tx for the current cursor
    /// position. Replaces the receipts-len-driven routing for begin-zone
    /// system transactions. The cursor is the single source of truth.
    /// Returns the resolved `(SystemTx,
    /// finalized_summary)` plus the body index the cursor is pointing at.
    /// Errors if the cursor is `UserTxs` (no system tx expected). Also errors if
    /// the expected kind of the cursor does not match the resolved expected kind
    /// for that body index. An example is block 1 + Phase 1 cursor, which is a
    /// programmer invariant violation.
    pub(in crate::executor) fn expected_system_tx_for_cursor(
        &mut self,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<ExpectedSystemTransaction, BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let cursor = self.system_tx_phase_cursor;
        if matches!(
            cursor,
            crate::system_tx::SystemTxPhase::EndBlockNodFactory
                | crate::system_tx::SystemTxPhase::EndBlockGemFactory
                | crate::system_tx::SystemTxPhase::EndBlockIntexFactory
                | crate::system_tx::SystemTxPhase::EndBlockCredisFactory
                | crate::system_tx::SystemTxPhase::EndBlockGemCleanup
                | crate::system_tx::SystemTxPhase::EndBlock
        ) {
            return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
        }
        let Some(body_index) = cursor.body_index() else {
            // Cursor=UserTxs: all begin-zone system txs are consumed.
            // A reserved system transaction address here has two possible
            // causes: an unsolicited user-tx attempt at the reserved address,
            // or a duplicate / out-of-band system tx. Both are fatal.
            return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
        };
        let body_index_usize = usize::from(body_index);
        let (input, finalized_summary) =
            self.expected_system_tx_at_body_index(body_index_usize, block_number, block_artifacts)?;
        if let Some(expected_kind) = cursor.expected_selector() {
            if expected_kind != input.selector() {
                return Err(ExecutorValidationError::SystemTxCursorBodyMismatch {
                    body_index: body_index_usize,
                    expected_kind,
                    resolved_kind: input.selector(),
                }
                .into());
            }
        }
        Ok((body_index_usize, input, finalized_summary))
    }
}
