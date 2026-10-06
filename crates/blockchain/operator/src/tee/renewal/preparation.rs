//! Private renewal preparation stage.
use super::lifecycle::next_renewal_deadline;
use super::*;

use crate::tx::UnsignedRelayTransactionV1;
pub(super) struct RenewalPreparation<'a, R, E, N> {
    pub(super) rpc: &'a R,
    pub(super) evm_signer: &'a RelaySignerV1,
    pub(super) enclave: &'a mut E,
    pub(super) node_signer: &'a N,
    pub(super) config: &'a RenewalServiceConfigV1,
}

pub(super) async fn prepare_attempt<
    R: RelayPreparationRpc + Sync,
    E: RenewalEnclaveV1,
    N: RenewalNodeSignerV1,
>(
    preparation: &mut RenewalPreparation<'_, R, E, N>,
    view: &FinalizedRenewalChainViewV1,
) -> Result<PreparedRenewalV1> {
    let rpc = preparation.rpc;
    let evm_signer = preparation.evm_signer;
    let enclave = &mut *preparation.enclave;
    let node_signer = preparation.node_signer;
    let config = preparation.config;
    let desired_valid_until = next_renewal_deadline(&view.binding, view.policy.maximum_lease)
        .ok_or_else(|| eyre::eyre!("maximum renewal lease overflows timestamp"))?;
    let intent = renewal_intent(config, view, desired_valid_until)?;
    let generated_evidence = generate_renewal_evidence(
        enclave,
        &intent,
        &view.policy,
        RenewalEvidenceWindow {
            finalized_timestamp: view.schedule.finalized_timestamp,
            desired_valid_until,
        },
    )?;
    let intent_hash = intent
        .intent_hash()
        .map_err(|error| eyre::eyre!("hash renewal intent: {error}"))?;
    let node_signature = node_signer
        .sign_node_hash(intent_hash)
        .wrap_err("sign renewal intent with node authority")?;
    let enclave_signature = generated_evidence.enclave_signature;
    let evidence = generated_evidence.evidence;
    let evidence_hash = generated_evidence.evidence_hash;
    let calldata = ITeeRegistryV1::renewEnclaveCall {
        evidence: evidence.clone().into(),
        nodeSignature: node_signature.to_vec().into(),
        enclaveSignature: enclave_signature.to_vec().into(),
    }
    .abi_encode();
    let gas_limit = TeeRegistryGasScheduleV1::normative()
        .maximum_transaction_gas(
            RegistryMutatorV1::RenewEnclave,
            calldata.len(),
            evidence.len(),
            view.policy.measurement_rules.len(),
            view.policy.attestation_mode,
        )
        .map_err(|error| eyre::eyre!("calculate normative renewal gas: {error}"))?;
    let chain_id = rpc.chain_id().await?;
    let account_nonce = rpc.transaction_count(evm_signer.address()).await?;
    let gas_price = buffered_gas_price(rpc.gas_price().await?);
    let required_balance = gas_price.saturating_mul(U256::from(gas_limit));
    let balance = rpc.balance(evm_signer.address()).await?;
    if balance < required_balance {
        eyre::bail!(
            "renewal EVM signer {} has {balance} but needs at least {required_balance}",
            evm_signer.address()
        );
    }
    let raw = evm_signer.sign_renewal(UnsignedRelayTransactionV1 {
        chain_id,
        account_nonce,
        gas_price,
        gas_limit,
        to: TEE_REGISTRY_ADDRESS,
        calldata: &calldata,
    })?;
    let intent_bytes = intent
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode canonical renewal intent: {error}"))?;
    Ok(PreparedRenewalV1 {
        source: view.binding.clone(),
        intent: intent_bytes,
        intent_hash,
        evidence_hash,
        evidence,
        node_signature: node_signature.to_vec(),
        enclave_signature: enclave_signature.to_vec(),
        calldata_hash: keccak256(&calldata),
        calldata,
        requested_valid_until: intent.requested_valid_until,
        collateral_valid_until: generated_evidence.collateral_valid_until,
        collateral_margin: generated_evidence.collateral_margin,
        // The V1 journal shape keeps `relay` for restart compatibility.
        // Manual renewal binds it to the caller's global EVM signer.
        relay: evm_signer.address(),
        relay_variants: vec![raw],
    })
}

pub(super) fn renewal_intent(
    config: &RenewalServiceConfigV1,
    view: &FinalizedRenewalChainViewV1,
    requested_valid_until: u64,
) -> Result<RegistrationIntentV1> {
    Ok(RegistrationIntentV1 {
        chain_id: view.policy.chain_id,
        genesis_hash: view.policy.genesis_hash,
        operation: AttestationOperationV1::RenewEnclave,
        attestation_mode: view.policy.attestation_mode,
        policy_hash: view
            .policy
            .policy_hash()
            .map_err(|error| eyre::eyre!("hash active policy: {error}"))?,
        node_id: config.manifest.node_id.clone(),
        enclave_id: view.binding.enclave_id,
        binding_id: view.binding.binding_id,
        binding_version: view.binding.binding_version,
        registration_version: view
            .binding
            .registration_version
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("registration version exhausted"))?,
        renewal_nonce: view
            .binding
            .renewal_nonce
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("renewal nonce exhausted"))?,
        transition_nonce: view.binding.transition_nonce,
        requested_valid_until,
        recipient_x25519: config.manifest.recipient_x25519,
        attestation_ed25519: config.manifest.attestation_ed25519,
        noise_responder_x25519: config.manifest.noise_responder_x25519,
        node_host_authorization_hash: view.binding.node_host_authorization_hash,
    })
}

