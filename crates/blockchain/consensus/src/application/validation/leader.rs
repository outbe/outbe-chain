//! Ordered layout, canonical envelope and leader signer validation.
use super::SystemTxLeaderValidationContext;
use alloy_consensus::{BlockHeader as _, SignableTransaction as _, Transaction as _};
use alloy_primitives::Address;
use commonware_cryptography::certificate::Scheme as _;
use commonware_utils::ordered::Quorum as _;
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
    if header.number() >= 2 {
        let finalization_tx = *layout
            .begin
            .first()
            .ok_or_else(|| "missing CertifiedParentAccounting system tx".to_string())?;
        let input =
            outbe_primitives::system_tx::SystemTxInputV2::decode(finalization_tx.input().as_ref())
                .map_err(|error| {
                    format!("decode CertifiedParentAccounting system tx input: {error}")
                })?;
        let outbe_primitives::system_tx::SystemTxInputV2::CertifiedParentAccounting { metadata } =
            input
        else {
            return Err("expected CertifiedParentAccounting system tx at begin ordinal 0".into());
        };
        if metadata.finalized_block_hash != header.parent_hash() {
            return Err(format!(
                "CertifiedParentAccounting metadata hash must match block parent: expected {}, got {}",
                header.parent_hash(),
                metadata.finalized_block_hash
            ));
        }
    }

    Ok(())
}

pub(super) fn validate_boundary_outcome(
    layout: &SystemTxLayout<'_>,
    artifacts: &OutbeBlockArtifacts,
) -> Result<(), String> {
    let Some(outbe_primitives::reshare_artifact::ConsensusHeaderArtifact::BoundaryOutcome(
        header_artifact,
    )) = artifacts.consensus_header_artifact.as_ref()
    else {
        return Ok(());
    };
    let mut found = false;
    for tx in layout.begin.iter().chain(layout.end.iter()) {
        let tx = *tx;
        let input = outbe_primitives::system_tx::SystemTxInputV2::decode(tx.input().as_ref())
            .map_err(|error| format!("decode system transaction input: {error}"))?;
        if let outbe_primitives::system_tx::SystemTxInputV2::BoundaryOutcome { artifact } = input {
            if &artifact != header_artifact {
                return Err("BoundaryOutcome system tx artifact mismatch".into());
            }
            found = true;
        }
    }
    if !found {
        return Err("missing BoundaryOutcome system tx for header artifact".into());
    }
    Ok(())
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
    let artifacts = outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(
        header.extra_data().as_ref(),
    )
    .map_err(|error| format!("decode Outbe block artifacts for system tx validation: {error}"))?;

    let layout = outbe_primitives::system_tx::split_system_layout(&body.transactions)
        .map_err(|error| format!("invalid system tx layout for leader binding: {error}"))?;
    let has_boundary_outcome = matches!(
        &artifacts.consensus_header_artifact,
        Some(outbe_primitives::reshare_artifact::ConsensusHeaderArtifact::BoundaryOutcome(_))
    );
    let has_tee_bootstrap =
        layout.has_begin_kind(outbe_primitives::system_tx::SystemTxKind::TeeBootstrap);
    outbe_primitives::system_tx::validate_system_tx_set_for_activation(
        &layout,
        header.number(),
        has_boundary_outcome,
        has_tee_bootstrap,
        activation,
    )
    .map_err(|error| format!("invalid system tx set: {error}"))?;

    Ok((layout, artifacts))
}
