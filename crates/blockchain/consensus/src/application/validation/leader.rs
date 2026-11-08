//! Ordered layout, canonical envelope and leader signer validation.
use super::SystemTxLeaderValidationContext;
use crate::errors::BlockValidationError;
use alloy_consensus::{BlockHeader as _, SignableTransaction as _, Transaction as _};
use alloy_primitives::Address;
use commonware_cryptography::certificate::Scheme as _;
use commonware_utils::ordered::Quorum as _;
use outbe_primitives::system_tx::binding;
use outbe_primitives::{
    reshare_artifact::OutbeBlockArtifacts, system_tx::SystemTxLayout, OutbeBlockBody, OutbeHeader,
};
use reth_ethereum::primitives::SignedTransaction as _;

/// Resolve the EVM address of the consensus leader for `round`: map the
/// proposer's BLS key to its participant index, then to the ordered EVM
/// committee entry.
pub(super) fn consensus_leader_evm_address(
    context: &SystemTxLeaderValidationContext<'_>,
) -> Result<Address, BlockValidationError> {
    let SystemTxLeaderValidationContext {
        round,
        proposer,
        certificate_scheme_provider,
        committee_provider,
        ..
    } = context;
    let epoch = round.epoch();
    let scheme = certificate_scheme_provider
        .scoped(epoch)
        .ok_or(BlockValidationError::MissingCertificateScheme(epoch))?;
    let participant = scheme
        .participants()
        .index(proposer)
        .ok_or(BlockValidationError::LeaderNotInParticipantSet(epoch))?;
    let index: usize = participant
        .get()
        .try_into()
        .map_err(|_| BlockValidationError::ParticipantIndexOverflow(participant.get()))?;
    let committee = committee_provider
        .ordered_committee(epoch)
        .ok_or(BlockValidationError::MissingOrderedCommittee(epoch))?;

    committee
        .get(index)
        .copied()
        .ok_or(BlockValidationError::MissingCommitteeParticipant { epoch, index })
}

pub(super) fn validate_gas_limit(header: &OutbeHeader) -> Result<(), BlockValidationError> {
    let expected_gas_limit = outbe_primitives::system_tx::protocol_block_gas_limit(header.number());
    if header.gas_limit() != expected_gas_limit {
        return Err(BlockValidationError::GasLimitMismatch {
            block_number: header.number(),
            expected: expected_gas_limit,
            actual: header.gas_limit(),
        });
    }
    Ok(())
}

pub(super) fn validate_parent_accounting(
    layout: &SystemTxLayout<'_>,
    header: &OutbeHeader,
) -> Result<(), BlockValidationError> {
    binding::validate_parent_accounting_binding(layout, header)
        .map_err(BlockValidationError::from_parent_accounting_binding)
}

pub(super) fn validate_boundary_outcome(
    layout: &SystemTxLayout<'_>,
    artifacts: &OutbeBlockArtifacts,
) -> Result<(), BlockValidationError> {
    binding::validate_boundary_outcome_binding(layout, artifacts)
        .map_err(BlockValidationError::from_boundary_binding)
}

pub(super) fn validate_envelopes(
    layout: &SystemTxLayout<'_>,
    header: &OutbeHeader,
    chain_id: u64,
) -> Result<(), BlockValidationError> {
    for (index, tx) in layout.begin.iter().enumerate() {
        let tx = *tx;
        let input = outbe_primitives::system_tx::SystemTx::decode(tx.input().as_ref())
            .map_err(BlockValidationError::DecodeSystemTxInput)?;
        let ordinal: u8 = index
            .try_into()
            .map_err(|_| BlockValidationError::OrdinalOverflow(index))?;
        let unsigned = outbe_primitives::system_tx::build_unsigned_system_tx(
            ordinal,
            header.number(),
            chain_id,
            input.encode()?,
        )
        .map_err(BlockValidationError::BuildUnsignedSystemTx)?;
        if tx.signature_hash() != unsigned.signature_hash() {
            return Err(BlockValidationError::SignatureHashMismatch {
                kind: input.name(),
                ordinal,
            });
        }
    }
    for tx in &layout.end {
        let tx = *tx;
        let input = outbe_primitives::system_tx::SystemTx::decode(tx.input().as_ref())
            .map_err(BlockValidationError::DecodeSystemTxInput)?;
        let ordinal = input
            .nonce_ordinal()
            .ok_or_else(|| BlockValidationError::OrdinalOverflow(usize::from(u8::MAX)))?;
        let unsigned = outbe_primitives::system_tx::build_unsigned_system_tx(
            ordinal,
            header.number(),
            chain_id,
            input.encode()?,
        )
        .map_err(BlockValidationError::BuildUnsignedSystemTx)?;
        if tx.signature_hash() != unsigned.signature_hash() {
            return Err(BlockValidationError::SignatureHashMismatch {
                kind: input.name(),
                ordinal,
            });
        }
    }

    Ok(())
}

pub(super) fn validate_signers(
    layout: &SystemTxLayout<'_>,
    expected: Address,
) -> Result<(), BlockValidationError> {
    for tx in layout.begin.iter().chain(layout.end.iter()) {
        let signer = tx
            .try_recover()
            .map_err(|error| BlockValidationError::RecoverSigner(format!("{error}")))?;
        if signer != expected {
            return Err(BlockValidationError::SignerMismatch { signer, expected });
        }
    }

    Ok(())
}

pub(super) fn validate_layout<'a>(
    body: &'a OutbeBlockBody,
    header: &OutbeHeader,
) -> Result<(SystemTxLayout<'a>, OutbeBlockArtifacts), BlockValidationError> {
    binding::validate_system_layout(body, header).map_err(BlockValidationError::from_layout_binding)
}

#[cfg(test)]
mod tests;
