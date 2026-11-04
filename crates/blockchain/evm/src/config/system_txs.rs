use crate::system_tx::build_unsigned_system_tx;
use crate::system_tx::build_unsigned_system_tx_with_gas_limit;
use crate::system_tx::split_system_layout;
use crate::system_tx::validate_system_tx_set_for_publication;
use crate::system_tx::SystemTx;
use crate::system_tx::SystemTxVisibleGasPlan;

use alloy_consensus::Transaction as _;
use alloy_evm::RecoveredTx;

use alloy_primitives::{Address, Bytes, B256};

use outbe_primitives::{
    block::BlockHeightRegime,
    consensus_metadata::CertifiedParentAccountingMetadata,
    reshare_artifact::{
        decode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
    },
    OutbeBlock,
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
    block_gas_limit: u64,
    prebuilt_phase1_tx: Option<Recovered<TransactionSigned>>,
}

pub(super) fn system_tx_expectations_for_block(
    block: &SealedBlock<OutbeBlock>,
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

    let has_tee_bootstrap = layout.has_begin_selector(crate::system_tx::TEE_BOOTSTRAP_SELECTOR);
    if let Err(error) = validate_system_tx_set_for_publication(
        &layout,
        block.header().number(),
        has_boundary_outcome,
        has_tee_bootstrap,
        has_publication,
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
        block_gas_limit: u64,
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
                block_gas_limit,
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
        let mut inputs = Vec::new();
        if regime.has_parent_finalization_accounting() {
            let metadata = parent_consensus_metadata
                .ok_or(SystemTxBuildError::MissingParentConsensusMetadata)?;
            if metadata.finalized_block_hash != parent_hash {
                return Err(SystemTxBuildError::CertifiedParentAccountingHashMismatch {
                    expected: parent_hash,
                    actual: metadata.finalized_block_hash,
                }
                .into());
            }
            inputs.push(SystemTx::CertifiedParentAccounting { metadata });
            inputs.push(SystemTx::LateFinalizeCredits {
                artifact: artifacts.late_finalize_credits.unwrap_or_default(),
            });
        }
        inputs.push(SystemTx::CycleTick);
        if let Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)) =
            artifacts.consensus_header_artifact
        {
            inputs.push(SystemTx::BoundaryOutcome { artifact });
        }
        if regime.requires_genesis_bootstrap_txs() {
            let payload = pending_tee_bootstrap
                .ok_or(SystemTxBuildError::MissingBlock1Ost3BootstrapPayload)?;
            crate::tee_bootstrap::validate_tee_bootstrap_for_build(
                block_number,
                &payload,
                &self.tee_attestation_v1,
            )?;
            inputs.push(SystemTx::TeeBootstrap { payload });
        } else if pending_tee_bootstrap.is_some() {
            return Err(SystemTxBuildError::Ost3BootstrapForbiddenAtBlock { block_number }.into());
        }
        inputs.push(SystemTx::OracleSlashWindow);
        inputs.push(SystemTx::HookEvents);
        if let Some(batch) = artifacts.radicle_publication {
            inputs.push(SystemTx::RadiclePublication { batch });
        }
        Ok(inputs)
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
        if !BlockHeightRegime::for_block(block_number).has_parent_finalization_accounting() {
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

        let metadata =
            parent_consensus_metadata.ok_or(SystemTxBuildError::MissingPhase1PrebuildMetadata)?;
        if metadata.finalized_block_hash != parent_hash {
            return Err(SystemTxBuildError::Phase1PrebuildMetadataHashMismatch {
                expected: parent_hash,
                actual: metadata.finalized_block_hash,
            }
            .into());
        }

        let input = SystemTx::CertifiedParentAccounting { metadata };
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
    let gas_plan = SystemTxVisibleGasPlan::new(ctx.block_gas_limit, &encoded_inputs)
        .map_err(SystemTxBuildError::plan_visible_system_tx_gas)?;

    encoded_inputs
        .into_iter()
        .enumerate()
        .map(|(ordinal, calldata)| sign_single_begin_system_tx(&ctx, &gas_plan, ordinal, calldata))
        .collect()
}

fn sign_single_begin_system_tx(
    ctx: &ProposerSignContext<'_>,
    gas_plan: &SystemTxVisibleGasPlan,
    ordinal: usize,
    calldata: Bytes,
) -> Result<Recovered<TransactionSigned>, BlockExecutionError> {
    let gas_limit = gas_plan
        .gas_limit(ordinal)
        .ok_or(SystemTxBuildError::VisibleGasPlanMissingOrdinal { ordinal })?;

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

    let unsigned = build_unsigned_system_tx_with_gas_limit(
        outbe_primitives::system_tx::SystemTxEnvelopeInput {
            ordinal: ordinal
                .try_into()
                .map_err(|_| SystemTxBuildError::SystemTxOrdinalExceedsU8 { ordinal })?,
            block_number: ctx.block_number,
            chain_id: ctx.chain_id,
            calldata,
            gas_limit,
        },
    )
    .map_err(SystemTxBuildError::build_unsigned_system_tx)?;
    let signed = ctx
        .signer
        .sign_unsigned(unsigned)
        .map_err(SystemTxBuildError::sign_system_tx)?;
    Ok(Recovered::new_unchecked(signed, ctx.proposer))
}
