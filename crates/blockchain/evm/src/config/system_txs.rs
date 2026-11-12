use crate::system_tx::build_unsigned_system_tx;
use crate::system_tx::split_system_layout;
use crate::system_tx::validate_system_tx_set_for_publication;
use crate::system_tx::SystemTx;

use alloy_consensus::Transaction as _;
use alloy_evm::RecoveredTx;

use alloy_primitives::{Address, Bytes, B256};

use {
    crate::system_tx::{assemble_prefix_system_txs, BlockOrder},
    outbe_primitives::{
        block::{BlockHeightRegime, BlockSystemFlags},
        consensus_metadata::CertifiedParentAccountingMetadata,
        reshare_artifact::{
            decode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
        },
        system_tx::CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
        system_tx_context::SystemTxBuildContext,
        OutbeBlock,
    },
};

use reth_ethereum::TransactionSigned;

use reth_evm::execute::BlockExecutionError;

use reth_primitives_traits::{
    AlloyBlockHeader as _, Recovered, SealedBlock, SignedTransaction as _,
};

use super::OutbeEvmConfig;
use crate::errors::{ExecutorValidationError, SystemTxBuildError, SystemTxExpectationError};
use crate::SharedOutbeEvmSigner;

type SystemTxExpectations = (
    Vec<Recovered<TransactionSigned>>,
    Option<SystemTxExpectationError>,
    Option<Address>,
);

struct ProposerSignContext<'a> {
    signer: &'a SharedOutbeEvmSigner,
    proposer: Address,
    regime: BlockHeightRegime,
    block_number: u64,
    chain_id: u64,
    prebuilt_phase1_tx: Option<Recovered<TransactionSigned>>,
}

pub(super) fn system_tx_expectations_for_block(
    block: &SealedBlock<OutbeBlock>,
    order: &BlockOrder,
) -> SystemTxExpectations {
    let (has_boundary_outcome, has_publication) =
        match decode_outbe_block_artifacts(block.header().extra_data().as_ref()) {
            Ok(artifacts) => (
                matches!(
                    artifacts.consensus_header_artifact,
                    Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
                ),
                artifacts.radicle_publication.is_some(),
            ),
            Err(error) => {
                return (
                    Vec::new(),
                    Some(SystemTxExpectationError::decode_block_artifacts(error)),
                    None,
                );
            }
        };

    let layout = match split_system_layout(&block.body().transactions) {
        Ok(layout) => layout,
        Err(error) => {
            return (
                Vec::new(),
                Some(SystemTxExpectationError::layout(error)),
                None,
            )
        }
    };

    if let Err(error) = validate_system_tx_set_for_publication(
        order,
        &layout,
        block.header().number(),
        BlockSystemFlags {
            has_boundary_outcome,
            has_publication,
        },
    ) {
        return (
            Vec::new(),
            Some(SystemTxExpectationError::layout(error)),
            None,
        );
    }

    let recover =
        |tx: &TransactionSigned| -> Result<Recovered<TransactionSigned>, SystemTxExpectationError> {
            let signer = tx
                .try_recover()
                .map_err(SystemTxExpectationError::recover_signer)?;
            Ok(Recovered::new_unchecked(tx.clone(), signer))
        };

    let mut begin = Vec::with_capacity(layout.begin.len());
    for tx in layout.begin {
        match recover(tx) {
            Ok(recovered) => begin.push(recovered),
            Err(error) => return (Vec::new(), Some(error), None),
        }
    }

    let proposer = begin.first().map(|tx| Address::from(*tx.signer()));
    (begin, None, proposer)
}

