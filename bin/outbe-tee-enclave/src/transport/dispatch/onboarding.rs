use crate::transport::*;

pub(in crate::transport) struct OnboardingArtifactExpectations<'a> {
    pub artifact: &'a [u8],
    pub expected_intent_hash: B256,
    pub expected_tribute_offer_public: [u8; 32],
    pub expected_key_epoch: u64,
    pub expected_tribute_offer_epoch: u64,
}

pub(in crate::transport) fn complete_onboarding_artifact_ingest_response(
    request: CompleteOnboardingArtifactIngestV1,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    boot: Option<&EnclaveBootConfig>,
    initialization: &InitializationState,
) -> EnclaveResponse {
    if request.upgrade_export == Some(true) {
        return complete_upgrade_key_export(request, offer_key, initialization);
    }
    let derived = initialization
        .manifest()
        .and_then(|manifest| {
            manifest.ok_or_else(|| "onboarding ingest requires an initialization manifest".into())
        })
        .map_err(crate::errors::TeeError::Dkg)
        .and_then(|manifest| {
            if request.verified_admission.block_number == 0
                || request.verified_admission.block_hash.is_zero()
            {
                return Err(crate::errors::TeeError::Dkg(
                    "onboarding ingest lost its verified admission anchor".into(),
                ));
            }
            let expected = OnboardingArtifactExpectations {
                artifact: &request.artifact,
                expected_intent_hash: request.expected_intent_hash,
                expected_tribute_offer_public: request.expected_tribute_offer_public,
                expected_key_epoch: request.expected_key_epoch,
                expected_tribute_offer_epoch: request.expected_tribute_offer_epoch,
            };
            derive_onboarding_offer_key_v1(keys, &manifest, &expected)
                .map(|derived| (derived, manifest.network_binding()))
        });
    let (derived, network_binding) = match derived {
        Ok(value) => value,
        Err(error) => {
            return EnclaveResponse::Error {
                message: error.to_string(),
            }
        }
    };
    let Some(boot) = boot else {
        return EnclaveResponse::Error {
            message: "onboarding ingest requires durable production sealed storage".into(),
        };
    };
    let public = derived.public();
    if let Err(error) = persist_then_activate_offer_key(boot, network_binding, offer_key, derived) {
        return EnclaveResponse::Error { message: error };
    }
    EnclaveResponse::FinalizedAdmissionIngestedV1 {
        request_hash: request.request_hash,
        tribute_offer_public: public,
    }
}

fn complete_upgrade_key_export(
    request: CompleteOnboardingArtifactIngestV1,
    offer_key: &SharedTributeOfferKey,
    initialization: &InitializationState,
) -> EnclaveResponse {
    let result = (|| -> Result<Vec<u8>, String> {
        let context =
            outbe_tee::dcap_protocol::DcapOnboardingArtifactV1::decode_canonical(&request.artifact)
                .map_err(|_| "invalid upgrade export context")?
                .context;
        let manifest = initialization
            .manifest()?
            .ok_or("source is not initialized")?;
        if manifest.chain_id != context.chain_id
            || manifest.genesis_hash != context.genesis_hash
            || request.verified_admission.block_number == 0
            || request.verified_admission.block_hash.is_zero()
        {
            return Err("upgrade export lost its verified network authorization".into());
        }
        let resident = offer_key.get().ok_or("source key is unavailable")?;
        if resident.public() != context.tribute_offer_public
            || resident.key_epoch() != context.key_epoch
            || resident.tribute_offer_epoch() != context.tribute_offer_epoch
        {
            return Err("source key does not match the finalized network commitment".into());
        }
        crate::crypto::encrypt_onboarding_artifact_v1(
            resident.secret(),
            context,
            resident.group_sig(),
        )
        .map_err(|e| e.to_string())?
        .encode_canonical()
        .map_err(|e| format!("artifact encoding: {e:?}"))
    })();
    match result {
        Ok(artifact) => EnclaveResponse::UpgradeKeyExportedV1 {
            request_hash: request.request_hash,
            artifact,
        },
        Err(message) => EnclaveResponse::Error { message },
    }
}

