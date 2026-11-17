//! Transitional sidecar startup admission, isolated from fresh RAM initialization.
use super::*;

/// Require that the local enclave holds the exact offer key of the selected
/// upstream. Losing the key is terminal for this node identity.
pub(super) async fn require_upstream_offer_key_v1(upstream: &str) -> eyre::Result<()> {
    let expected_offer = outbe_consensus::read_upstream_network_encryption_public_key(upstream)
        .await
        .wrap_err("failed to read mandatory offer key from the selected upstream")?;
    if expected_offer.is_zero() {
        return Err(LaunchError::UpstreamMissingOst3OfferKey.into());
    }
    let resident_offer = outbe_tee::resident_network_encryption_public_key_v1()
        .wrap_err("failed to read the local enclave resident offer key")?;
    if resident_offer != expected_offer {
        return Err(LaunchError::LocalEnclaveOfferKeyMismatch.into());
    }
    info!(
        offer_public_key = %resident_offer,
        %upstream,
        "resident offer key matched upstream before execution launch"
    );
    Ok(())
}

/// Canonical block hash at `height` on the selected upstream.
async fn upstream_block_hash_v1(
    upstream: &str,
    height: u64,
) -> eyre::Result<Option<alloy_primitives::B256>> {
    outbe_consensus::follow_transport::UpstreamRpcClient::new(upstream)?
        .block_hash_by_number(height)
        .await
        .wrap_err("read upstream block at the durable TEE join anchor")
}

/// `Ready` local admission at the latest finalized block, with a resident
/// offer key equal to the on-chain key. Returns the admission for the
/// rejection report.
fn local_tee_admission_ready_v1(
    protocol: &dyn outbe_consensus::protocol_state::EngineProtocolState,
    node: &OutbeFullNode,
    (chain_id, genesis_hash): (u64, alloy_primitives::B256),
    identity: outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
) -> eyre::Result<(
    bool,
    Option<outbe_consensus::validators::LocalTeeRuntimeAdmissionV1>,
)> {
    let admission = crate::admission::read_finalized_local_tee_admission(
        protocol,
        &node.provider,
        chain_id,
        genesis_hash,
        identity,
    )?;
    if !matches!(
        admission,
        Some(outbe_consensus::validators::LocalTeeRuntimeAdmissionV1::Ready { .. })
    ) {
        return Ok((false, admission));
    }
    let on_chain_offer = outbe_consensus::validators::read_network_encryption_public_at_latest(
        protocol,
        &node.provider,
    )
    .wrap_err("read the canonical offer key for the TEE lease guard")?;
    let resident_offer = outbe_tee::resident_network_encryption_public_key_state_v1()
        .wrap_err("read the resident offer key for the TEE lease guard")?;
    Ok((
        !on_chain_offer.is_zero() && resident_offer == Some(on_chain_offer),
        admission,
    ))
}

