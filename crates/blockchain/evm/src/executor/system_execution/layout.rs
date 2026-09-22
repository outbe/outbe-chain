use super::super::*;

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
    /// Test-only introspection point; the production driver is internal.
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
    /// mode reads it from `expected_begin_system_txs` (the body); proposer mode
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
    ) -> Result<
        Vec<(
            SystemTxKind,
            SystemTxInputV2,
            Option<AccountedParentArtifact>,
        )>,
        BlockExecutionError,
    > {
        // Block 0 (genesis) has no begin-zone system txs. Mirror the proposer
        // body builder (`OutbeEvmConfig::build_begin_system_txs`), which returns
        // empty for block 0, so both deterministic paths agree even if a stray
        // `pending_tee_bootstrap` is set - never inject a begin-zone tx at genesis.
        if block_number == 0 {
            return Ok(Vec::new());
        }

        let verifier_mode = !self.expected_begin_system_txs.is_empty();
        let mut ordinal = 0usize;
        let mut system_txs = Vec::new();

        if block_number >= 2 {
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
            system_txs.push((
                SystemTxKind::CertifiedParentAccounting,
                input,
                Some(summary),
            ));
            ordinal += 1;
        }

        // mandatory LateFinalizeCredits phase for every block >= 2,
        // ordered immediately after Phase 1 (CPA). Proposer mode builds it from
        // the header artifact (empty until Phase 7 wires gathered credits);
        // verifier mode re-derives it from the body and the header<->calldata
        // parity check enforces equality.
        if block_number >= 2 {
            let input = if verifier_mode {
                self.expected_begin_input(ordinal)?
            } else {
                SystemTxInputV2::LateFinalizeCredits {
                    artifact: block_artifacts
                        .late_finalize_credits
                        .clone()
                        .unwrap_or_default(),
                }
            };
            if !matches!(input, SystemTxInputV2::LateFinalizeCredits { .. }) {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!("expected LateFinalizeCredits system tx at ordinal {ordinal}")
                            .into(),
                    ),
                ));
            }
            system_txs.push((SystemTxKind::LateFinalizeCredits, input, None));
            ordinal += 1;
        }

        if self.ocomp_lifecycle_active {
            let input = if verifier_mode {
                self.expected_begin_input(ordinal)?
            } else {
                SystemTxInputV2::OcompLifecycleBegin
            };
            if !matches!(input, SystemTxInputV2::OcompLifecycleBegin) {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!("expected OcompLifecycleBegin system tx at ordinal {ordinal}")
                            .into(),
                    ),
                ));
            }
            system_txs.push((SystemTxKind::OcompLifecycleBegin, input, None));
            ordinal += 1;
        }

        if block_number >= 1 {
            let input = if verifier_mode {
                self.expected_begin_input(ordinal)?
            } else {
                SystemTxInputV2::CycleTick
            };
            if !matches!(input, SystemTxInputV2::CycleTick) {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!("expected CycleTick system tx at ordinal {ordinal}").into(),
                    ),
                ));
            }
            system_txs.push((SystemTxKind::CycleTick, input, None));
            ordinal += 1;

            let input = if verifier_mode {
                self.expected_begin_input(ordinal)?
            } else {
                SystemTxInputV2::RewardsGemDelivery
            };
            if !matches!(input, SystemTxInputV2::RewardsGemDelivery) {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!("expected RewardsGemDelivery system tx at ordinal {ordinal}")
                            .into(),
                    ),
                ));
            }
            system_txs.push((SystemTxKind::RewardsGemDelivery, input, None));
            ordinal += 1;
        }

        if let Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)) =
            &block_artifacts.consensus_header_artifact
        {
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
                            format!(
                                "BoundaryOutcome system tx artifact mismatch at ordinal {ordinal}"
                            )
                            .into(),
                        ),
                    ));
                }
                _ => {
                    return Err(BlockExecutionError::Internal(
                        InternalBlockExecutionError::Other(
                            format!("expected BoundaryOutcome system tx at ordinal {ordinal}")
                                .into(),
                        ),
                    ));
                }
            }
            system_txs.push((SystemTxKind::BoundaryOutcome, input, None));
            ordinal += 1;
        }

        // Optional Phase 3b: one-time `TeeBootstrap`, between `BoundaryOutcome`
        // (begin_order 5) and `OracleSlashWindow` (begin_order 7).
        // Verifier mode: include it iff the body carries it at this ordinal.
        // Proposer mode: inject the `pending_tee_bootstrap` payload supplied by
        // the bootstrap producer - identically to `build_begin_system_txs` so the
        // proposer's signed body and the executor's expected inputs match.
        if block_number == 1 {
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
            system_txs.push((SystemTxKind::TeeBootstrap, input, None));
            ordinal += 1;
        } else if self.pending_tee_bootstrap.is_some() {
            return Err(BlockExecutionError::Internal(
                InternalBlockExecutionError::Other(
                    format!("OST3 bootstrap payload is forbidden at block {block_number}").into(),
                ),
            ));
        }

        if block_number >= 1 {
            let input = if verifier_mode {
                self.expected_begin_input(ordinal)?
            } else {
                SystemTxInputV2::OracleSlashWindow
            };
            if !matches!(input, SystemTxInputV2::OracleSlashWindow) {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!("expected OracleSlashWindow system tx at ordinal {ordinal}").into(),
                    ),
                ));
            }
            system_txs.push((SystemTxKind::OracleSlashWindow, input, None));
            ordinal += 1;
        }

        if block_number >= 1 {
            let input = if verifier_mode {
                self.expected_begin_input(ordinal)?
            } else {
                SystemTxInputV2::HookEvents
            };
            if !matches!(input, SystemTxInputV2::HookEvents) {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!("expected HookEvents system tx at ordinal {ordinal}").into(),
                    ),
                ));
            }
            system_txs.push((SystemTxKind::HookEvents, input, None));
        }

        Ok(system_txs)
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
    /// system transactions; the cursor is the single source of truth.
    /// Returns the resolved `(SystemTxKind, SystemTxInputV2,
    /// finalized_summary)` plus the body index the cursor is pointing at.
    /// Errors if the cursor is `UserTxs` (no system tx expected) or if the
    /// cursor's expected kind does not match the resolved expected kind for
    /// that body index (e.g. block 1 + Phase 1 cursor - a programmer
    /// invariant violation).
    pub(in crate::executor) fn expected_system_tx_for_cursor(
        &self,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<ExpectedSystemTransaction, BlockExecutionError> {
        let cursor = self.system_tx_phase_cursor;
        let Some(body_index) = cursor.body_index() else {
            // Cursor=UserTxs: all begin-zone system txs are consumed.
            // Encountering a reserved system transaction address here is
            // either an unsolicited user-tx attempt at the reserved
            // address or a duplicate / out-of-band system tx - both fatal.
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
