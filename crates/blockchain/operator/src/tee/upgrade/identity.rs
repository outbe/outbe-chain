use super::*;

pub(super) fn validate_candidate_identity_v1(
    candidate: &ReplacementCandidateEnclaveV1,
    selector: &NodeBindingSelectorV1,
    active: &crate::tee::registry::FinalizedRenewalChainViewV1,
) -> Result<()> {
    let manifest = candidate.manifest();
    let node_id_hash = manifest
        .node_id
        .node_id_hash()
        .map_err(|error| eyre::eyre!("hash candidate node identity: {error}"))?;
    let enclave_id = manifest
        .enclave_id()
        .map_err(|error| eyre::eyre!("derive candidate enclave identity: {error}"))?;
    let node_host_authorization_hash = manifest
        .node_host_authorization_hash()
        .map_err(|error| eyre::eyre!("derive candidate NodeHost authorization: {error}"))?;
    let chain_matches = manifest.chain_id == active.policy.chain_id
        && manifest.genesis_hash == active.policy.genesis_hash
        && node_id_hash == active.binding.node_id_hash;
    if !chain_matches
        || enclave_id == active.binding.enclave_id
        || node_host_authorization_hash != active.binding.node_host_authorization_hash
    {
        eyre::bail!("candidate manifest is not a same-NodeHost successor of finalized A");
    }
    match selector {
        NodeBindingSelectorV1::NodeHost(public) if public == &manifest.node_id.reth_p2p_public => {}
        _ => {
            eyre::bail!("upgrade selector does not match the candidate node identity");
        }
    }
    Ok(())
}

pub(super) fn ensure_transition_source_or_target_v1(
    current: &crate::tee::registry::RenewalBindingV1,
    intent: &RegistrationIntentV1,
) -> Result<()> {
    if transition_target_matches_v1(current, intent) {
        return Ok(());
    }
    let source_matches = current.node_id_hash
        == intent
            .node_id
            .node_id_hash()
            .map_err(|error| eyre::eyre!("hash transition node identity: {error}"))?
        && source_counters_match(current, intent)
        && source_identity_changes_match(current, intent);
    if !source_matches {
        eyre::bail!("finalized Registry binding matches neither transition source nor target");
    }
    Ok(())
}

pub(super) fn transition_target_matches_v1(
    current: &crate::tee::registry::RenewalBindingV1,
    intent: &RegistrationIntentV1,
) -> bool {
    target_identity_matches(current, intent)
        && target_counters_match(current, intent)
        && target_keys_match(current, intent)
}
fn source_counters_match(
    current: &crate::tee::registry::RenewalBindingV1,
    intent: &RegistrationIntentV1,
) -> bool {
    current.binding_version.checked_add(1) == Some(intent.binding_version)
        && current.registration_version.checked_add(1) == Some(intent.registration_version)
        && current.renewal_nonce == intent.renewal_nonce
        && current.transition_nonce.checked_add(1) == Some(intent.transition_nonce)
}
fn target_identity_matches(
    current: &crate::tee::registry::RenewalBindingV1,
    intent: &RegistrationIntentV1,
) -> bool {
    current.node_id_hash == intent.node_id.node_id_hash().unwrap_or(B256::ZERO)
        && current.enclave_id == intent.enclave_id
        && current.binding_id == intent.binding_id
        && current.intent_hash == intent.intent_hash().unwrap_or(B256::ZERO)
}
fn target_counters_match(
    current: &crate::tee::registry::RenewalBindingV1,
    intent: &RegistrationIntentV1,
) -> bool {
    current.policy_hash == intent.policy_hash
        && current.binding_version == intent.binding_version
        && current.registration_version == intent.registration_version
        && target_lease_matches(current, intent)
}
fn target_lease_matches(
    current: &crate::tee::registry::RenewalBindingV1,
    intent: &RegistrationIntentV1,
) -> bool {
    current.renewal_nonce == intent.renewal_nonce
        && current.transition_nonce == intent.transition_nonce
        && current.valid_until == intent.requested_valid_until
}
fn target_keys_match(
    current: &crate::tee::registry::RenewalBindingV1,
    intent: &RegistrationIntentV1,
) -> bool {
    current.recipient_x25519 == B256::from(intent.recipient_x25519)
        && current.attestation_ed25519 == B256::from(intent.attestation_ed25519)
        && current.noise_responder_x25519 == B256::from(intent.noise_responder_x25519)
        && current.node_host_authorization_hash == intent.node_host_authorization_hash
}

fn source_identity_changes_match(
    current: &crate::tee::registry::RenewalBindingV1,
    intent: &RegistrationIntentV1,
) -> bool {
    current.node_host_authorization_hash == intent.node_host_authorization_hash
        && current.enclave_id != intent.enclave_id
        && current.binding_id != intent.binding_id
}
