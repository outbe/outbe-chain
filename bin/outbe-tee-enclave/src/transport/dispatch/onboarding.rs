use crate::transport::*;

pub(in crate::transport) fn complete_onboarding_artifact_ingest_response(
    request: CompleteOnboardingArtifactIngestV1,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    boot: Option<&EnclaveBootConfig>,
    initialization: &InitializationState,
) -> EnclaveResponse {
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
            derive_onboarding_offer_key_v1(
                keys,
                &manifest,
                &request.artifact,
                request.expected_intent_hash,
                request.expected_tribute_offer_public,
                request.expected_key_epoch,
                request.expected_tribute_offer_epoch,
            )
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

#[allow(clippy::too_many_arguments)]
pub(in crate::transport) fn complete_gramine_direct_dev_onboarding_ingest_response(
    artifact: &[u8],
    expected_intent_hash: B256,
    expected_tribute_offer_public: [u8; 32],
    expected_key_epoch: u64,
    expected_tribute_offer_epoch: u64,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    boot: Option<&EnclaveBootConfig>,
    initialization: Option<&InitializationState>,
) -> EnclaveResponse {
    let Some(initialization) = initialization else {
        return EnclaveResponse::Error {
            message: "GramineDirectDev onboarding requires production initialization".into(),
        };
    };
    let manifest = match initialization.manifest() {
        Ok(Some(manifest)) if manifest.attestation_mode == AttestationMode::GramineDirectDev => {
            manifest
        }
        Ok(Some(_)) => {
            return EnclaveResponse::Error {
                message: "GramineDirectDev onboarding is forbidden by the initialized network"
                    .into(),
            }
        }
        Ok(None) => {
            return EnclaveResponse::Error {
                message: "GramineDirectDev onboarding requires an initialization manifest".into(),
            }
        }
        Err(message) => return EnclaveResponse::Error { message },
    };
    let derived = match derive_onboarding_offer_key_v1(
        keys,
        &manifest,
        artifact,
        expected_intent_hash,
        expected_tribute_offer_public,
        expected_key_epoch,
        expected_tribute_offer_epoch,
    ) {
        Ok(derived) => derived,
        Err(error) => {
            return EnclaveResponse::Error {
                message: error.to_string(),
            }
        }
    };
    let Some(boot) = boot else {
        return EnclaveResponse::Error {
            message: "GramineDirectDev onboarding requires durable production sealed storage"
                .into(),
        };
    };
    let public = derived.public();
    if let Err(message) =
        persist_then_activate_offer_key(boot, manifest.network_binding(), offer_key, derived)
    {
        return EnclaveResponse::Error { message };
    }
    EnclaveResponse::GramineDirectDevOnboardingArtifactIngestedV1 {
        tribute_offer_public: public,
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::transport) fn derive_onboarding_offer_key_v1(
    keys: &EnclaveKeys,
    manifest: &EnclaveInitializationManifestV1,
    artifact: &[u8],
    expected_intent_hash: alloy_primitives::B256,
    expected_tribute_offer_public: [u8; 32],
    expected_key_epoch: u64,
    expected_tribute_offer_epoch: u64,
) -> crate::errors::Result<DerivedTributeOfferKey> {
    let artifact = outbe_tee::dcap_protocol::DcapOnboardingArtifactV1::decode_canonical(artifact)
        .map_err(|_| crate::errors::TeeError::DecryptFailed)?;
    let context = artifact.context;
    let manifest_node_id_hash = manifest
        .node_id
        .node_id_hash()
        .map_err(|error| crate::errors::TeeError::Dkg(error.to_string()))?;
    let manifest_enclave_id = manifest
        .enclave_id()
        .map_err(|error| crate::errors::TeeError::Dkg(error.to_string()))?;
    if context.chain_id != manifest.chain_id
        || context.genesis_hash != manifest.genesis_hash
        || context.intent_hash != expected_intent_hash
        || context.node_id_hash != manifest_node_id_hash
        || context.enclave_id != manifest_enclave_id
        || context.recipient_x25519 != manifest.recipient_x25519
        || context.recipient_x25519 != keys.tribute_offer_public()
        || context.tribute_offer_public != expected_tribute_offer_public
        || context.key_epoch != expected_key_epoch
        || context.tribute_offer_epoch != expected_tribute_offer_epoch
    {
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