/// Initial `TeeLeaseGuardGateV1` for this process.
///
/// A FullNode uses the upstream anchor that `prepare` verified. A validator
/// first tries local facts. When they cannot arm the gate and `--upstream` is
/// set, it verifies the upstream NodeHost admission and offer key, checks the
/// durable join anchor against the upstream chain, and waits at the higher
/// anchor. Every check fails closed.
pub(super) async fn initial_tee_lease_gate_v1(
    (protocol, node, args): (
        &dyn outbe_consensus::protocol_state::EngineProtocolState,
        &OutbeFullNode,
        &ConsensusArgs,
    ),
    chain: (u64, alloy_primitives::B256),
    identity: outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
    prepared_anchor: Option<crate::admission::LocalTeeAdmissionAnchorV1>,
) -> eyre::Result<crate::admission::TeeLeaseGuardAnchorsV1> {
    use crate::admission::{
        local_validator_tee_gate_v1, select_validator_upstream_anchor_v1, TeeLeaseGuardAnchorsV1,
    };

    if !args.is_validator {
        return Ok(TeeLeaseGuardAnchorsV1 {
            gate: prepared_anchor.map_or(TeeLeaseGuardGateV1::Armed, TeeLeaseGuardGateV1::Pending),
            durable: None,
        });
    }
    let durable = prepared_anchor;
    let local_finalized_height = node
        .provider
        .finalized_block_num_hash()
        .wrap_err("read local finalized height for the TEE lease guard")?
        .map_or(0, |finalized| finalized.number);
    let (local_ready_with_offer, local_admission) =
        if durable.is_none() && local_finalized_height >= 1 {
            local_tee_admission_ready_v1(protocol, node, chain, identity)?
        } else {
            (false, None)
        };
    let local_is_genesis_founder = if durable.is_none() && local_finalized_height == 0 {
        let signing_key_path = args
            .signing_key
            .as_ref()
            .ok_or(outbe_consensus::errors::StackEpochError::SigningKeyRequired)?;
        let key_backend = args.key_backend().wrap_err("invalid BLS key backend")?;
        let local_consensus_key =
            outbe_consensus::validators::load_signing_public_key(signing_key_path, &key_backend)
                .wrap_err("failed to load consensus signing public key for TEE lease guard")?;
        let genesis_validators = outbe_consensus::validators::read_consensus_validators_at_block(
            protocol,
            &node.provider,
            chain.1,
        )
        .wrap_err("failed to read genesis consensus validator set for TEE lease guard")?;
        genesis_validators
            .public_keys
            .contains(&local_consensus_key)
    } else {
        false
    };
    let upstream = args.upstream.as_deref();
    if let Some(gate) = local_validator_tee_gate_v1(
        durable,
        local_finalized_height,
        local_ready_with_offer,
        local_is_genesis_founder,
        upstream.is_some(),
    ) {
        if gate == TeeLeaseGuardGateV1::AwaitingAnchor {
            if let Some(reason) = local_admission.and_then(tee_lease_admission_rejection) {
                return Err(LaunchError::LocalNodeRejectedByTeeLease { reason }.into());
            }
        }
        return Ok(TeeLeaseGuardAnchorsV1 {
            gate,
            durable: None,
        });
    }
    let Some(upstream) = upstream else {
        // Without an upstream, the startup check reports the catch-up error.
        return Ok(TeeLeaseGuardAnchorsV1 {
            gate: durable.map_or(
                TeeLeaseGuardGateV1::AwaitingAnchor,
                TeeLeaseGuardGateV1::Pending,
            ),
            durable: None,
        });
    };
    let upstream_anchor = require_upstream_fullnode_tee_admission(upstream, identity).await?;
    require_upstream_offer_key_v1(upstream).await?;
    let upstream_hash_at_durable = match durable {
        Some(anchor) => upstream_block_hash_v1(upstream, anchor.finalized_height).await?,
        None => None,
    };
    let anchors =
        select_validator_upstream_anchor_v1(durable, upstream_anchor, upstream_hash_at_durable)?;
    info!(
        ?anchors,
        local_finalized_height,
        "validator TEE lease guard waits for UpstreamBootstrap to reach the admission anchor"
    );
    Ok(anchors)
}

pub(super) fn verify_initial_local_tee_admission_v1(
    (protocol, node, args): (
        &dyn outbe_consensus::protocol_state::EngineProtocolState,
        &OutbeFullNode,
        &ConsensusArgs,
    ),
    chain: outbe_execution::tee_remote_session::RegistryChainIdentity,
    local_tee_identity: outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
    anchors: &mut crate::admission::TeeLeaseGuardAnchorsV1,
    node_data_dir: &Path,
) -> eyre::Result<()> {
    let validator_has_recovery_anchor =
        args.is_validator && anchors.gate.pending_anchor().is_some();
    if let Some(admission) = read_gated_finalized_local_tee_admission(
        protocol,
        &node.provider,
        chain,
        local_tee_identity,
        anchors,
    )? {
        let rejection = if validator_has_recovery_anchor {
            validator_recovery_startup_admission_rejection(admission)
        } else {
            tee_lease_admission_rejection(admission)
        };
        if let Some(reason) = rejection {
            return Err(LaunchError::LocalNodeRejectedByTeeLease { reason }.into());
        }
        if args.is_validator && args.upstream.is_none() && !anchors.gate.is_armed() {
            if let Some(reason) = crate::admission::read_finalized_local_tee_admission(
                protocol,
                &node.provider,
                chain.chain_id,
                chain.genesis_hash,
                local_tee_identity,
            )?
            .and_then(validator_recovery_startup_admission_rejection)
            {
                return Err(LaunchError::LocalNodeRejectedByTeeLease { reason }.into());
            }
        }
    }
    require_validator_tee_recovery_complete_v1(
        args.is_validator,
        args.upstream.is_some(),
        anchors.gate,
        node_data_dir,
    )
}
