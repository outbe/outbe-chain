use super::super::*;

type BeginSystemTransaction = (
    SystemTxKind,
    SystemTxInputV2,
    Option<AccountedParentArtifact>,
);

type ExpectedSystemTransaction = (
    usize,
    SystemTxKind,
    SystemTxInputV2,
    Option<AccountedParentArtifact>,
    u64,
    u64,
);

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
    fn expected_begin_input(&self, ordinal: usize) -> Result<SystemTxInputV2, BlockExecutionError> {
        let recovered = self.expected_begin_system_txs.get(ordinal).ok_or_else(|| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!("missing expected begin system tx at ordinal {ordinal}").into(),
            ))
        })?;
        SystemTxInputV2::decode(recovered.tx().input().as_ref()).map_err(|error| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!("decode expected begin system tx input: {error}").into(),
            ))
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
                SystemTxInputV2::decode(tx.input().as_ref()).map(|input| input.kind()),
                Ok(SystemTxKind::TeeBootstrap)
            )
        })
    }

    pub(in crate::executor) fn begin_block_system_tx_inputs(
        &self,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<Vec<BeginSystemTransaction>, BlockExecutionError> {
        // Genesis returns before inspecting queued bootstrap or body inputs.
        if block_number == 0 {
            return Ok(Vec::new());
        }
        let has_boundary_outcome = matches!(
            block_artifacts.consensus_header_artifact,
            Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
        );
        let ocomp_activation = if self.ocomp_lifecycle_active {
            OcompLifecycleActivation::at_block(0)
        } else {
            OcompLifecycleActivation::Disabled
        };
        // The shared layout defines order. Block 1 always reserves mandatory OST3.
        let kinds = expected_begin_block_kinds_for_activation(
            block_number,
            has_boundary_outcome,
            block_number == 1,
            ocomp_activation,
        );
        let mut system_txs = Vec::new();
        for kind in &kinds {
            let ordinal = system_txs.len();
            match *kind {
                SystemTxKind::CertifiedParentAccounting => {
                    system_txs.push(self.certified_parent_begin_input(ordinal)?);
                }
                SystemTxKind::BoundaryOutcome => {
                    if let Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)) =
                        &block_artifacts.consensus_header_artifact
                    {
                        let input = self.boundary_begin_input(ordinal, artifact)?;
                        system_txs.push((*kind, input, None));
                    }
                }
                SystemTxKind::TeeBootstrap => {
                    let input = self.bootstrap_begin_input(ordinal)?;
                    system_txs.push((*kind, input, None));
                }
                SystemTxKind::LateFinalizeCredits => {
                    self.append_begin_input(&mut system_txs, *kind, || {
                        SystemTxInputV2::LateFinalizeCredits {
                            artifact: block_artifacts
                                .late_finalize_credits
                                .clone()
                                .unwrap_or_default(),
                        }
                    })?
                }
                SystemTxKind::OcompLifecycleBegin => {
                    self.append_begin_input(&mut system_txs, *kind, || {
                        SystemTxInputV2::OcompLifecycleBegin
                    })?
                }
                SystemTxKind::CycleTick => {
                    self.append_begin_input(&mut system_txs, *kind, || SystemTxInputV2::CycleTick)?
                }
                SystemTxKind::RewardsGemDelivery => {
                    self.append_begin_input(&mut system_txs, *kind, || {
                        SystemTxInputV2::RewardsGemDelivery
                    })?
                }
                SystemTxKind::OracleSlashWindow => {
                    // Keep late OST3 rejection after boundary, before decoding Oracle.
                    if block_number != 1 && self.pending_tee_bootstrap.is_some() {
                        return Err(BlockExecutionError::Internal(
                            InternalBlockExecutionError::Other(
                                format!(
                                    "OST3 bootstrap payload is forbidden at block {block_number}"
                                )
                                .into(),
                            ),
                        ));
                    }
                    self.append_begin_input(&mut system_txs, *kind, || {
                        SystemTxInputV2::OracleSlashWindow
                    })?;
                }
                SystemTxKind::HookEvents => {
                    self.append_begin_input(&mut system_txs, *kind, || SystemTxInputV2::HookEvents)?
                }
                // The canonical begin plan excludes the end-zone terminal request.
                SystemTxKind::OcompTerminalRequest => {
                    return Err(BlockExecutionError::Internal(
                        InternalBlockExecutionError::Other(
                            format!("unexpected system tx at body_index={ordinal}; expected begin_block system txs {kinds:?}").into(),
                        ),
                    ));
                }
            }
        }
        Ok(system_txs)
    }

    /// Admit a simple phase and advance its ordinal only after validation.
    fn append_begin_input(
        &self,
        system_txs: &mut Vec<BeginSystemTransaction>,
        expected_kind: SystemTxKind,
        proposer_input: impl FnOnce() -> SystemTxInputV2,
    ) -> Result<(), BlockExecutionError> {
        let ordinal = system_txs.len();
        let input = if self.expected_begin_system_txs.is_empty() {
            proposer_input()
        } else {
            self.expected_begin_input(ordinal)?
        };
        if input.kind() != expected_kind {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!("expected {expected_kind:?} system tx at ordinal {ordinal}").into(),
                ),
            ));
        }
        system_txs.push((expected_kind, input, None));
        Ok(())
    }

    fn certified_parent_begin_input(
        &self,
        ordinal: usize,
    ) -> Result<BeginSystemTransaction, BlockExecutionError> {
        let verifier_mode = !self.expected_begin_system_txs.is_empty();
        let input = if verifier_mode {
            self.expected_begin_input(ordinal)?
        } else {
            let metadata = self.parent_consensus_metadata.clone().ok_or_else(|| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    "missing parent consensus metadata for CertifiedParentAccounting".into(),
                ))
            })?;
            SystemTxInputV2::CertifiedParentAccounting { metadata }
        };
        let SystemTxInputV2::CertifiedParentAccounting { metadata } = &input else {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    "expected CertifiedParentAccounting system tx at ordinal 0".into(),
                ),
            ));
        };
        if metadata.finalized_block_hash != self.parent_hash {
            return Err(BlockExecutionError::Internal(
            InternalBlockExecutionError::Other(
                format!(
                    "CertifiedParentAccounting metadata hash must match block parent: expected {}, got {}",
                    self.parent_hash, metadata.finalized_block_hash
                )
                .into(),
            ),
        ));
        }
        let summary = self.accounted_parent_artifact_for_metadata(metadata)?;
        Ok((
            SystemTxKind::CertifiedParentAccounting,
            input,
            Some(summary),
        ))
    }

    fn boundary_begin_input(
        &self,
        ordinal: usize,
        artifact: &outbe_primitives::consensus::DkgBoundaryArtifact,
    ) -> Result<SystemTxInputV2, BlockExecutionError> {
        let verifier_mode = !self.expected_begin_system_txs.is_empty();
        let input = if verifier_mode {
            self.expected_begin_input(ordinal)?
        } else {
            SystemTxInputV2::BoundaryOutcome {
                artifact: artifact.clone(),
            }
        };
        match &input {
            SystemTxInputV2::BoundaryOutcome {
                artifact: input_artifact,
            } if input_artifact == artifact => {}
            SystemTxInputV2::BoundaryOutcome { .. } => {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!("BoundaryOutcome system tx artifact mismatch at ordinal {ordinal}")
                            .into(),
                    ),
                ));
            }
            _ => {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!("expected BoundaryOutcome system tx at ordinal {ordinal}").into(),
                    ),
                ));
            }
        }
        Ok(input)
    }

    fn bootstrap_begin_input(
        &self,
        ordinal: usize,
    ) -> Result<SystemTxInputV2, BlockExecutionError> {
        let verifier_mode = !self.expected_begin_system_txs.is_empty();
        let input = if verifier_mode {
            self.expected_begin_input(ordinal)?
        } else {
            let payload = self.pending_tee_bootstrap.clone().ok_or_else(|| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    "missing mandatory block-1 OST3 bootstrap payload".into(),
                ))
            })?;
            SystemTxInputV2::TeeBootstrap { payload }
        };
        if !matches!(input, SystemTxInputV2::TeeBootstrap { .. }) {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!("expected mandatory OST3 system tx at ordinal {ordinal}").into(),
                ),
            ));
        }
        Ok(input)
    }

    fn expected_system_tx_at_body_index(
        &self,
        body_index: usize,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<
        (
            SystemTxKind,
            SystemTxInputV2,
            Option<AccountedParentArtifact>,
            u64,
            u64,
        ),
        BlockExecutionError,
    > {
        let system_txs = self.begin_block_system_tx_inputs(block_number, block_artifacts)?;
        let mut gas_inputs = system_txs
            .iter()
            .map(|(kind, input, _)| {
                input
                    .encode()
                    .map(|calldata| (*kind, calldata))
                    .map_err(|error| {
                        BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                            format!("encode system tx for visible gas plan: {error}").into(),
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if self.ocomp_lifecycle_active {
            let terminal = SystemTxInputV2::OcompTerminalRequest;
            gas_inputs.push((
                terminal.kind(),
                terminal.encode().map_err(|error| {
                    BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                        format!("encode terminal system tx for visible gas plan: {error}").into(),
                    ))
                })?,
            ));
        }
        let gas_plan = SystemTxVisibleGasPlan::new(self.inner.evm.block().gas_limit(), &gas_inputs)
            .map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!("plan visible system tx gas: {error}").into(),
                ))
            })?;
        let intrinsic_gas = gas_plan.intrinsic_gas(body_index).ok_or_else(|| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!("visible gas plan missing intrinsic gas for body_index={body_index}")
                    .into(),
            ))
        })?;
        let protocol_precharge = gas_plan.protocol_precharge(body_index).ok_or_else(|| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!("visible gas plan missing protocol precharge for body_index={body_index}")
                    .into(),
            ))
        })?;
        let visible_base_gas = intrinsic_gas
            .checked_add(protocol_precharge)
            .ok_or_else(|| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!("visible base gas overflow for body_index={body_index}").into(),
                ))
            })?;
        let gas_limit = gas_plan.gas_limit(body_index).ok_or_else(|| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!("visible gas plan missing gas limit for body_index={body_index}").into(),
            ))
        })?;
        system_txs
            .into_iter()
            .nth(body_index)
            .map(|(kind, input, summary)| (kind, input, summary, visible_base_gas, gas_limit))
            .ok_or_else(|| {
            let has_boundary_outcome = matches!(
                block_artifacts.consensus_header_artifact,
                Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
            );
            let has_tee_bootstrap = self.block_has_tee_bootstrap();
            let ocomp_activation = if self.ocomp_lifecycle_active {
                OcompLifecycleActivation::at_block(0)
            } else {
                OcompLifecycleActivation::Disabled
            };
            let expected = expected_begin_block_kinds_for_activation(
                block_number,
                has_boundary_outcome,
                has_tee_bootstrap,
                ocomp_activation,
            );
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!(
                    "unexpected system tx at body_index={body_index}; expected begin_block system txs {expected:?}"
                )
                .into(),
            ))
            })
    }

    /// resolve the expected system tx for the current cursor
    /// position. Replaces the receipts-len-driven routing for begin-zone
    /// system transactions. The cursor is the single source of truth.
    /// Returns the resolved `(SystemTxKind, SystemTxInputV2,
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
        let cursor = self.system_tx_phase_cursor;
        let Some(body_index) = cursor.body_index() else {
            // Cursor=UserTxs: all begin-zone system txs are consumed.
            // A reserved system transaction address here has two possible
            // causes: an unsolicited user-tx attempt at the reserved address,
            // or a duplicate / out-of-band system tx. Both are fatal.
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    "tx to reserved system transaction address after begin-zone system txs are consumed"
                        .into(),
                ),
            ));
        };
        let body_index_usize = usize::from(body_index);
        let (resolved_kind, input, finalized_summary, visible_base_gas, gas_limit) =
            self.expected_system_tx_at_body_index(body_index_usize, block_number, block_artifacts)?;
        if let Some(expected_kind) = cursor.expected_kind() {
            if expected_kind != resolved_kind {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!(
                            "system tx cursor/body mismatch at body_index={body_index_usize}: cursor expects {expected_kind:?}, body has {resolved_kind:?}"
                        )
                        .into(),
                    ),
                ));
            }
        }
        Ok((
            body_index_usize,
            resolved_kind,
            input,
            finalized_summary,
            visible_base_gas,
            gas_limit,
        ))
    }
}