impl OutbeEvmConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn build_begin_system_txs(
        &self,
        block_number: u64,
        chain_id: u64,
        _block_gas_limit: u64,
        parent_hash: B256,
        extra_data: &Bytes,
        parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
        proposer_evm_address: Option<Address>,
        prebuilt_phase1_tx: Option<Recovered<TransactionSigned>>,
        pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
    ) -> Result<Vec<Recovered<TransactionSigned>>, BlockExecutionError> {
        let regime = BlockHeightRegime::for_block(block_number);
        if regime.is_genesis() {
            if pending_tee_bootstrap.is_some() {
                return Err(SystemTxBuildError::Ost3BootstrapInvalidAtGenesis.into());
            }
            return Ok(Vec::new());
        }

        let (signer, proposer) = self.resolve_proposer_system_tx_signer(proposer_evm_address)?;
        let artifacts = decode_outbe_block_artifacts(extra_data.as_ref())
            .map_err(ExecutorValidationError::block_artifacts)?;
        let inputs = self.assemble_proposer_begin_inputs(
            regime,
            block_number,
            parent_hash,
            artifacts,
            parent_consensus_metadata,
            pending_tee_bootstrap,
        )?;
        sign_planned_begin_system_txs(
            inputs,
            ProposerSignContext {
                signer,
                proposer,
                regime,
                block_number,
                chain_id,
                prebuilt_phase1_tx,
            },
        )
    }

    fn resolve_proposer_system_tx_signer(
        &self,
        proposer_evm_address: Option<Address>,
    ) -> Result<(&SharedOutbeEvmSigner, Address), BlockExecutionError> {
        let signer = self
            .evm_signer
            .as_ref()
            .ok_or(SystemTxBuildError::MissingProposerSystemTxSigner)?;
        let proposer = proposer_evm_address.unwrap_or_else(|| signer.address());
        if signer.address() != proposer {
            return Err(SystemTxBuildError::ConfiguredSignerMismatch {
                signer: signer.address(),
                proposer,
            }
            .into());
        }
        Ok((signer, proposer))
    }

    fn assemble_proposer_begin_inputs(
        &self,
        regime: BlockHeightRegime,
        block_number: u64,
        parent_hash: B256,
        artifacts: OutbeBlockArtifacts,
        parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
        pending_tee_bootstrap: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
    ) -> Result<Vec<SystemTx>, BlockExecutionError> {
        if regime.is_genesis() {
            if pending_tee_bootstrap.is_some() {
                return Err(SystemTxBuildError::Ost3BootstrapInvalidAtGenesis.into());
            }
            return Ok(Vec::new());
        }
        if !regime.requires_genesis_bootstrap_txs() && pending_tee_bootstrap.is_some() {
            return Err(SystemTxBuildError::Ost3BootstrapForbiddenAtBlock { block_number }.into());
        }
        let expected_tee_policy = if regime.requires_genesis_bootstrap_txs() {
            let activation = self
                .tee_attestation_v1
                .activation()
                .map_err(SystemTxBuildError::invalid_mandatory_tee_chain_spec_authority)?;
            Some(
                activation
                    .policy_at(block_number)
                    .map_err(SystemTxBuildError::from)?,
            )
        } else {
            None
        };
        let build_ctx = SystemTxBuildContext {
            regime,
            block_number,
            parent_hash,
            artifacts: &artifacts,
            parent_consensus_metadata: parent_consensus_metadata.as_ref(),
            pending_tee_bootstrap: pending_tee_bootstrap.as_ref(),
            expected_tee_policy,
        };
        assemble_prefix_system_txs(&self.runtime_host.block_order(), &build_ctx)
            .map_err(SystemTxBuildError::from)
            .map_err(Into::into)
    }

    /// build and sign a single Phase 1 (`CertifiedParentAccounting`)
    /// body[0] tx for the next block in proposer mode. Returns `None` for
    /// `block_number <= OutbeProtocolSchedule.genesis_bootstrap_block_number`
    /// (genesis bootstrap skips Phase 1 entirely).
    ///
    /// The returned `Recovered<TransactionSigned>` is the canonical witness.
    /// The payload builder caches it in [`OutbeBlockExecutionCtx::prebuilt_phase1_tx`]
    /// for the executor's pre-exec Phase 1 commit. The payload builder also
    /// reuses it byte-for-byte in the main `build_begin_system_txs` call, so the
    /// body[0] tx and the pre-exec witness share the same `signature_hash`.
    pub fn build_signed_phase1_tx(
        &self,
        block_number: u64,
        chain_id: u64,
        parent_hash: B256,
        parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
        proposer_evm_address: Option<Address>,
    ) -> Result<Option<Recovered<TransactionSigned>>, BlockExecutionError> {
        let regime = BlockHeightRegime::for_block(block_number);
        if !regime.has_parent_finalization_accounting() {
            return Ok(None);
        }

        let signer = self
            .evm_signer
            .as_ref()
            .ok_or(SystemTxBuildError::MissingPhase1PrebuildSigner)?;
        let proposer = proposer_evm_address.unwrap_or_else(|| signer.address());
        if signer.address() != proposer {
            return Err(SystemTxBuildError::ConfiguredSignerMismatch {
                signer: signer.address(),
                proposer,
            }
            .into());
        }

        let block_order = self.runtime_host.block_order();
        let step = block_order
            .consensus
            .get(usize::from(CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX))
            .ok_or(SystemTxBuildError::MissingPhase1PrebuildMetadata)?;
        let empty_artifacts = OutbeBlockArtifacts::default();
        let build_ctx = SystemTxBuildContext {
            regime,
            block_number,
            parent_hash,
            artifacts: &empty_artifacts,
            parent_consensus_metadata: parent_consensus_metadata.as_ref(),
            pending_tee_bootstrap: None,
            expected_tee_policy: None,
        };
        let payload = (step.build_payload)(&build_ctx).map_err(|error| match error {
            outbe_primitives::system_tx_context::SystemTxPayloadBuildError::MissingParentConsensusMetadata { .. } => {
                SystemTxBuildError::MissingPhase1PrebuildMetadata
            }
            outbe_primitives::system_tx_context::SystemTxPayloadBuildError::CertifiedParentAccountingHashMismatch {
                parent_hash,
                metadata_hash,
            } => SystemTxBuildError::Phase1PrebuildMetadataHashMismatch {
                expected: parent_hash,
                actual: metadata_hash,
            },
            other => SystemTxBuildError::from(other),
        })?;

        let input = SystemTx::Consensus {
            step_index: CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
            payload,
        };
        let calldata = input
            .encode()
            .map_err(SystemTxBuildError::phase1_prebuild_encode_input)?;
        let unsigned = build_unsigned_system_tx(0, block_number, chain_id, calldata)
            .map_err(SystemTxBuildError::phase1_prebuild_build_unsigned_tx)?;
        let signed = signer
            .sign_unsigned(unsigned)
            .map_err(SystemTxBuildError::phase1_prebuild_sign_tx)?;
        Ok(Some(Recovered::new_unchecked(signed, proposer)))
    }

    pub fn build_end_system_tx(
        &self,
        block_number: u64,
        chain_id: u64,
        proposer: Address,
        input: SystemTx,
    ) -> Result<Recovered<TransactionSigned>, BlockExecutionError> {
        sign_end_system_tx(
            self.evm_signer.as_ref(),
            proposer,
            block_number,
            chain_id,
            input,
            &self.runtime_host.block_order(),
        )
    }
}

