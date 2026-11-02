use super::super::*;

type BeginSystemTransaction = (SystemTx, Option<AccountedParentArtifact>);

type ExpectedSystemTransaction = (usize, SystemTx, Option<AccountedParentArtifact>, u64, u64);

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

    pub(in crate::executor) fn begin_block_system_tx_inputs(
        &self,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<Vec<BeginSystemTransaction>, BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        // Genesis returns before inspecting queued bootstrap or body inputs.
        if block_number == 0 {
            return Ok(Vec::new());
        }
        use crate::system_tx::{LATE_FINALIZE_CREDITS_SELECTOR, RADICLE_PUBLICATION_SELECTOR};

        let mut system_txs = Vec::new();
        if block_number >= 2 {
            system_txs.push(self.certified_parent_begin_input(0)?);
            self.append_begin_input(&mut system_txs, LATE_FINALIZE_CREDITS_SELECTOR, || {
                SystemTx::LateFinalizeCredits {
                    artifact: block_artifacts
                        .late_finalize_credits
                        .clone()
                        .unwrap_or_default(),
                }
            })?;
        }
        self.append_begin_input(&mut system_txs, SystemTx::CycleTick.selector(), || {
            SystemTx::CycleTick
        })?;
        if let Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)) =
            &block_artifacts.consensus_header_artifact
        {
            let input = self.boundary_begin_input(system_txs.len(), artifact)?;
            system_txs.push((input, None));
        }
        if block_number == 1 {
            let input = self.bootstrap_begin_input(system_txs.len())?;
            system_txs.push((input, None));
        }
        // Keep late OST3 rejection after boundary, before decoding Oracle.
        if block_number != 1 && self.pending_tee_bootstrap.is_some() {
            return Err(
                ExecutorValidationError::Ost3BootstrapForbiddenAtBlock { block_number }.into(),
            );
        }
        self.append_begin_input(
            &mut system_txs,
            SystemTx::OracleSlashWindow.selector(),
            || SystemTx::OracleSlashWindow,
        )?;
        self.append_begin_input(&mut system_txs, SystemTx::HookEvents.selector(), || {
            SystemTx::HookEvents
        })?;
        if let Some(batch) = &block_artifacts.radicle_publication {
            self.append_begin_input(&mut system_txs, RADICLE_PUBLICATION_SELECTOR, || {
                SystemTx::RadiclePublication {
                    batch: batch.clone(),
                }
            })?;
        }
        Ok(system_txs)
    }

    /// Admit a simple phase and advance its ordinal only after validation.
    fn append_begin_input(
        &self,
        system_txs: &mut Vec<BeginSystemTransaction>,
        expected_selector: [u8; 4],
        proposer_input: impl FnOnce() -> SystemTx,
    ) -> Result<(), BlockExecutionError> {
        let ordinal = system_txs.len();
        let input = if self.expected_begin_system_txs.is_empty() {
            proposer_input()
        } else {
            self.expected_begin_input(ordinal)?
        };
        if input.selector() != expected_selector {
            return Err(
                crate::errors::ExecutorValidationError::ExpectedSystemTxAtOrdinal {
                    expected_kind: expected_selector,
                    ordinal,
                }
                .into(),
            );
        }
        system_txs.push((input, None));
        Ok(())
    }

    fn certified_parent_begin_input(
        &self,
        ordinal: usize,
    ) -> Result<BeginSystemTransaction, BlockExecutionError> {
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
        Ok((input, Some(summary)))
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

    fn expected_system_tx_at_body_index(
        &self,
        body_index: usize,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<(SystemTx, Option<AccountedParentArtifact>, u64, u64), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let system_txs = self.begin_block_system_tx_inputs(block_number, block_artifacts)?;
        let gas_inputs = system_txs
            .iter()
            .map(|(input, _)| {
                input.encode().map_err(|error| -> BlockExecutionError {
                    ExecutorValidationError::encode_system_tx_for_visible_gas_plan(error).into()
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let gas_plan = SystemTxVisibleGasPlan::new(self.inner.evm.block().gas_limit(), &gas_inputs)
            .map_err(|error| -> BlockExecutionError {
                ExecutorValidationError::plan_visible_system_tx_gas(error).into()
            })?;
        let intrinsic_gas = gas_plan
            .intrinsic_gas(body_index)
            .ok_or(ExecutorValidationError::VisibleGasPlanMissingIntrinsicGas { body_index })?;
        let protocol_precharge = gas_plan.protocol_precharge(body_index).ok_or(
            ExecutorValidationError::VisibleGasPlanMissingProtocolPrecharge { body_index },
        )?;
        let visible_base_gas = intrinsic_gas
            .checked_add(protocol_precharge)
            .ok_or(ExecutorValidationError::VisibleBaseGasOverflow { body_index })?;
        let gas_limit = gas_plan
            .gas_limit(body_index)
            .ok_or(ExecutorValidationError::VisibleGasPlanMissingGasLimit { body_index })?;
        system_txs
            .into_iter()
            .nth(body_index)
            .map(|(input, summary)| (input, summary, visible_base_gas, gas_limit))
            .ok_or_else(|| {
                let has_boundary_outcome = matches!(
                    block_artifacts.consensus_header_artifact,
                    Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
                );
                let has_tee_bootstrap = self.block_has_tee_bootstrap();
                let expected =
                    outbe_primitives::system_tx::expected_begin_block_selectors_for_publication(
                        block_number,
                        has_boundary_outcome,
                        has_tee_bootstrap,
                        block_artifacts.radicle_publication.is_some(),
                    );
                ExecutorValidationError::UnexpectedBeginSystemTx {
                    body_index,
                    expected,
                }
                .into()
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
        &self,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<ExpectedSystemTransaction, BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let cursor = self.system_tx_phase_cursor;
        let Some(body_index) = cursor.body_index() else {
            // Cursor=UserTxs: all begin-zone system txs are consumed.
            // A reserved system transaction address here has two possible
            // causes: an unsolicited user-tx attempt at the reserved address,
            // or a duplicate / out-of-band system tx. Both are fatal.
            return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
        };
        let body_index_usize = usize::from(body_index);
        let (input, finalized_summary, visible_base_gas, gas_limit) =
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
        Ok((
            body_index_usize,
            input,
            finalized_summary,
            visible_base_gas,
            gas_limit,
        ))
    }
}
