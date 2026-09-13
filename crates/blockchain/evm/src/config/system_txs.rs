use crate::system_tx::build_unsigned_system_tx;
use crate::system_tx::build_unsigned_system_tx_with_gas_limit;
use crate::system_tx::split_system_layout;
use crate::system_tx::validate_system_tx_set_for_activation;
use crate::system_tx::OcompLifecycleActivation;
use crate::system_tx::SystemTxInputV2;
use crate::system_tx::SystemTxKind;
use crate::system_tx::SystemTxVisibleGasPlan;

use alloy_consensus::Transaction as _;
use alloy_evm::RecoveredTx;

use alloy_primitives::{Address, Bytes, B256};

use outbe_primitives::{
    consensus_metadata::CertifiedParentAccountingMetadata,
    reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact},
    OutbeBlock,
};

use reth_ethereum::TransactionSigned;

use reth_evm::execute::BlockExecutionError;

use reth_primitives_traits::{
    AlloyBlockHeader as _, Recovered, SealedBlock, SignedTransaction as _,
};

use super::OutbeEvmConfig;

type SystemTxExpectations = (
    Vec<Recovered<TransactionSigned>>,
    Vec<Recovered<TransactionSigned>>,
    Option<String>,
    Option<Address>,
);

pub(super) fn system_tx_expectations_for_block(
    block: &SealedBlock<OutbeBlock>,
    ocomp_lifecycle_activation: OcompLifecycleActivation,
) -> SystemTxExpectations {
    let has_boundary_outcome =
        match decode_outbe_block_artifacts(block.header().extra_data().as_ref()) {
            Ok(artifacts) => matches!(
                artifacts.consensus_header_artifact,
                Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
            ),
            Err(error) => {
                return (
                    Vec::new(),
                    Vec::new(),
                    Some(format!(
                        "decode Outbe block artifacts for system tx validation: {error}"
                    )),
                    None,
                );
            }
        };

    let layout = match split_system_layout(&block.body().transactions) {
        Ok(layout) => layout,
        Err(error) => return (Vec::new(), Vec::new(), Some(error.to_string()), None),
    };

    let has_tee_bootstrap = layout.has_begin_kind(SystemTxKind::TeeBootstrap);
    if let Err(error) = validate_system_tx_set_for_activation(
        &layout,
        block.header().number(),
        has_boundary_outcome,
        has_tee_bootstrap,
        ocomp_lifecycle_activation,
    ) {
        return (Vec::new(), Vec::new(), Some(error.to_string()), None);
    }

    let recover = |tx: &TransactionSigned| -> Result<Recovered<TransactionSigned>, String> {
        let signer = tx
            .try_recover()
            .map_err(|error| format!("recover system tx signer: {error}"))?;
        Ok(Recovered::new_unchecked(tx.clone(), signer))
    };

    let mut begin = Vec::with_capacity(layout.begin.len());
    for tx in layout.begin {
        match recover(tx) {
            Ok(recovered) => begin.push(recovered),
            Err(error) => return (Vec::new(), Vec::new(), Some(error), None),
        }
    }

    let mut end = Vec::with_capacity(layout.end.len());
    for tx in layout.end {
        match recover(tx) {
            Ok(recovered) => end.push(recovered),
            Err(error) => return (Vec::new(), Vec::new(), Some(error), None),
        }
    }

    let proposer = begin
        .first()
        .or_else(|| end.first())
        .map(|tx| Address::from(*tx.signer()));
    (begin, end, None, proposer)
}

