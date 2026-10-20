use crate::system_tx::build_unsigned_system_tx;
use crate::system_tx::build_unsigned_system_tx_with_gas_limit;
use crate::system_tx::split_system_layout;
use crate::system_tx::validate_system_tx_set_for_publication;
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
use crate::errors::{ExecutorValidationError, SystemTxBuildError, SystemTxExpectationError};

type SystemTxExpectations = (
    Vec<Recovered<TransactionSigned>>,
    Option<SystemTxExpectationError>,
    Option<Address>,
);

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

    let has_tee_bootstrap = layout.has_begin_kind(SystemTxKind::TeeBootstrap);
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
        if block_number == 0 {
            if pending_tee_bootstrap.is_some() {
                return Err(SystemTxBuildError::Ost3BootstrapInvalidAtGenesis.into());
            }
            return Ok(Vec::new());
        }

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

        let artifacts = decode_outbe_block_artifacts(extra_data.as_ref())
            .map_err(ExecutorValidationError::block_artifacts)?;
        let mut inputs = Vec::new();
        if block_number >= 2 {
            let metadata = parent_consensus_metadata
                .ok_or(SystemTxBuildError::MissingParentConsensusMetadata)?;
            if metadata.finalized_block_hash != parent_hash {
                return Err(SystemTxBuildError::CertifiedParentAccountingHashMismatch {
                    expected: parent_hash,
                    actual: metadata.finalized_block_hash,
                }
                .into());
            }
            inputs.push(SystemTxInputV2::CertifiedParentAccounting { metadata });
        }
        if block_number >= 2 {
            // Mandatory inclusion-window phase after Phase 1. The
            // gathered credits ride in the header artifact (empty until Phase 7).
            // Executor parity re-derives the same input on the verifier path.
            inputs.push(SystemTxInputV2::LateFinalizeCredits {
                artifact: artifacts.late_finalize_credits.clone().unwrap_or_default(),
            });
        }
        if block_number >= 1 {
            inputs.push(SystemTxInputV2::CycleTick);
        }
        if let Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)) =
            artifacts.consensus_header_artifact
        {
            inputs.push(SystemTxInputV2::BoundaryOutcome { artifact });
        }
        // The greenfield network has exactly one bootstrap wire path: canonical
        // OST3 is mandatory at block 1 and forbidden at every other height.
        if block_number == 1 {
            let payload = pending_tee_bootstrap
                .ok_or(SystemTxBuildError::MissingBlock1Ost3BootstrapPayload)?;
            payload
                .preflight()
                .map_err(SystemTxBuildError::invalid_block1_ost3_bootstrap_payload)?;
            let activation = self
                .tee_attestation_v1
                .activation()
                .map_err(SystemTxBuildError::invalid_mandatory_tee_chain_spec_authority)?;
            let expected_policy = activation
                .policy_at(block_number)
                .map_err(SystemTxBuildError::from)?;
            if &payload.authority.policy != expected_policy {
                return Err(SystemTxBuildError::Block1Ost3PolicyScheduleMismatch.into());
            }
            inputs.push(SystemTxInputV2::TeeBootstrap { payload });
        } else if pending_tee_bootstrap.is_some() {
            return Err(SystemTxBuildError::Ost3BootstrapForbiddenAtBlock { block_number }.into());
        }
        if block_number >= 1 {
            inputs.push(SystemTxInputV2::OracleSlashWindow);
        }
        if block_number >= 1 {
            inputs.push(SystemTxInputV2::HookEvents);
        }
        if let Some(batch) = artifacts.radicle_publication {
            inputs.push(SystemTxInputV2::RadiclePublication { batch });
        }

        let encoded_inputs = inputs
            .into_iter()
            .map(|input| {
                let kind = input.kind();
                let calldata = input
                    .encode()
                    .map_err(SystemTxBuildError::encode_system_tx_input)?;
                Ok((kind, calldata))
            })
            .collect::<Result<Vec<_>, BlockExecutionError>>()?;
        let gas_plan = SystemTxVisibleGasPlan::new(block_gas_limit, &encoded_inputs)
            .map_err(SystemTxBuildError::plan_visible_system_tx_gas)?;

        encoded_inputs
            .into_iter()
            .enumerate()
            .map(|(ordinal, (kind, calldata))| {
                let gas_limit = gas_plan
                    .gas_limit(ordinal)
                    .ok_or(SystemTxBuildError::VisibleGasPlanMissingOrdinal { ordinal })?;

                // For body[0] (Phase 1
                // CertifiedParentAccounting on block_number >= 2), reuse the
                // prebuilt tx that the payload builder supplies. The defensive
                // calldata equality check guarantees that the tx fed to
                // pre-exec and the tx in body[0] are byte-identical. This
                // holds even if the calldata derivation ever diverges.
                if ordinal == 0
                    && matches!(
                        kind,
                        crate::system_tx::SystemTxKind::CertifiedParentAccounting
                    )
                {
                    if let Some(prebuilt) = prebuilt_phase1_tx.as_ref() {
                        if prebuilt.tx().input() != &calldata {
                            return Err(SystemTxBuildError::PrebuiltPhase1CalldataDiverges.into());
                        }
                        if Address::from(*prebuilt.signer()) != proposer {
                            return Err(SystemTxBuildError::PrebuiltPhase1SignerMismatch {
                                signer: Address::from(*prebuilt.signer()),
                                proposer,
                            }
                            .into());
                        }
                        return Ok(prebuilt.clone());
                    }
                }

                let unsigned = build_unsigned_system_tx_with_gas_limit(
                    outbe_primitives::system_tx::SystemTxEnvelopeInput {
                        kind,
                        ordinal: ordinal.try_into().map_err(|_| {
                            SystemTxBuildError::SystemTxOrdinalExceedsU8 { ordinal }
                        })?,
                        block_number,
                        chain_id,
                        calldata,
                        gas_limit,
                    },
                )
                .map_err(SystemTxBuildError::build_unsigned_system_tx)?;
                let signed = signer
                    .sign_unsigned(unsigned)
                    .map_err(SystemTxBuildError::sign_system_tx)?;
                Ok(Recovered::new_unchecked(signed, proposer))
            })
            .collect()
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
        use outbe_primitives::protocol_schedule::OutbeProtocolSchedule;

        // Gate on the protocol-schedule field rather than a
        // magic literal `1`. The schedule is the single source of truth.
        let schedule = OutbeProtocolSchedule::default();
        if block_number <= schedule.genesis_bootstrap_block_number {
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

        let input = SystemTxInputV2::CertifiedParentAccounting { metadata };
        let kind = input.kind();
        let calldata = input
            .encode()
            .map_err(SystemTxBuildError::phase1_prebuild_encode_input)?;
        let unsigned = build_unsigned_system_tx(kind, 0, block_number, chain_id, calldata)
            .map_err(SystemTxBuildError::phase1_prebuild_build_unsigned_tx)?;
        let signed = signer
            .sign_unsigned(unsigned)
            .map_err(SystemTxBuildError::phase1_prebuild_sign_tx)?;
        Ok(Some(Recovered::new_unchecked(signed, proposer)))
    }
}