pub(crate) fn sign_end_system_tx(
    evm_signer: Option<&SharedOutbeEvmSigner>,
    proposer: Address,
    block_number: u64,
    chain_id: u64,
    input: SystemTx,
    order: &BlockOrder,
) -> Result<Recovered<TransactionSigned>, BlockExecutionError> {
    let evm_signer = evm_signer.ok_or(ExecutorValidationError::MissingBeginZoneProposerAddress)?;
    if evm_signer.address() != proposer {
        return Err(ExecutorValidationError::SystemTxSignerMismatch {
            body_index: usize::from(input.nonce_ordinal().unwrap_or(0)),
            expected_phase: order.step_name_or_stage(&input),
            proposer,
            signer: evm_signer.address(),
        }
        .into());
    }
    let ordinal = input
        .nonce_ordinal()
        .ok_or(ExecutorValidationError::MissingBeginZoneProposerAddress)?;
    let calldata = input
        .encode()
        .map_err(ExecutorValidationError::encode_system_tx_for_visible_gas_plan)?;
    let unsigned = build_unsigned_system_tx(ordinal, block_number, chain_id, calldata)
        .map_err(ExecutorValidationError::plan_visible_system_tx_gas)?;
    let signed = evm_signer.sign_unsigned(unsigned).map_err(|error| {
        BlockExecutionError::other(ExecutorValidationError::plan_visible_system_tx_gas(error))
    })?;
    Ok(Recovered::new_unchecked(signed, proposer))
}

fn sign_planned_begin_system_txs(
    inputs: Vec<SystemTx>,
    ctx: ProposerSignContext<'_>,
) -> Result<Vec<Recovered<TransactionSigned>>, BlockExecutionError> {
    let encoded_inputs = inputs
        .into_iter()
        .map(|input| {
            input.encode().map_err(|error| {
                BlockExecutionError::from(SystemTxBuildError::encode_system_tx_input(error))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    encoded_inputs
        .into_iter()
        .enumerate()
        .map(|(ordinal, calldata)| sign_single_begin_system_tx(&ctx, ordinal, calldata))
        .collect()
}

fn sign_single_begin_system_tx(
    ctx: &ProposerSignContext<'_>,
    ordinal: usize,
    calldata: Bytes,
) -> Result<Recovered<TransactionSigned>, BlockExecutionError> {
    if ordinal == 0 && ctx.regime.has_parent_finalization_accounting() {
        if let Some(prebuilt) = ctx.prebuilt_phase1_tx.as_ref() {
            if prebuilt.tx().input() != &calldata {
                return Err(SystemTxBuildError::PrebuiltPhase1CalldataDiverges.into());
            }
            if Address::from(*prebuilt.signer()) != ctx.proposer {
                return Err(SystemTxBuildError::PrebuiltPhase1SignerMismatch {
                    signer: Address::from(*prebuilt.signer()),
                    proposer: ctx.proposer,
                }
                .into());
            }
            return Ok(prebuilt.clone());
        }
    }

    let ordinal_u8 = ordinal
        .try_into()
        .map_err(|_| SystemTxBuildError::SystemTxOrdinalExceedsU8 { ordinal })?;
    let unsigned = build_unsigned_system_tx(ordinal_u8, ctx.block_number, ctx.chain_id, calldata)
        .map_err(SystemTxBuildError::build_unsigned_system_tx)?;
    let signed = ctx
        .signer
        .sign_unsigned(unsigned)
        .map_err(SystemTxBuildError::sign_system_tx)?;
    Ok(Recovered::new_unchecked(signed, ctx.proposer))
}
