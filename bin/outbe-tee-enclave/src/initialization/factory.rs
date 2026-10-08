//! Construct enclave initialization state for each runtime environment.

use super::*;

pub fn production(
    boot: Arc<EnclaveBootConfig>,
    keys: &EnclaveKeys,
) -> Result<InitializationState, String> {
    let mut challenge = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut challenge);
    if crate::transport::sealing_key().is_none() {
        return Err("production initialization requires an SGX sealing key".to_string());
    }
    let attestation = crate::gramine::attestation_type();
    #[cfg(all(not(feature = "mock"), feature = "production-dcap-release"))]
    let trusted_network_descriptor = match &attestation {
        crate::gramine::AttestationType::Dcap => Some(load_trusted_network_descriptor_v1()?),
        other => {
            return Err(format!(
                "production DCAP release refuses runtime attestation {}",
                other.label()
            ));
        }
    };
    #[cfg(all(not(feature = "mock"), not(feature = "production-dcap-release")))]
    let trusted_network_descriptor = match &attestation {
        crate::gramine::AttestationType::Dcap | crate::gramine::AttestationType::SgxNoAttest => {
            Some(load_trusted_network_descriptor_v1()?)
        }
        _ => None,
    };
    #[cfg(feature = "mock")]
    let trusted_network_descriptor = None;
    #[cfg(not(feature = "mock"))]
    if let Some(descriptor) = trusted_network_descriptor.as_ref() {
        let chain_id = u64::try_from(alloy_primitives::U256::from_be_bytes(
            descriptor.network_binding.chain_id,
        ))
        .map_err(|_| "measured consensus chain id does not fit u64".to_owned())?;
        outbe_consensus::config::init_consensus_chain_id(chain_id)
            .map_err(|error| format!("bind measured consensus chain id: {error}"))?;
    }
    production_with_challenge_and_attestation_inner(
        boot,
        keys,
        challenge,
        attestation,
        trusted_network_descriptor,
    )
}

/// Separate process-harness seam: production protocol, software sealing.
#[cfg(feature = "local-e2e")]
pub fn local_e2e(
    boot: Arc<EnclaveBootConfig>,
    keys: &EnclaveKeys,
    descriptor: TrustedNetworkDescriptorV1,
) -> Result<InitializationState, String> {
    if !crate::local_e2e::configured() {
        return Err("E2E sealing is not configured".into());
    }
    let mut challenge = [0; 32];
    rand_core::OsRng.fill_bytes(&mut challenge);
    production_with_challenge_and_attestation_inner(
        boot,
        keys,
        challenge,
        crate::gramine::AttestationType::SgxNoAttest,
        Some(descriptor),
    )
}

#[cfg(test)]
pub(super) fn production_with_challenge(
    boot: Arc<EnclaveBootConfig>,
    keys: &EnclaveKeys,
    challenge: [u8; 32],
) -> Result<InitializationState, String> {
    production_with_challenge_and_attestation_inner(
        boot,
        keys,
        challenge,
        crate::gramine::AttestationType::Dcap,
        None,
    )
}

fn production_with_challenge_and_attestation_inner(
    boot: Arc<EnclaveBootConfig>,
    keys: &EnclaveKeys,
    challenge: [u8; 32],
    attestation: crate::gramine::AttestationType,
    trusted_network_descriptor: Option<TrustedNetworkDescriptorV1>,
) -> Result<InitializationState, String> {
    if challenge == [0; 32] {
        return Err("initialization challenge must be nonzero".to_string());
    }
    let restored = restore_manifest(&boot, keys)?;
    let state = InitializationState {
        mode: InitializationMode::Production,
        gramine_direct_dev_evidence_allowed: matches!(
            &attestation,
            crate::gramine::AttestationType::SgxNoAttest
        ),
        attestation,
        challenge,
        boot: Some(boot),
        trusted_network_descriptor,
        mock_network_binding: None,
        stored: Mutex::new(restored.map(|manifest| StoredInitialization {
            manifest,
            loaded_from_seal: true,
        })),
        remote_sessions: Mutex::new(BTreeMap::new()),
        remote_admission_generation: std::sync::atomic::AtomicU64::new(0),
    };
    if let Some(manifest) = state.manifest()? {
        state.validate_network_binding(&manifest)?;
    }
    Ok(state)
}

#[cfg(test)]
pub(crate) fn production_with_challenge_and_attestation(
    boot: Arc<EnclaveBootConfig>,
    keys: &EnclaveKeys,
    challenge: [u8; 32],
    attestation: crate::gramine::AttestationType,
) -> Result<InitializationState, String> {
    production_with_challenge_and_attestation_inner(boot, keys, challenge, attestation, None)
}

#[cfg(test)]
pub(crate) fn production_with_trusted_network_descriptor(
    boot: Arc<EnclaveBootConfig>,
    keys: &EnclaveKeys,
    challenge: [u8; 32],
    trusted_network_descriptor: TrustedNetworkDescriptorV1,
) -> Result<InitializationState, String> {
    production_with_challenge_and_attestation_inner(
        boot,
        keys,
        challenge,
        crate::gramine::AttestationType::Dcap,
        Some(trusted_network_descriptor),
    )
}

/// Hardware-free production-session state for cross-crate integration
/// tests. This seam is absent unless the enclave is built with `mock`.
#[cfg(feature = "mock")]
pub fn production_with_synthetic_dcap_for_test(
    boot: Arc<EnclaveBootConfig>,
    keys: &EnclaveKeys,
) -> Result<InitializationState, String> {
    let mut challenge = [0_u8; 32];
    rand_core::OsRng.fill_bytes(&mut challenge);
    production_with_challenge_and_attestation_inner(
        boot,
        keys,
        challenge,
        crate::gramine::AttestationType::Dcap,
        None,
    )
}

/// Separate dev/mock behavior. It never creates a production authorization
/// claim and is selected only by the required-feature mock binary or tests.
pub fn development() -> InitializationState {
    InitializationState {
        mode: InitializationMode::Development,
        attestation: crate::gramine::AttestationType::Unavailable,
        gramine_direct_dev_evidence_allowed: true,
        challenge: [0xDD; 32],
        boot: None,
        trusted_network_descriptor: None,
        mock_network_binding: None,
        stored: Mutex::new(None),
        remote_sessions: Mutex::new(BTreeMap::new()),
        remote_admission_generation: std::sync::atomic::AtomicU64::new(0),
    }
}

/// Hardware-free transport tests still exercise the exact network-bound DKG
/// protocol. This constructor exists only in mock builds and cannot create a
/// production authorization or sealed state.
#[cfg(feature = "mock")]
pub fn development_for_network(
    network_binding: outbe_primitives::tee_attestation_v1::NetworkBindingV1,
) -> InitializationState {
    let mut state = development();
    state.mock_network_binding = Some(network_binding);
    state
}
