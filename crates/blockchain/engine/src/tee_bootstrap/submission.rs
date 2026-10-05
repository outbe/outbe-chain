use super::*;

/// Build this validator's complete block-1 OST3 submission. Production quote
/// generation is available only through the NodeHost-authorized enclave and
/// collateral acquisition is host-only startup work. The separate development
/// mode signs an explicit GramineDirectDev intent and never creates a DCAP
/// verdict or hardware-attestation claim.
pub fn build_local_tee_bootstrap_submission_v2(
    client: &mut RuntimeEnclaveClient,
    request: LocalRegistrationRequest<'_>,
    evm_signer: &OutbeEvmSigner,
    sign_node_hash: impl Fn(B256) -> Result<[u8; 65], String>,
) -> eyre::Result<TeeBootstrapParticipantSubmissionV2> {
    let policy = request.policy;
    let policy_hash = policy
        .policy_hash()
        .map_err(|error| eyre::eyre!("invalid OST3 policy: {error}"))?;

    let identity = request.resolve_identity(client)?;
    let (intent, node_id_hash) = request.into_intent(identity, policy_hash)?;
    let intent_hash = intent
        .intent_hash()
        .map_err(|error| eyre::eyre!("invalid OST3 registration intent: {error}"))?;
    let node_signature = sign_node_hash(intent_hash)
        .map_err(|error| eyre::eyre!("cannot sign OST3 registration intent: {error}"))?;
    let SignedValidatorBinding {
        validator_binding,
        validator_signature,
        node_binding_signature,
    } = sign_validator_binding(policy, node_id_hash, evm_signer, &sign_node_hash)?;
    let (evidence, enclave_signature) = create_evidence(client, policy.attestation_mode, intent)?;
    evidence
        .encode_canonical()
        .map_err(|error| eyre::eyre!("local OST3 evidence is not canonical: {error}"))?;
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
    fn resolve_identity(
        &self,
        client: &mut RuntimeEnclaveClient,
    ) -> eyre::Result<LocalEnclaveIdentity> {
        match (self.policy.attestation_mode, client) {
            (
                AttestationMode::DcapRequired | AttestationMode::GramineDirectDev,
                RuntimeEnclaveClient::Production(_),
            ) => production_identity(self.production_manifest, &self.node_id, self.policy),
            (AttestationMode::GramineDirectDev, RuntimeEnclaveClient::Development(dev)) => {
                development_identity(dev.quote(), &self.node_id, self.policy)
            }
            (AttestationMode::DcapRequired, RuntimeEnclaveClient::Development(_)) => Err(
                eyre::eyre!("DcapRequired genesis policy cannot use a development enclave session"),
            ),
        }
    }

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
        let node_id_hash = self
            .node_id
            .node_id_hash()
            .map_err(|error| eyre::eyre!("invalid OST3 node identity: {error}"))?;
        let binding_id = {
            let mut preimage = Vec::with_capacity(OST3_BINDING_ID_DOMAIN.len() + 96);
            preimage.extend_from_slice(OST3_BINDING_ID_DOMAIN);
            preimage.extend_from_slice(node_id_hash.as_slice());
            preimage.extend_from_slice(enclave_id.as_slice());
            preimage.extend_from_slice(policy_hash.as_slice());
            keccak256(preimage)
        };
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
        .ok_or_else(|| eyre::eyre!("production OST3 requires one committed NodeHost manifest"))?;
    if manifest.chain_id != policy.chain_id
        || manifest.genesis_hash != policy.genesis_hash
        || &manifest.node_id != node_id
    {
        return Err(eyre::eyre!(
            "committed NodeHost manifest does not match OST3 chain or persistent P2P identity"
        ));
    }
    Ok(LocalEnclaveIdentity {
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        enclave_id: manifest
            .enclave_id()
            .map_err(|error| eyre::eyre!("invalid committed enclave identity: {error}"))?,
        node_host_authorization_hash: manifest
            .node_host_authorization_hash()
            .map_err(|error| eyre::eyre!("invalid committed NodeHost authorization: {error}"))?,
    })
}

