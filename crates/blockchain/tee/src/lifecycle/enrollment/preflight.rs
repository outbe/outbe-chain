use super::super::{LifecycleRegistryStateV1, VALIDATOR_STATUS_JAILED};
use super::*;
use crate::registry_view::{NodeBindingSelectorV1, RenewalBindingV1};
use outbe_primitives::tee_attestation_v1::AttestationOperationV1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnrollmentMode {
    InitialJoin,
    ActiveRejoin,
    ExpiredRejoin,
}

#[derive(Clone, Debug)]
pub struct EnrollmentPreflightV1 {
    pub registry: LifecycleRegistryStateV1,
    pub jailed: bool,
    pub balance: U256,
}

pub(super) async fn observe<P: LifecycleChainPort, S: std::error::Error + 'static>(
    port: &P,
    live: &InProcessTeeContext,
    sender: Address,
) -> Result<EnrollmentPreflightV1, EnrollmentError<P::Error, S>> {
    let at = port.finalized_checkpoint().await?;
    let registry = port
        .registry_state(at, &NodeBindingSelectorV1::Validator(sender))
        .await?;
    let manifest = live.manifest();
    let schedule = &registry.schedule;
    let pinned = (
        schedule.finalized_height,
        schedule.finalized_hash,
        schedule.finalized_timestamp,
    ) == (at.height, at.hash, at.timestamp);
    let chain = (U256::from(at.chain_id).to_be_bytes(), at.genesis_hash)
        == (manifest.chain_id, manifest.genesis_hash);
    let policy = (
        registry.policy.chain_id,
        registry.policy.genesis_hash,
        registry.policy.attestation_mode,
    ) == (
        manifest.chain_id,
        manifest.genesis_hash,
        manifest.attestation_mode,
    );
    if ![
        registry.checkpoint == at,
        pinned,
        chain,
        policy,
        !at.hash.is_zero(),
        !at.state_root.is_zero(),
    ]
    .into_iter()
    .all(|v| v)
    {
        return Err(EnrollmentError::IdentityMismatch);
    }
    // Check both indexes so an existing NodeHost cannot silently change its operator.
    let node = port
        .registry_state(
            at,
            &NodeBindingSelectorV1::NodeHost(manifest.node_id.reth_p2p_public),
        )
        .await?;
    if node.checkpoint != at || node.binding != registry.binding {
        return Err(EnrollmentError::IdentityMismatch);
    }
    let node_hash = manifest.node_id.node_id_hash()?;
    if registry
        .binding
        .as_ref()
        .is_some_and(|b| b.node_id_hash != node_hash)
    {
        return Err(EnrollmentError::IdentityMismatch);
    }
    let jailed = port
        .validator_record(at, sender)
        .await?
        .is_some_and(|record| record.status == VALIDATOR_STATUS_JAILED);
    let balance = port.balance(at, sender).await?;
    Ok(EnrollmentPreflightV1 {
        registry,
        jailed,
        balance,
    })
}

pub(super) fn wait_or_reject<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    view: &EnrollmentPreflightV1,
) -> Result<Option<EnrollmentOutcome>, EnrollmentError<E, S>> {
    // The port carries strict-retirement records only, including scheduled ones.
    if view.jailed {
        return Ok(Some(
            if view
                .registry
                .active_upgrade
                .is_some_and(|u| view.registry.checkpoint.height >= u.activation_height)
            {
                EnrollmentOutcome::RequiresStrictUpgradeLateRecovery
            } else {
                EnrollmentOutcome::WaitForUnjail
            },
        ));
    }
    if classify(&view.registry).is_none() {
        return Ok(Some(EnrollmentOutcome::WaitForLeaseExpiry));
    }
    Ok(None)
}

fn classify(view: &LifecycleRegistryStateV1) -> Option<EnrollmentMode> {
    match view.binding.as_ref() {
        None => Some(EnrollmentMode::InitialJoin),
        Some(b) if view.checkpoint.timestamp >= b.valid_until => {
            Some(EnrollmentMode::ExpiredRejoin)
        }
        Some(_) if view.policy.attestation_mode == AttestationMode::NoAttestationDev => {
            Some(EnrollmentMode::ActiveRejoin)
        }
        Some(_) => None,
    }
}

pub(super) fn registered_intent<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    view: &EnrollmentPreflightV1,
    live: &InProcessTeeContext,
) -> Result<Option<RegistrationIntentV1>, EnrollmentError<E, S>> {
    let Some(b) = view.registry.binding.as_ref() else {
        return Ok(None);
    };
    if b.enclave_id != live.manifest().enclave_id()?
        || view.registry.checkpoint.timestamp >= b.valid_until
    {
        return Ok(None);
    }
    let intent = intent_from_binding(live, b, view.registry.policy.attestation_mode);
    live.manifest().validate_intent_binding(&intent)?;
    if intent.intent_hash()? != b.intent_hash {
        return Err(EnrollmentError::IdentityMismatch);
    }
    Ok(Some(intent))
}

pub(super) fn intent_from_binding(
    live: &InProcessTeeContext,
    b: &RenewalBindingV1,
    mode: AttestationMode,
) -> RegistrationIntentV1 {
    let manifest = live.manifest();
    RegistrationIntentV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: mode,
        policy_hash: b.policy_hash,
        node_id: manifest.node_id.clone(),
        enclave_id: b.enclave_id,
        binding_id: b.binding_id,
        binding_version: b.binding_version,
        registration_version: b.registration_version,
        renewal_nonce: b.renewal_nonce,
        transition_nonce: b.transition_nonce,
        requested_valid_until: b.valid_until,
        recipient_x25519: b.recipient_x25519.into(),
        attestation_ed25519: b.attestation_ed25519.into(),
        noise_responder_x25519: b.noise_responder_x25519.into(),
        node_host_authorization_hash: b.node_host_authorization_hash,
    }
}
