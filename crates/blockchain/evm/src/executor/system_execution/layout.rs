use super::super::*;
use crate::errors::ExecutorValidationError;
use {
    crate::system_tx::{assemble_prefix_system_txs, BlockSystemPlan},
    outbe_primitives::{
        block::BlockSystemFlags,
        system_tx::{
            SystemTxStepId, CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
            CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
        },
        system_tx_context::{SystemTxBuildContext, SystemTxValidationContext},
    },
};

type BeginSystemTransaction = (SystemTx, Option<AccountedParentArtifact>);
type ExpectedSystemTransaction = (usize, SystemTx, Option<AccountedParentArtifact>);

#[derive(Debug, Clone)]
pub struct ResolvedBeginSystemTx {
    pub input: SystemTx,
    pub finalized_summary: Option<AccountedParentArtifact>,
}

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
    pub fn is_preexecuted_phase1_witness(&self, tx: &TransactionSigned) -> bool {
        self.preexecuted_phase1_tx_hash.is_some_and(|cached_hash| {
            is_reserved_system_tx(tx) && tx.signature_hash() == cached_hash
        })
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
        let recovered = self
            .expected_begin_system_txs
            .get(ordinal)
            .ok_or(ExecutorValidationError::MissingExpectedBeginSystemTx { ordinal })?;
        SystemTx::decode(recovered.tx().input().as_ref()).map_err(|error| {
            ExecutorValidationError::decode_expected_begin_system_tx_input(error).into()
        })
    }

    pub fn block_has_tee_bootstrap(&self) -> bool {
        self.pending_tee_bootstrap.is_some()
            || self.expected_begin_system_txs.iter().any(|tx| {
                matches!(
                    SystemTx::decode(tx.input()),
                    Ok(SystemTx::Consensus {
                        step_index: CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
                        ..
                    })
                )
            })
    }

    pub fn begin_block_system_tx_inputs(
        &self,
        block_number: u64,
        block_artifacts: &OutbeBlockArtifacts,
    ) -> Result<Vec<BeginSystemTransaction>, BlockExecutionError> {
        let regime = BlockHeightRegime::for_block(block_number);
        if !regime.has_system_tx_prefix() {
            return Ok(Vec::new());
        }
        if !regime.requires_genesis_bootstrap_txs() && self.pending_tee_bootstrap.is_some() {
            return Err(
                ExecutorValidationError::Ost3BootstrapForbiddenAtBlock { block_number }.into(),
            );
        }
        let val_ctx = SystemTxValidationContext {
            regime,
            block_number,
            parent_hash: self.parent_hash,
            artifacts: block_artifacts,
        };
        let inputs = if self.expected_begin_system_txs.is_empty() {
            let build_ctx = SystemTxBuildContext {
                regime,
                block_number,
                parent_hash: self.parent_hash,
                artifacts: block_artifacts,
                parent_consensus_metadata: self.parent_consensus_metadata.as_ref(),
                pending_tee_bootstrap: self.pending_tee_bootstrap.as_ref(),
                expected_tee_policy: None,
            };
            assemble_prefix_system_txs(&self.block_order, &build_ctx)
                .map_err(ExecutorValidationError::from)?
        } else {
            let plan = BlockSystemPlan::for_block(
                block_number,
                BlockSystemFlags::from_artifacts(block_artifacts),
                &self.block_order,
            );
            let mut inputs =
                Vec::with_capacity(plan.consensus_steps.len() + plan.begin_block_steps.len());
            for (ordinal, planned) in plan
                .consensus_steps
                .iter()
                .chain(&plan.begin_block_steps)
                .enumerate()
            {
                let input = self.expected_begin_input(ordinal)?;
                if input.step_id() != planned.id {
                    return Err(ExecutorValidationError::SystemTxPhaseMismatch {
                        body_index: ordinal,
                        expected_phase: planned.step.name,
                        actual_phase: self.block_order.step_name_or_stage(&input),
                    }
                    .into());
                }
                (planned.step.validate_payload)(input.payload(), &val_ctx)
                    .map_err(ExecutorValidationError::from)?;
                inputs.push(input);
            }
            inputs
        };
        inputs
            .into_iter()
            .map(|input| {
                let summary = if input.step_id()
                    == SystemTxStepId::consensus(CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX)
                {
                    match &self.cached_phase1_input {
                        Some((_, summary)) => Some(*summary),
                        None => Some(self.parent_summary_for_input(&input)?),
                    }
                } else {
                    None
                };
                Ok((input, summary))
            })
            .collect()
    }

    fn parent_summary_for_input(
        &self,
        input: &SystemTx,
    ) -> Result<AccountedParentArtifact, BlockExecutionError> {
        let metadata = CertifiedParentAccountingMetadata::decode(input.payload())
            .map_err(ExecutorValidationError::block_artifacts)?;
        if metadata.finalized_block_hash != self.parent_hash {
            return Err(
                ExecutorValidationError::CertifiedParentAccountingHashMismatch {
                    expected: self.parent_hash,
                    actual: metadata.finalized_block_hash,
                }
                .into(),
            );
        }
        self.accounted_parent_artifact_for_metadata(&metadata)
    }

    pub fn certified_parent_begin_input(
        &self,
        ordinal: usize,
    ) -> Result<(SystemTx, AccountedParentArtifact), BlockExecutionError> {
        let input = if self.expected_begin_system_txs.is_empty() {
            let metadata = self
                .parent_consensus_metadata
                .as_ref()
                .ok_or(ExecutorValidationError::MissingParentConsensusMetadata)?;
            SystemTx::Consensus {
                step_index: CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
                payload: metadata
                    .encode()
                    .map_err(ExecutorValidationError::block_artifacts)?,
            }
        } else {
            self.expected_begin_input(ordinal)?
        };
        if input.step_id() != SystemTxStepId::consensus(CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX) {
            return Err(
                ExecutorValidationError::ExpectedCertifiedParentAccountingAtOrdinal0.into(),
            );
        }
        let summary = self.parent_summary_for_input(&input)?;
        Ok((input, summary))
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

    /// The FSM is the sole source of the next prefix transaction's body position.
    pub fn expected_system_tx_for_cursor(
        &mut self,
        block_number: u64,
        block_artifacts: &OutbeBlockArtifacts,
    ) -> Result<ExpectedSystemTransaction, BlockExecutionError> {
        let body_index = self
            .fsm
            .expected_prefix_body_index()
            .ok_or(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed)?;
        let (input, finalized_summary) =
            self.expected_system_tx_at_body_index(body_index, block_number, block_artifacts)?;
        if self.fsm.expected_step_id() != Some(input.step_id()) {
            return Err(ExecutorValidationError::SystemTxPhaseMismatch {
                body_index,
                expected_phase: self
                    .fsm
                    .expected_step()
                    .map_or("MidBlock", |step| step.step.name),
                actual_phase: self.block_order.step_name_or_stage(&input),
            }
            .into());
        }
        Ok((body_index, input, finalized_summary))
    }
}
