//! Ordered layout, canonical envelope and leader signer validation.
use super::SystemTxLeaderValidationContext;
use alloy_consensus::{BlockHeader as _, SignableTransaction as _, Transaction as _};
use alloy_primitives::Address;
use commonware_cryptography::certificate::Scheme as _;
use commonware_utils::ordered::Quorum as _;
use outbe_primitives::system_tx::binding::{
    self, BoundaryBindingError, LayoutBindingError, ParentAccountingBindingError,
};
use outbe_primitives::{
    reshare_artifact::OutbeBlockArtifacts,
    system_tx::{OcompLifecycleActivation, SystemTxLayout},
    OutbeBlockBody, OutbeHeader,
};
use reth_ethereum::primitives::SignedTransaction as _;

/// Resolve the EVM address of the consensus leader for `round`: map the
/// proposer's BLS key to its participant index, then to the ordered EVM
/// committee entry.
pub(super) fn consensus_leader_evm_address(
    context: &SystemTxLeaderValidationContext<'_>,
) -> Result<Address, String> {
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
        .ok_or_else(|| format!("missing certificate scheme for epoch {epoch}"))?;
    let participant = scheme.participants().index(proposer).ok_or_else(|| {
        format!("consensus leader public key is not in epoch {epoch} participant set")
    })?;
    let index: usize = participant
        .get()
        .try_into()
        .map_err(|_| format!("participant index {} does not fit usize", participant.get()))?;
    let committee = committee_provider
        .ordered_committee(epoch)
        .ok_or_else(|| format!("missing ordered EVM committee for epoch {epoch}"))?;

    committee.get(index).copied().ok_or_else(|| {
        format!("ordered EVM committee for epoch {epoch} is missing participant index {index}")
    })
}

pub(super) fn validate_gas_limit(header: &OutbeHeader) -> Result<(), String> {
    let expected_gas_limit = outbe_primitives::system_tx::protocol_block_gas_limit(header.number());
    if header.gas_limit() != expected_gas_limit {
        return Err(format!(
            "protocol gas limit mismatch at block {}: expected {}, got {}",
            header.number(),
            expected_gas_limit,
            header.gas_limit()
        ));
    }
    Ok(())
}

pub(super) fn validate_parent_accounting(
    layout: &SystemTxLayout<'_>,
    header: &OutbeHeader,
) -> Result<(), String> {
    binding::validate_parent_accounting_binding(layout, header).map_err(|error| match error {
        ParentAccountingBindingError::Decode(source) => {
            format!("decode CertifiedParentAccounting system tx input: {source}")
        }
        error => error.to_string(),
    })
}

pub(super) fn validate_boundary_outcome(
    layout: &SystemTxLayout<'_>,
    artifacts: &OutbeBlockArtifacts,
) -> Result<(), String> {
    binding::validate_boundary_outcome_binding(layout, artifacts).map_err(|error| match error {
        BoundaryBindingError::Decode(source) => {
            format!("decode system transaction input: {source}")
        }
        error => error.to_string(),
    })
}

pub(super) fn validate_envelopes(
    layout: &SystemTxLayout<'_>,
    header: &OutbeHeader,
    chain_id: u64,
) -> Result<(), String> {
    let mut canonical_inputs = Vec::with_capacity(layout.system_tx_count());
    for tx in layout.begin.iter().chain(layout.end.iter()) {
        let tx = *tx;
        let input = outbe_primitives::system_tx::SystemTxInputV2::decode(tx.input().as_ref())
            .map_err(|error| format!("decode system transaction input: {error}"))?;
        let kind = input.kind();
        let calldata = input.encode().map_err(|error| error.to_string())?;
        canonical_inputs.push((kind, calldata));
    }
    let gas_plan = outbe_primitives::system_tx::SystemTxVisibleGasPlan::new(
        header.gas_limit(),
        &canonical_inputs,
    )
    .map_err(|error| format!("plan visible system tx gas: {error}"))?;

    for (ordinal, (tx, (kind, calldata))) in layout
        .begin
        .iter()
        .chain(layout.end.iter())
        .zip(canonical_inputs)
        .enumerate()
    {
        let tx = *tx;
        let ordinal: u8 = ordinal
            .try_into()
            .map_err(|_| format!("system tx ordinal {ordinal} exceeds u8 range"))?;
        let unsigned = outbe_primitives::system_tx::build_unsigned_system_tx_with_gas_limit(
            kind,
            ordinal,
            header.number(),
            chain_id,
            calldata,
            gas_plan
                .gas_limit(usize::from(ordinal))
                .ok_or_else(|| format!("visible gas plan missing system tx ordinal {ordinal}"))?,
        )
        .map_err(|error| format!("build unsigned system transaction: {error}"))?;
        if tx.signature_hash() != unsigned.signature_hash() {
            return Err(format!(
                "system tx signature_hash mismatch for {:?} at ordinal {}",
                kind, ordinal
            ));
        }
    }

    Ok(())
}

pub(super) fn validate_signers(
    layout: &SystemTxLayout<'_>,
    expected: Address,
) -> Result<(), String> {
    for tx in layout.begin.iter().chain(layout.end.iter()) {
        let signer = tx
            .try_recover()
            .map_err(|error| format!("recover system tx signer for leader binding: {error}"))?;
        if signer != expected {
            return Err(format!(
                "system tx signer {signer} does not match consensus leader EVM address {expected}"
            ));
        }
    }

    Ok(())
}

pub(super) fn validate_layout<'a>(
    body: &'a OutbeBlockBody,
    header: &OutbeHeader,
    activation: OcompLifecycleActivation,
) -> Result<(SystemTxLayout<'a>, OutbeBlockArtifacts), String> {
    binding::validate_system_layout(body, header, activation).map_err(|error| match error {
        LayoutBindingError::Artifacts(source) => {
            format!("decode Outbe block artifacts for system tx validation: {source}")
        }
        LayoutBindingError::Layout(source) => {
            format!("invalid system tx layout for leader binding: {source}")
        }
        error => error.to_string(),
    })
}

#[cfg(test)]
mod tests;
