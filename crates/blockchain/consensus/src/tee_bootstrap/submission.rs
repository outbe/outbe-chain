use super::*;

pub fn build_in_process_tee_bootstrap_submission_v2(
    context: &outbe_tee::in_process::InProcessTeeContext,
    request: LocalRegistrationRequest<'_>,
    evm_signer: &OutbeEvmSigner,
    sign_node_hash: impl Fn(B256) -> Result<[u8; 65], crate::errors::TeeBootstrapError>,
) -> eyre::Result<TeeBootstrapParticipantSubmissionV2> {
    let identity = production_identity(Some(context.manifest()), &request.node_id, request.policy)?;
    assemble_submission(
        request,
        identity,
        evm_signer,
        sign_node_hash,
        |mode, intent| {
            let (evidence, signature) = match mode {
                AttestationMode::DcapRequired => {
                    let generated = context.generate_dcap_quote(&intent)?;
                    let components = acquire_dcap_collateral_v1(&generated.quote_body)?;
                    (
                        AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
                            intent,
                            quote: generated.quote_body,
                            components,
                            transition_key_ready_proof: generated.transition_key_ready_proof,
                        }),
                        generated.enclave_signature,
                    )
                }
                AttestationMode::NoAttestationDev => {
                    let signature = context.sign_registration_intent_dev_v1(&intent)?;
                    (
                        AttestationEvidenceV1::NoAttestationDev(NoAttestationDevEvidenceV1 {
                            dev_attestation_public: intent.attestation_ed25519,
                            dev_signature: signature,
                            transition_key_ready_proof: None,
                            intent,
                        }),
                        signature,
                    )
                }
            };
            Ok((evidence, signature))
        },
    )
}

fn assemble_submission(
    request: LocalRegistrationRequest<'_>,
    identity: LocalEnclaveIdentity,
    evm_signer: &OutbeEvmSigner,
    sign_node_hash: impl Fn(B256) -> Result<[u8; 65], crate::errors::TeeBootstrapError>,
    mut evidence: impl FnMut(
        AttestationMode,
        RegistrationIntentV1,
    ) -> eyre::Result<(AttestationEvidenceV1, [u8; 64])>,
) -> eyre::Result<TeeBootstrapParticipantSubmissionV2> {
    let policy = request.policy;
    let policy_hash = policy
        .policy_hash()
        .map_err(|error| crate::errors::TeeBootstrapError::InvalidPolicy(format!("{error}")))?;
    let (intent, node_id_hash) = request.into_intent(identity, policy_hash)?;
    let intent_hash = intent.intent_hash().map_err(|error| {
        crate::errors::TeeBootstrapError::InvalidRegistrationIntent(format!("{error}"))
    })?;
    let node_signature = sign_node_hash(intent_hash).map_err(|error| {
        crate::errors::TeeBootstrapError::SignRegistrationIntentFailed(format!("{error}"))
    })?;
    let SignedValidatorBinding {
        validator_binding,
        validator_signature,
        node_binding_signature,
    } = sign_validator_binding(policy, node_id_hash, evm_signer, &sign_node_hash)?;
    let (evidence, enclave_signature) = evidence(policy.attestation_mode, intent)?;
    evidence.encode_canonical().map_err(|error| {
        crate::errors::TeeBootstrapError::LocalEvidenceNotCanonical(format!("{error}"))
    })?;
    Ok(TeeBootstrapParticipantSubmissionV2 {
        evidence,
        validator_binding,
        validator_signature,
        node_binding_signature,
        node_signature,
        enclave_signature,
    })
}

/// Identity and attestation policy for one local OST3 registration. The
/// enclave session and signing capabilities are supplied separately.
pub struct LocalRegistrationRequest<'a> {
    pub production_manifest: Option<&'a EnclaveInitializationManifestV1>,
    pub node_id: NodeIdV1,
    pub policy: &'a TeePolicyV1,
    pub requested_valid_until: u64,
}

struct LocalEnclaveIdentity {
    recipient_x25519: [u8; 32],
    attestation_ed25519: [u8; 32],
    noise_responder_x25519: [u8; 32],
    enclave_id: B256,
    node_host_authorization_hash: B256,
}