pub(in crate::transport) fn complete_gramine_direct_dev_onboarding_ingest_response(
    expected: OnboardingArtifactExpectations<'_>,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    boot: Option<&EnclaveBootConfig>,
    initialization: Option<&InitializationState>,
) -> EnclaveResponse {
    let result = (|| -> Result<[u8; 32], String> {
        let initialization = initialization.ok_or_else(|| {
            "GramineDirectDev onboarding requires production initialization".to_string()
        })?;
        let manifest = initialization.manifest()?.ok_or_else(|| {
            "GramineDirectDev onboarding requires an initialization manifest".to_string()
        })?;
        if manifest.attestation_mode != AttestationMode::GramineDirectDev {
            return Err(
                "GramineDirectDev onboarding is forbidden by the initialized network".to_string(),
            );
        }
        let derived = derive_onboarding_offer_key_v1(keys, &manifest, &expected)
            .map_err(|error| error.to_string())?;
        let boot = boot.ok_or_else(|| {
            "GramineDirectDev onboarding requires durable production sealed storage".to_string()
        })?;
        let public = derived.public();
        persist_then_activate_offer_key(boot, manifest.network_binding(), offer_key, derived)?;
        Ok(public)
    })();
    match result {
        Ok(tribute_offer_public) => EnclaveResponse::GramineDirectDevOnboardingArtifactIngestedV1 {
            tribute_offer_public,
        },
        Err(message) => EnclaveResponse::Error { message },
    }
}

pub(in crate::transport) fn derive_onboarding_offer_key_v1(
    keys: &EnclaveKeys,
    manifest: &EnclaveInitializationManifestV1,
    expected: &OnboardingArtifactExpectations<'_>,
) -> crate::errors::Result<DerivedTributeOfferKey> {
    let artifact =
        outbe_tee::dcap_protocol::DcapOnboardingArtifactV1::decode_canonical(expected.artifact)
            .map_err(|_| crate::errors::TeeError::DecryptFailed)?;
    let context = artifact.context;
    if !onboarding_context_matches(&context, keys, manifest, expected)? {
        return Err(crate::errors::TeeError::Dkg(
            "onboarding artifact does not match initialized identity or finalized Registry context"
                .into(),
        ));
    }
    let group_sig = crate::crypto::decrypt_onboarding_artifact_v1(
        keys.tribute_offer_x25519_secret(),
        &artifact,
    )?;
    let (secret, public) = crate::crypto::derive_tribute_offer_secret_from_group_sig(
        group_sig.as_ref(),
        alloy_primitives::B256::from(context.chain_id),
        context.tribute_offer_epoch,
    )?;
    if public != context.tribute_offer_public {
        return Err(crate::errors::TeeError::Dkg(
            "onboarding artifact derives a different offer public key".into(),
        ));
    }
    Ok(
        DerivedTributeOfferKey::from_parts(secret, public, group_sig)
            .with_epochs(context.key_epoch, context.tribute_offer_epoch),
    )
}

fn onboarding_context_matches(
    context: &outbe_tee::dcap_protocol::DcapOnboardingContextV1,
    keys: &EnclaveKeys,
    manifest: &EnclaveInitializationManifestV1,
    expected: &OnboardingArtifactExpectations<'_>,
) -> crate::errors::Result<bool> {
    let manifest_node_id_hash = manifest
        .node_id
        .node_id_hash()
        .map_err(|error| crate::errors::TeeError::Dkg(error.to_string()))?;
    let manifest_enclave_id = manifest
        .enclave_id()
        .map_err(|error| crate::errors::TeeError::Dkg(error.to_string()))?;
    let matches_network = context.chain_id == manifest.chain_id
        && context.genesis_hash == manifest.genesis_hash
        && context.intent_hash == expected.expected_intent_hash;
    let matches_node =
        context.node_id_hash == manifest_node_id_hash && context.enclave_id == manifest_enclave_id;
    let matches_recipient = context.recipient_x25519 == manifest.recipient_x25519
        && context.recipient_x25519 == keys.tribute_offer_public();
    let matches_offer = context.tribute_offer_public == expected.expected_tribute_offer_public
        && context.key_epoch == expected.expected_key_epoch
        && context.tribute_offer_epoch == expected.expected_tribute_offer_epoch;
    Ok(matches_network && matches_node && matches_recipient && matches_offer)
}