impl OutbeEvmConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn build_begin_system_txs(
        &self,
        block_number: u64,
        chain_id: u64,
        block_gas_limit: u64,
        parent_hash: B256,
        extra_data: &Bytes,
        parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
        proposer_evm_address: Option<Address>,
        prebuilt_phase1_tx: Option<Recovered<TransactionSigned>>,
        pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
    ) -> Result<Vec<Recovered<TransactionSigned>>, BlockExecutionError> {
        if block_number == 0 {
            if pending_tee_bootstrap.is_some() {
                return Err(BlockExecutionError::Internal(
                    alloy_evm::block::InternalBlockExecutionError::Other(
                        "OST3 bootstrap payload is invalid at genesis".into(),
                    ),
                ));
            }
            return Ok(Vec::new());
        }

        let signer = self.evm_signer.as_ref().ok_or_else(|| {
            BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                "missing EVM signer for proposer system tx".into(),
            ))
        })?;
        let proposer = proposer_evm_address.unwrap_or_else(|| signer.address());
        if signer.address() != proposer {
            return Err(BlockExecutionError::Internal(
                alloy_evm::block::InternalBlockExecutionError::Other(
                    format!(
                        "configured EVM signer {} does not match proposer {proposer}",
                        signer.address()
                    )
                    .into(),
                ),
            ));
        }

        let artifacts = decode_outbe_block_artifacts(extra_data.as_ref())
            .map_err(|error| BlockExecutionError::msg(error.to_string()))?;
        let mut inputs = Vec::new();
        if block_number >= 2 {
            let metadata = parent_consensus_metadata.ok_or_else(|| {
                BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                    "missing parent consensus metadata for CertifiedParentAccounting".into(),
                ))
            })?;
            if metadata.finalized_block_hash != parent_hash {
                return Err(BlockExecutionError::Internal(
                    alloy_evm::block::InternalBlockExecutionError::Other(
                        format!(
                            "CertifiedParentAccounting metadata hash must match block parent: expected {parent_hash}, got {}",
                            metadata.finalized_block_hash
                        )
                        .into(),
                    ),
                ));
            }
            inputs.push(SystemTxInputV2::CertifiedParentAccounting { metadata });
        }
        if block_number >= 2 {
            // mandatory inclusion-window phase after Phase 1. The
            // gathered credits ride in the header artifact (empty until Phase 7);
            // executor parity re-derives the same input on the verifier path.
            inputs.push(SystemTxInputV2::LateFinalizeCredits {
                artifact: artifacts.late_finalize_credits.clone().unwrap_or_default(),
            });
        }
        if self.ocomp_lifecycle_active_at(block_number) {
            inputs.push(SystemTxInputV2::OcompLifecycleBegin);
        }
        if block_number >= 1 {
            inputs.push(SystemTxInputV2::CycleTick);
            inputs.push(SystemTxInputV2::RewardsGemDelivery);
        }
        if let Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)) =
            artifacts.consensus_header_artifact
        {
            inputs.push(SystemTxInputV2::BoundaryOutcome { artifact });
        }
        // The greenfield network has exactly one bootstrap wire path: canonical
        // OST3 is mandatory at block 1 and forbidden at every other height.
        if block_number == 1 {
            let payload = pending_tee_bootstrap.ok_or_else(|| {
                BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                    "missing mandatory block-1 OST3 bootstrap payload".into(),
                ))
            })?;
            payload.preflight().map_err(|error| {
                BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                    format!("invalid block-1 OST3 bootstrap payload: {error}").into(),
                ))
            })?;
            let activation = self.tee_attestation_v1.activation().map_err(|error| {
                BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                    format!("invalid mandatory TEE ChainSpec authority: {error}").into(),
                ))
            })?;
            let expected_policy = activation.policy_at(block_number).map_err(|error| {
                BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                    error.into(),
                ))
            })?;
            if &payload.policy != expected_policy {
                return Err(BlockExecutionError::Internal(
                    alloy_evm::block::InternalBlockExecutionError::Other(
                        "block-1 OST3 policy does not match the immutable ChainSpec schedule"
                            .into(),
                    ),
                ));
            }
            inputs.push(SystemTxInputV2::TeeBootstrap { payload });
        } else if pending_tee_bootstrap.is_some() {
            return Err(BlockExecutionError::Internal(
                alloy_evm::block::InternalBlockExecutionError::Other(
                    format!("OST3 bootstrap payload is forbidden at block {block_number}").into(),
                ),
            ));
        }
        if block_number >= 1 {
            inputs.push(SystemTxInputV2::OracleSlashWindow);
        }
        if block_number >= 1 {
            inputs.push(SystemTxInputV2::HookEvents);
        }

        let encoded_inputs = inputs
            .into_iter()
            .map(|input| {
                let kind = input.kind();
                let calldata = input.encode().map_err(|error| {
                    BlockExecutionError::Internal(
                        alloy_evm::block::InternalBlockExecutionError::Other(
                            format!("encode system tx input: {error}").into(),
                        ),
                    )
                })?;
                Ok((kind, calldata))
            })
            .collect::<Result<Vec<_>, BlockExecutionError>>()?;
        let mut gas_plan_inputs = encoded_inputs.clone();
        if self.ocomp_lifecycle_active_at(block_number) {
            let terminal = SystemTxInputV2::OcompTerminalRequest;
            gas_plan_inputs.push((
                terminal.kind(),
                terminal.encode().map_err(|error| {
                    BlockExecutionError::Internal(
                        alloy_evm::block::InternalBlockExecutionError::Other(
                            format!("encode terminal system tx input: {error}").into(),
                        ),
                    )
                })?,
            ));
        }
        let gas_plan =
            SystemTxVisibleGasPlan::new(block_gas_limit, &gas_plan_inputs).map_err(|error| {
                BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                    format!("plan visible system tx gas: {error}").into(),
                ))
            })?;

        encoded_inputs
            .into_iter()
            .enumerate()
            .map(|(ordinal, (kind, calldata))| {
                let gas_limit = gas_plan.gas_limit(ordinal).ok_or_else(|| {
                    BlockExecutionError::Internal(
                        alloy_evm::block::InternalBlockExecutionError::Other(
                            format!("visible gas plan missing system tx ordinal {ordinal}").into(),
                        ),
                    )
                })?;

                // for body[0] (Phase 1
                // CertifiedParentAccounting on block_number >= 2) reuse the
                // prebuilt tx supplied by the payload builder. The defensive
                // calldata equality check guarantees the tx fed to pre-exec
                // and the tx going into body[0] are byte-identical even if
                // the calldata derivation ever diverges.
                if ordinal == 0
                    && matches!(kind, crate::system_tx::SystemTxKind::CertifiedParentAccounting)
                {
                    if let Some(prebuilt) = prebuilt_phase1_tx.as_ref() {
                        if prebuilt.tx().input() != &calldata {
                            return Err(BlockExecutionError::Internal(
                                alloy_evm::block::InternalBlockExecutionError::Other(
                                    "prebuilt Phase 1 tx calldata diverges from re-derived input"
                                        .into(),
                                ),
                            ));
                        }
                        if Address::from(*prebuilt.signer()) != proposer {
                            return Err(BlockExecutionError::Internal(
                                alloy_evm::block::InternalBlockExecutionError::Other(
                                    format!(
                                        "prebuilt Phase 1 signer {} does not match proposer {proposer}",
                                        Address::from(*prebuilt.signer())
                                    )
                                    .into(),
                                ),
                            ));
                        }
                        return Ok(prebuilt.clone());
                    }
                }

                let unsigned = build_unsigned_system_tx_with_gas_limit(
                    kind,
                    ordinal.try_into().map_err(|_| {
                        BlockExecutionError::Internal(
                            alloy_evm::block::InternalBlockExecutionError::Other(
                                format!("system tx ordinal {ordinal} exceeds u8 range").into(),
                            ),
                        )
                    })?,
                    block_number,
                    chain_id,
                    calldata,
                    gas_limit,
                )
                .map_err(|error| {
                    BlockExecutionError::Internal(
                        alloy_evm::block::InternalBlockExecutionError::Other(
                            format!("build unsigned system tx: {error}").into(),
                        ),
                    )
                })?;
                let signed = signer.sign_unsigned(unsigned).map_err(|error| {
                    BlockExecutionError::Internal(
                        alloy_evm::block::InternalBlockExecutionError::Other(
                            format!("sign system tx: {error}").into(),
                        ),
                    )
                })?;
                Ok(Recovered::new_unchecked(signed, proposer))
            })
            .collect()
    }

    pub fn build_end_system_txs(
        &self,
        block_number: u64,
        chain_id: u64,
        begin_system_tx_count: usize,
        proposer_evm_address: Option<Address>,
    ) -> Result<Vec<Recovered<TransactionSigned>>, BlockExecutionError> {
        if !self.ocomp_lifecycle_active_at(block_number) {
            return Ok(Vec::new());
        }

        let signer = self.evm_signer.as_ref().ok_or_else(|| {
            BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                "missing EVM signer for proposer terminal system tx".into(),
            ))
        })?;
        let proposer = proposer_evm_address.unwrap_or_else(|| signer.address());
        if signer.address() != proposer {
            return Err(BlockExecutionError::Internal(
                alloy_evm::block::InternalBlockExecutionError::Other(
                    format!(
                        "configured EVM signer {} does not match proposer {proposer}",
                        signer.address()
                    )
                    .into(),
                ),
            ));
        }

        let input = SystemTxInputV2::OcompTerminalRequest;
        let calldata = input.encode().map_err(|error| {
            BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                format!("encode terminal system tx input: {error}").into(),
            ))
        })?;
        let ordinal = begin_system_tx_count.try_into().map_err(|_| {
            BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                format!("terminal system tx ordinal {begin_system_tx_count} exceeds u8 range")
                    .into(),
            ))
        })?;
        let unsigned = build_unsigned_system_tx(
            SystemTxKind::OcompTerminalRequest,
            ordinal,
            block_number,
            chain_id,
            calldata,
        )
        .map_err(|error| {
            BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                format!("build terminal system tx: {error}").into(),
            ))
        })?;
        let signed = signer.sign_unsigned(unsigned).map_err(|error| {
            BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                format!("sign terminal system tx: {error}").into(),
            ))
        })?;
        Ok(vec![Recovered::new_unchecked(signed, proposer)])
    }

    /// build and sign a single Phase 1 (`CertifiedParentAccounting`)
    /// body[0] tx for the next block in proposer mode. Returns `None` for
    /// `block_number <= OutbeProtocolSchedule.genesis_bootstrap_block_number`
    /// (genesis bootstrap skips Phase 1 entirely).
    ///
    /// The returned `Recovered<TransactionSigned>` is the canonical witness:
    /// the payload builder caches it in [`OutbeBlockExecutionCtx::prebuilt_phase1_tx`]
    /// for the executor's pre-exec Phase 1 commit, and reuses it byte-for-byte
    /// in the main `build_begin_system_txs` call so the body[0] tx and the
    /// pre-exec witness share the same `signature_hash`.
    pub fn build_signed_phase1_tx(
        &self,
        block_number: u64,
        chain_id: u64,
        parent_hash: B256,
        parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
        proposer_evm_address: Option<Address>,
    ) -> Result<Option<Recovered<TransactionSigned>>, BlockExecutionError> {
        use outbe_primitives::protocol_schedule::OutbeProtocolSchedule;

        // / gate on the protocol-schedule field rather than a
        // magic literal `1`. The schedule is the single source of truth.
        let schedule = OutbeProtocolSchedule::default();
        if block_number <= schedule.genesis_bootstrap_block_number {
            return Ok(None);
        }

        let signer = self.evm_signer.as_ref().ok_or_else(|| {
            BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                "missing EVM signer for proposer Phase 1 prebuild".into(),
            ))
        })?;
        let proposer = proposer_evm_address.unwrap_or_else(|| signer.address());
        if signer.address() != proposer {
            return Err(BlockExecutionError::Internal(
                alloy_evm::block::InternalBlockExecutionError::Other(
                    format!(
                        "configured EVM signer {} does not match proposer {proposer}",
                        signer.address()
                    )
                    .into(),
                ),
            ));
        }

        let metadata = parent_consensus_metadata.ok_or_else(|| {
            BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                "missing parent consensus metadata for Phase 1 prebuild".into(),
            ))
        })?;
        if metadata.finalized_block_hash != parent_hash {
            return Err(BlockExecutionError::Internal(
                alloy_evm::block::InternalBlockExecutionError::Other(
                    format!(
                        "Phase 1 prebuild: metadata hash must match block parent: expected {parent_hash}, got {}",
                        metadata.finalized_block_hash
                    )
                    .into(),
                ),
            ));
        }

        let input = SystemTxInputV2::CertifiedParentAccounting { metadata };
        let kind = input.kind();
        let calldata = input.encode().map_err(|error| {
            BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                format!("Phase 1 prebuild: encode SystemTxInputV2: {error}").into(),
            ))
        })?;
        let unsigned = build_unsigned_system_tx(kind, 0, block_number, chain_id, calldata)
            .map_err(|error| {
                BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                    format!("Phase 1 prebuild: build unsigned tx: {error}").into(),
                ))
            })?;
        let signed = signer.sign_unsigned(unsigned).map_err(|error| {
            BlockExecutionError::Internal(alloy_evm::block::InternalBlockExecutionError::Other(
                format!("Phase 1 prebuild: sign tx: {error}").into(),
            ))
        })?;
        Ok(Some(Recovered::new_unchecked(signed, proposer)))
    }
}