impl LocalRegistrationRequest<'_> {
    fn into_intent(
        self,
        identity: LocalEnclaveIdentity,
        policy_hash: B256,
    ) -> eyre::Result<(RegistrationIntentV1, B256)> {
        let LocalEnclaveIdentity {
            recipient_x25519,
            attestation_ed25519,
            noise_responder_x25519,
            enclave_id,
            node_host_authorization_hash,
        } = identity;
        let node_id_hash = self.node_id.node_id_hash().map_err(|error| {
            crate::errors::TeeBootstrapError::InvalidNodeIdentity(format!("{error}"))
        })?;
        let binding_id = bootstrap_binding_id(node_id_hash, enclave_id, policy_hash);
        let intent = RegistrationIntentV1 {
            chain_id: self.policy.chain_id,
            genesis_hash: self.policy.genesis_hash,
            operation: AttestationOperationV1::RegisterEnclave,
            attestation_mode: self.policy.attestation_mode,
            policy_hash,
            node_id: self.node_id,
            enclave_id,
            binding_id,
            binding_version: 1,
            registration_version: 0,
            renewal_nonce: 0,
            transition_nonce: 0,
            requested_valid_until: self.requested_valid_until,
            recipient_x25519,
            attestation_ed25519,
            noise_responder_x25519,
            node_host_authorization_hash,
        };
        Ok((intent, node_id_hash))
    }
}

fn production_identity(
    production_manifest: Option<&EnclaveInitializationManifestV1>,
    node_id: &NodeIdV1,
    policy: &TeePolicyV1,
) -> eyre::Result<LocalEnclaveIdentity> {
    let manifest = production_manifest
        .ok_or(crate::errors::TeeBootstrapError::ProductionRequiresCommittedManifest)?;
    if manifest.chain_id != policy.chain_id
        || manifest.genesis_hash != policy.genesis_hash
        || &manifest.node_id != node_id
    {
        return Err(crate::errors::TeeBootstrapError::CommittedManifestMismatch.into());
    }
    Ok(LocalEnclaveIdentity {
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        enclave_id: manifest.enclave_id().map_err(|error| {
            crate::errors::TeeBootstrapError::InvalidCommittedEnclaveIdentity(error.to_string())
        })?,
        node_host_authorization_hash: manifest.node_host_authorization_hash().map_err(|error| {
            crate::errors::TeeBootstrapError::InvalidCommittedNodeHostAuthorization(
                error.to_string(),
            )
        })?,
    })
}

struct SignedValidatorBinding {
    validator_binding: ValidatorNodeBindingV1,
    validator_signature: [u8; 65],
    node_binding_signature: [u8; 65],
}

fn sign_validator_binding(
    policy: &TeePolicyV1,
    node_id_hash: B256,
    evm_signer: &OutbeEvmSigner,
    sign_node_hash: &impl Fn(B256) -> Result<[u8; 65], crate::errors::TeeBootstrapError>,
) -> eyre::Result<SignedValidatorBinding> {
    let validator_binding = ValidatorNodeBindingV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        validator: evm_signer.address().into_array(),
        node_id_hash,
    };
    let binding_hash = validator_binding.binding_hash().map_err(|error| {
        crate::errors::TeeBootstrapError::InvalidValidatorNodeHostBinding(error.to_string())
    })?;
    let validator_signature = evm_signer.sign_hash(&binding_hash).map_err(|error| {
        crate::errors::TeeBootstrapError::SignValidatorNodeHostBindingFailed(error.to_string())
    })?;
    let node_binding_signature = sign_node_hash(binding_hash).map_err(|error| {
        crate::errors::TeeBootstrapError::SignNodeHostValidatorBindingFailed(error.to_string())
    })?;
    Ok(SignedValidatorBinding {
        validator_binding,
        validator_signature,
        node_binding_signature,
    })
}

/// Frozen OST3 binding preimage, used by resident registration.
pub(super) fn bootstrap_binding_id(node: B256, enclave: B256, policy: B256) -> B256 {
    let mut preimage = Vec::with_capacity(OST3_BINDING_ID_DOMAIN.len() + 96);
    preimage.extend_from_slice(OST3_BINDING_ID_DOMAIN);
    preimage.extend_from_slice(node.as_slice());
    preimage.extend_from_slice(enclave.as_slice());
    preimage.extend_from_slice(policy.as_slice());
    keccak256(preimage)
}