fn development_identity(
    quote: &EnclaveResponse,
    node_id: &NodeIdV1,
    policy: &TeePolicyV1,
) -> eyre::Result<LocalEnclaveIdentity> {
    let EnclaveResponse::Quote {
        recipient_x25519_pub,
        attestation_pub,
        noise_static_pub,
        ..
    } = quote
    else {
        return Err(eyre::eyre!(
            "GramineDirectDev session did not retain its enclave identity quote"
        ));
    };
    let mut enclave_keys = [0_u8; 96];
    enclave_keys[..32].copy_from_slice(recipient_x25519_pub);
    enclave_keys[32..64].copy_from_slice(attestation_pub);
    enclave_keys[64..].copy_from_slice(noise_static_pub);
    let enclave_id = {
        let mut preimage = Vec::with_capacity(ENCLAVE_ID_DOMAIN_V1.len() + 96);
        preimage.extend_from_slice(ENCLAVE_ID_DOMAIN_V1);
        preimage.extend_from_slice(&enclave_keys);
        keccak256(preimage)
    };
    let node_id_hash = node_id
        .node_id_hash()
        .map_err(|error| eyre::eyre!("invalid OST3 node identity: {error}"))?;
    let node_host_authorization_hash = {
        let mut preimage =
            Vec::with_capacity(OST3_DEV_NODE_HOST_DOMAIN.len() + 32 + 32 + 32 + enclave_keys.len());
        preimage.extend_from_slice(OST3_DEV_NODE_HOST_DOMAIN);
        preimage.extend_from_slice(&policy.chain_id);
        preimage.extend_from_slice(policy.genesis_hash.as_slice());
        preimage.extend_from_slice(node_id_hash.as_slice());
        preimage.extend_from_slice(&enclave_keys);
        keccak256(preimage)
    };
    Ok(LocalEnclaveIdentity {
        recipient_x25519: *recipient_x25519_pub,
        attestation_ed25519: *attestation_pub,
        noise_responder_x25519: *noise_static_pub,
        enclave_id,
        node_host_authorization_hash,
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
    sign_node_hash: &impl Fn(B256) -> Result<[u8; 65], String>,
) -> eyre::Result<SignedValidatorBinding> {
    let validator_binding = ValidatorNodeBindingV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        validator: evm_signer.address().into_array(),
        node_id_hash,
    };
    let binding_hash = validator_binding
        .binding_hash()
        .map_err(|error| eyre::eyre!("invalid validator NodeHost binding: {error}"))?;
    let validator_signature = evm_signer
        .sign_hash(&binding_hash)
        .map_err(|error| eyre::eyre!("cannot sign validator NodeHost binding: {error}"))?;
    let node_binding_signature = sign_node_hash(binding_hash)
        .map_err(|error| eyre::eyre!("cannot sign NodeHost validator binding: {error}"))?;
    Ok(SignedValidatorBinding {
        validator_binding,
        validator_signature,
        node_binding_signature,
    })
}

fn create_evidence(
    client: &mut RuntimeEnclaveClient,
    policy_mode: AttestationMode,
    intent: RegistrationIntentV1,
) -> eyre::Result<(AttestationEvidenceV1, [u8; 64])> {
    let attestation_ed25519 = intent.attestation_ed25519;
    let evidence_kind = bootstrap_evidence_kind(
        policy_mode,
        matches!(client, RuntimeEnclaveClient::Production(_)),
    )?;
    let (evidence, enclave_signature) = match (evidence_kind, client) {
        (BootstrapEvidenceKind::Dcap, RuntimeEnclaveClient::Production(production)) => {
            let generated = production
                .generate_dcap_quote(&intent)
                .map_err(|error| eyre::eyre!("production OST3 quote generation failed: {error}"))?;
            let components =
                acquire_dcap_collateral_v1(&generated.quote_body).map_err(|error| {
                    eyre::eyre!("production OST3 collateral acquisition failed: {error}")
                })?;
            let enclave_signature = generated.enclave_signature;
            (
                AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
                    intent,
                    quote: generated.quote_body,
                    components,
                    transition_key_ready_proof: generated.transition_key_ready_proof,
                }),
                enclave_signature,
            )
        }
        (BootstrapEvidenceKind::Dcap, RuntimeEnclaveClient::Development(_)) => {
            return Err(eyre::eyre!(
                "DcapRequired genesis policy cannot use a development enclave session"
            ));
        }
        (BootstrapEvidenceKind::GramineDirectDev, client) => {
            let enclave_signature = sign_dev_intent(client, &intent)?;
            (
                AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
                    transition_key_ready_proof: None,
                    intent,
                    dev_attestation_public: attestation_ed25519,
                    dev_signature: enclave_signature,
                }),
                enclave_signature,
            )
        }
    };
    Ok((evidence, enclave_signature))
}

fn sign_dev_intent(
    client: &mut RuntimeEnclaveClient,
    intent: &RegistrationIntentV1,
) -> eyre::Result<[u8; 64]> {
    match client {
        RuntimeEnclaveClient::Production(production) => production
            .sign_registration_intent_dev_v1(intent)
            .map_err(|error| {
                eyre::eyre!("production SGX-no-attest OST3 intent signing failed: {error}")
            }),
        RuntimeEnclaveClient::Development(development) => development
            .sign_registration_intent_dev_v1(intent)
            .map_err(|error| eyre::eyre!("development OST3 intent signing failed: {error}")),
    }
}
