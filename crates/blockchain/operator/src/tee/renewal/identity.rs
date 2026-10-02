//! Private renewal identity stage.
use super::*;

pub(super) fn validate_identity(
    config: &RenewalServiceConfigV1,
    view: &FinalizedRenewalChainViewV1,
) -> Result<()> {
    let manifest = &config.manifest;
    let node_id_hash = manifest
        .node_id
        .node_id_hash()
        .map_err(|error| eyre::eyre!("hash manifest node identity: {error}"))?;
    let enclave_id = manifest
        .enclave_id()
        .map_err(|error| eyre::eyre!("derive manifest enclave identity: {error}"))?;
    let authorization = manifest
        .node_host_authorization_hash()
        .map_err(|error| eyre::eyre!("derive manifest NodeHost authorization: {error}"))?;
    let policy_hash = view
        .policy
        .policy_hash()
        .map_err(|error| eyre::eyre!("hash finalized policy: {error}"))?;
    let hashes = RenewalManifestHashes {
        node_id_hash,
        enclave_id,
        authorization,
        policy_hash,
    };
    if !manifest_matches(manifest, view, &hashes) {
        eyre::bail!("committed NodeHost manifest does not match the finalized Registry binding");
    }
    match &config.selector {
        NodeBindingSelectorV1::NodeHost(public) if public == &manifest.node_id.reth_p2p_public => {}
        _ => {
            eyre::bail!("renewal selector does not match the committed node identity");
        }
    }
    Ok(())
}

pub(super) struct RenewalManifestHashes {
    pub(super) node_id_hash: B256,
    pub(super) enclave_id: B256,
    pub(super) authorization: B256,
    pub(super) policy_hash: B256,
}

pub(super) fn manifest_matches(
    manifest: &EnclaveInitializationManifestV1,
    view: &FinalizedRenewalChainViewV1,
    hashes: &RenewalManifestHashes,
) -> bool {
    manifest_binding_matches(manifest, view, hashes.node_id_hash, hashes.enclave_id)
        && manifest_keys_match(manifest, &view.binding)
        && hashes.authorization == view.binding.node_host_authorization_hash
        && hashes.policy_hash == view.binding.policy_hash
}

pub(super) fn manifest_binding_matches(
    manifest: &EnclaveInitializationManifestV1,
    view: &FinalizedRenewalChainViewV1,
    node_id_hash: B256,
    enclave_id: B256,
) -> bool {
    manifest.chain_id == view.policy.chain_id
        && manifest.genesis_hash == view.policy.genesis_hash
        && node_id_hash == view.binding.node_id_hash
        && enclave_id == view.binding.enclave_id
}

pub(super) fn manifest_keys_match(
    manifest: &EnclaveInitializationManifestV1,
    binding: &RenewalBindingV1,
) -> bool {
    B256::from(manifest.recipient_x25519) == binding.recipient_x25519
        && B256::from(manifest.attestation_ed25519) == binding.attestation_ed25519
        && B256::from(manifest.noise_responder_x25519) == binding.noise_responder_x25519
}

pub(super) fn ensure_source_or_conflict(
    current: &RenewalBindingV1,
    source: &RenewalBindingV1,
) -> Result<()> {
    if current != source {
        eyre::bail!("finalized Registry binding matches neither renewal source nor target");
    }
    Ok(())
}

pub(super) fn target_matches(
    current: &RenewalBindingV1,
    attempt: &PreparedRenewalV1,
) -> Result<bool> {
    let intent = RegistrationIntentV1::decode_canonical(&attempt.intent)
        .map_err(|error| eyre::eyre!("decode journal renewal intent: {error}"))?;
    Ok(target_identity_matches(current, attempt, &intent)
        && target_counters_match(current, &intent)
        && target_keys_match(current, &intent))
}

pub(super) fn target_identity_matches(
    current: &RenewalBindingV1,
    attempt: &PreparedRenewalV1,
    intent: &RegistrationIntentV1,
) -> bool {
    current.node_id_hash == attempt.source.node_id_hash
        && current.enclave_id == intent.enclave_id
        && current.binding_id == intent.binding_id
        && target_commitments_match(current, attempt, intent)
}

pub(super) fn target_commitments_match(
    current: &RenewalBindingV1,
    attempt: &PreparedRenewalV1,
    intent: &RegistrationIntentV1,
) -> bool {
    current.intent_hash == attempt.intent_hash
        && current.evidence_hash == attempt.evidence_hash
        && current.policy_hash == intent.policy_hash
}

pub(super) fn target_counters_match(
    current: &RenewalBindingV1,
    intent: &RegistrationIntentV1,
) -> bool {
    current.binding_version == intent.binding_version
        && current.registration_version == intent.registration_version
        && current.renewal_nonce == intent.renewal_nonce
        && target_transition_and_lease_match(current, intent)
}

pub(super) fn target_transition_and_lease_match(
    current: &RenewalBindingV1,
    intent: &RegistrationIntentV1,
) -> bool {
    current.transition_nonce == intent.transition_nonce
        && current.valid_until == intent.requested_valid_until
}

pub(super) fn target_keys_match(current: &RenewalBindingV1, intent: &RegistrationIntentV1) -> bool {
    current.recipient_x25519 == B256::from(intent.recipient_x25519)
        && current.attestation_ed25519 == B256::from(intent.attestation_ed25519)
        && current.noise_responder_x25519 == B256::from(intent.noise_responder_x25519)
        && current.node_host_authorization_hash == intent.node_host_authorization_hash
}