pub(super) struct GeneratedRenewalEvidenceV1 {
    pub(super) evidence: Vec<u8>,
    pub(super) evidence_hash: B256,
    pub(super) enclave_signature: [u8; 64],
    pub(super) collateral_valid_until: u64,
    pub(super) collateral_margin: u64,
}

pub(super) struct RenewalEvidenceWindow {
    pub(super) finalized_timestamp: u64,
    pub(super) desired_valid_until: u64,
}

pub(super) fn generate_renewal_evidence(
    enclave: &mut impl RenewalEnclaveV1,
    intent: &RegistrationIntentV1,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    window: RenewalEvidenceWindow,
) -> Result<GeneratedRenewalEvidenceV1> {
    let RenewalEvidenceWindow {
        finalized_timestamp,
        desired_valid_until,
    } = window;
    let window = RenewalEvidenceWindow {
        finalized_timestamp,
        desired_valid_until,
    };
    match policy.attestation_mode {
        AttestationMode::DcapRequired => generate_dcap_renewal(enclave, intent, policy, window),
        AttestationMode::GramineDirectDev => generate_direct_renewal(enclave, intent),
    }
}

pub(super) fn generate_dcap_renewal(
    enclave: &mut impl RenewalEnclaveV1,
    intent: &RegistrationIntentV1,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    window: RenewalEvidenceWindow,
) -> Result<GeneratedRenewalEvidenceV1> {
    let RenewalEvidenceWindow {
        finalized_timestamp,
        desired_valid_until,
    } = window;
    let (dcap, enclave_signature) = generate_dcap_evidence(enclave, intent, policy)?;
    let window = dcap_collateral_validity_window_v1(&dcap, policy)
        .map_err(|error| eyre::eyre!("validate signed renewal collateral window: {error:?}"))?;
    let ceiling = window
        .expiration_ceiling
        .checked_sub(policy.collateral_margin)
        .ok_or_else(|| eyre::eyre!("renewal collateral cannot satisfy the active margin"))?;
    if window.issue_floor > finalized_timestamp || desired_valid_until > ceiling {
        eyre::bail!("fresh Intel collateral cannot cover the exact next renewal deadline");
    }
    let value = AttestationEvidenceV1::Dcap(dcap);
    let evidence = value
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode canonical renewal evidence: {error}"))?;
    let evidence_hash = dcap_evidence_hash_v1(&evidence)
        .map_err(|code| eyre::eyre!("hash canonical renewal DCAP evidence: {code:?}"))?;
    Ok(GeneratedRenewalEvidenceV1 {
        evidence,
        evidence_hash,
        enclave_signature,
        collateral_valid_until: window.expiration_ceiling,
        collateral_margin: policy.collateral_margin,
    })
}

pub(super) fn generate_direct_renewal(
    enclave: &mut impl RenewalEnclaveV1,
    intent: &RegistrationIntentV1,
) -> Result<GeneratedRenewalEvidenceV1> {
    let enclave_signature = enclave
        .sign_registration_intent_dev_v1(intent)
        .wrap_err("sign renewal intent inside GramineDirectDev enclave")?;
    if !intent.verify_enclave_signature(&enclave_signature) {
        eyre::bail!("GramineDirectDev enclave signature does not bind renewal intent");
    }
    let value = AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
        transition_key_ready_proof: None,
        intent: intent.clone(),
        dev_attestation_public: intent.attestation_ed25519,
        dev_signature: enclave_signature,
    });
    let evidence_hash = value
        .evidence_hash()
        .map_err(|error| eyre::eyre!("hash canonical renewal evidence: {error}"))?;
    let evidence = value
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode canonical renewal evidence: {error}"))?;
    Ok(GeneratedRenewalEvidenceV1 {
        evidence,
        evidence_hash,
        enclave_signature,
        collateral_valid_until: u64::MAX,
        collateral_margin: 0,
    })
}

pub(super) fn generate_dcap_evidence(
    enclave: &mut impl RenewalEnclaveV1,
    intent: &RegistrationIntentV1,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
) -> Result<(DcapEvidenceV1, [u8; 64])> {
    let generated = enclave
        .generate_dcap_quote(intent)
        .wrap_err("generate intent-bound renewal quote")?;
    let components = acquire_dcap_collateral_v1(&generated.quote_body)
        .map_err(|error| eyre::eyre!("acquire renewal collateral: {error}"))?;
    let evidence = DcapEvidenceV1 {
        intent: intent.clone(),
        quote: generated.quote_body,
        components,
        transition_key_ready_proof: generated.transition_key_ready_proof,
    };
    dcap_collateral_validity_window_v1(&evidence, policy)
        .map_err(|error| eyre::eyre!("validate renewal collateral: {error:?}"))?;
    Ok((evidence, generated.enclave_signature))
}
