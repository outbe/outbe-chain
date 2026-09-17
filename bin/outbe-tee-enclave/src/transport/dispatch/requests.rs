use crate::transport::*;

/// Dispatch a post-handshake request to a response. `offer_key` is the shared
/// DKG-derived offer key slot: once Seam F populates it, the offer-decrypt path
/// and `GetPublicKeys` use it instead of the pre-DKG dev offer key.
pub fn dispatch(
    req: EnclaveRequest,
    keys: &EnclaveKeys,
    dkg: &mut DkgSessionStore,
    offer_key: &SharedTributeOfferKey,
    chain_id: alloy_primitives::B256,
) -> EnclaveResponse {
    dispatch_with_initialization(
        req,
        keys,
        dkg,
        offer_key,
        chain_id,
        DispatchInitializationContext {
            boot: None,
            initialization: None,
            quote_generator: crate::gramine::dcap_quote,
        },
    )
}

type QuoteGenerator = fn(&[u8; 64]) -> Result<Vec<u8>, String>;

type GeneratedDcapQuoteResponse = (Vec<u8>, Vec<u8>, Vec<u8>);

pub(in crate::transport) struct DispatchInitializationContext<'a> {
    pub(in crate::transport) boot: Option<&'a EnclaveBootConfig>,
    pub(in crate::transport) initialization: Option<&'a InitializationState>,
    pub(in crate::transport) quote_generator: QuoteGenerator,
}

pub(in crate::transport) fn dispatch_with_initialization(
    req: EnclaveRequest,
    keys: &EnclaveKeys,
    dkg: &mut DkgSessionStore,
    offer_key: &SharedTributeOfferKey,
    chain_id: alloy_primitives::B256,
    context: DispatchInitializationContext<'_>,
) -> EnclaveResponse {
    let DispatchInitializationContext {
        boot,
        initialization,
        quote_generator,
    } = context;
    match req {
        EnclaveRequest::ApplyPledgeLedger { request } => {
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error { message: "pledge ledger requires resident group key".into() };
            };
            let result = (|| -> Result<_, String> {
                if request.context.chain_id != chain_id { return Err("pledge ledger chain mismatch".into()); }
                let key = crate::gratis::derive_gratis_state_key(derived.group_sig(), chain_id, 0).map_err(|e| e.to_string())?;
                let mut response = crate::pledgenote::dispatch(&key, derived.secret(), &request)?;
                response.attestation = keys.sign_attestation(&outbe_tee::pledgenote::attestation_preimage(response.inputs_hash, &response.reply)?).to_vec();
                Ok(response)
            })();
            match result {
                Ok(response) => EnclaveResponse::PledgeLedger { response: Box::new(response) },
                Err(message) => EnclaveResponse::Error { message },
            }
        },
        EnclaveRequest::ReplayPledgeLedger { request } => {
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error { message: "pledge ledger requires resident group key".into() };
            };
            let result = (|| -> Result<_, String> {
                if request.chain_id != chain_id { return Err("pledge replay chain mismatch".into()); }
                let key = crate::gratis::derive_gratis_state_key(derived.group_sig(), chain_id, 0).map_err(|e| e.to_string())?;
                let mut response = crate::pledgenote::replay(&key, derived.secret(), &request)?;
                response.attestation = keys.sign_attestation(&outbe_tee::pledgenote::attestation_preimage(response.inputs_hash, &response.reply)?).to_vec();
                Ok(response)
            })();
            match result {
                Ok(response) => EnclaveResponse::PledgeLedger { response: Box::new(response) },
                Err(message) => EnclaveResponse::Error { message },
            }
        },
        EnclaveRequest::GetQuote { .. }
        | EnclaveRequest::GetInitializationChallenge
        | EnclaveRequest::Initialize { .. }
        | EnclaveRequest::OpenSession
        | EnclaveRequest::OpenRemoteSessionV1 { .. }
        | EnclaveRequest::SessionHandshake { .. } => EnclaveResponse::Error {
            message: "pre-handshake request is not valid inside a Noise session".to_string(),
        },
        EnclaveRequest::BeginDcapVerificationV1 { .. }
        | EnclaveRequest::BeginDcapOnboardingVerificationV1 { .. }
        | EnclaveRequest::DcapVerificationChunkV1 { .. }
        | EnclaveRequest::FinishDcapVerificationV1 { .. } => EnclaveResponse::Error {
            message: "DCAP verification requires an authenticated production session".to_string(),
        },
        EnclaveRequest::PrepareGramineDirectDevOnboardingArtifactV1 { .. } => {
            EnclaveResponse::Error {
                message: "GramineDirectDev artifact creation requires an authenticated production session"
                    .to_string(),
            }
        }
        EnclaveRequest::AuthorizeRemoteSessionV1 {
            ticket_id,
            initiator_static_x25519,
            responder_static_x25519,
            deadline,
            finalized_block_hash,
        } => {
            let Some(initialization) = initialization else {
                return EnclaveResponse::Error {
                    message: "remote session authorization requires production initialization"
                        .into(),
                };
            };
            match initialization.authorize_remote_session(
                ticket_id,
                initiator_static_x25519,
                responder_static_x25519,
                deadline,
                finalized_block_hash,
                keys,
            ) {
                Ok(()) => EnclaveResponse::RemoteSessionAuthorizedV1 { ticket_id },
                Err(message) => EnclaveResponse::Error { message },
            }
        }
        EnclaveRequest::Health => {
            let (
                requests_total,
                requests_errored,
                requests_denied,
                class_initialized,
                class_founding_keyless,
                class_keyless_onboarding,
                class_ready,
                class_dev_source_seal,
                class_dev_recipient_ingest,
            ) = crate::telemetry::counters_snapshot();
            let (heap_current_bytes, heap_peak_bytes) = crate::telemetry::heap_snapshot();
            EnclaveResponse::HealthStatus {
                status: Box::new(outbe_tee::protocol::EnclaveHealthStatusV1 {
                    uptime_s: crate::telemetry::uptime_s(),
                    offer_key_ready: offer_key.get().is_some(),
                    heap_current_bytes,
                    heap_peak_bytes,
                    requests_total,
                    requests_errored,
                    requests_denied,
                    class_initialized,
                    class_founding_keyless,
                    class_keyless_onboarding,
                    class_ready,
                    class_dev_source_seal,
                    class_dev_recipient_ingest,
                }),
            }
        }
        EnclaveRequest::GetPublicKeys => EnclaveResponse::PublicKeys {
            offer_key_ready: offer_key.get().is_some(),
            // Advertise the DKG-derived offer key once available, so clients
            // encrypt to it. Before readiness the same field carries only the
            // one-time onboarding recipient and is never permanent chain state.
            recipient_x25519_pub: offer_key
                .get()
                .map(|k| k.public())
                .unwrap_or_else(|| keys.tribute_offer_public()),
            attestation_pub: keys.attestation_pub(),
            noise_static_pub: keys.noise_public(),
            tee_bls_pub: keys.tee_bls_public_bytes(),
            dkg_enc_pub: keys.dkg_enc_public(),
            // A share-recipient key is trusted only through the scoped
            // DkgParticipantAnnounceV1 response below.
            dkg_enc_sig: Vec::new(),
        },
        EnclaveRequest::IngestGramineDirectDevOnboardingArtifactV1 {
            artifact,
            expected_intent_hash,
            expected_tribute_offer_public,
            expected_key_epoch,
            expected_tribute_offer_epoch,
        } => complete_gramine_direct_dev_onboarding_ingest_response(
            &artifact,
            expected_intent_hash,
            expected_tribute_offer_public,
            expected_key_epoch,
            expected_tribute_offer_epoch,
            keys,
            offer_key,
            context.boot,
            context.initialization,
        ),
        EnclaveRequest::GenerateDcapQuote { intent } => {
            let Some(initialization) = initialization else {
                return EnclaveResponse::Error {
                    message: "DCAP quote generation requires initialized production state"
                        .to_string(),
                };
            };
            let result = (|| -> Result<GeneratedDcapQuoteResponse, String> {
                let report_data = initialization.quote_report_data(&intent)?;
                let decoded_intent = RegistrationIntentV1::decode_canonical(&intent)
                    .map_err(|error| format!("registration intent is not canonical: {error}"))?;
                let transition_key_ready_proof = if decoded_intent.operation
                    == AttestationOperationV1::TransitionEnclaveMeasurement
                {
                    let resident_offer_public = offer_key
                        .get()
                        .ok_or_else(|| {
                            "transition quote generation requires the permanent offer key"
                                .to_string()
                        })?
                        .public();
                    let manifest = initialization
                        .manifest()?
                        .ok_or_else(|| "enclave is not initialized".to_string())?;
                    let mut proof = TransitionKeyReadyProofV1 {
                        chain_id: decoded_intent.chain_id,
                        genesis_hash: decoded_intent.genesis_hash,
                        transition_intent_hash: decoded_intent
                            .intent_hash()
                            .map_err(|error| error.to_string())?,
                        candidate_manifest_hash: manifest
                            .authorization_hash()
                            .map_err(|error| error.to_string())?,
                        transition_nonce: decoded_intent.transition_nonce,
                        resident_offer_public,
                        candidate_attestation_signature: [0_u8; 64],
                    };
                    let signing_hash = proof.signing_hash().map_err(|error| error.to_string())?;
                    proof.candidate_attestation_signature =
                        keys.sign_attestation(signing_hash.as_slice());
                    proof
                        .encode_canonical()
                        .map_err(|error| error.to_string())?
                } else {
                    Vec::new()
                };
                let quote = quote_generator(&report_data)?;
                let quote_body = validate_generated_quote_binding(report_data, quote)?;
                let enclave_signature = keys.sign_attestation(&report_data[..32]);
                Ok((
                    quote_body,
                    enclave_signature.to_vec(),
                    transition_key_ready_proof,
                ))
            })();
            match result {
                Ok((quote_body, enclave_signature, transition_key_ready_proof)) => {
                    EnclaveResponse::DcapQuote {
                        intent,
                        quote_body,
                        enclave_signature,
                        transition_key_ready_proof,
                    }
                }
                Err(message) => EnclaveResponse::Error { message },
            }
        }
        EnclaveRequest::SignRegistrationIntentDevV1 { intent } => {
            let Some(initialization) = initialization else {
                return EnclaveResponse::Error {
                    message: "development intent signing requires the development transport"
                        .to_string(),
                };
            };
            if !initialization.gramine_direct_dev_evidence_allowed() {
                return EnclaveResponse::Error {
                    message: "GramineDirectDev evidence requires development mode or production SGX without remote attestation"
                        .to_string(),
                };
            }
            let result = (|| -> Result<Vec<u8>, String> {
                let decoded = RegistrationIntentV1::decode_canonical(&intent)
                    .map_err(|error| error.to_string())?;
                if decoded.attestation_mode != AttestationMode::GramineDirectDev
                    || decoded.attestation_ed25519 != keys.attestation_pub()
                    || decoded
                        .derived_enclave_id()
                        .map_err(|error| error.to_string())?
                        != decoded.enclave_id
                {
                    return Err(
                        "GramineDirectDev intent does not bind this enclave identity".into(),
                    );
                }
                let intent_hash = decoded.intent_hash().map_err(|error| error.to_string())?;
                Ok(keys.sign_attestation(intent_hash.as_slice()).to_vec())
            })();
            match result {
                Ok(enclave_signature) => EnclaveResponse::RegistrationIntentSignedDevV1 {
                    intent,
                    enclave_signature,
                },
                Err(message) => EnclaveResponse::Error { message },
            }
        }
        EnclaveRequest::ProcessTributeOfferBatch { offers } => {
            let derived = offer_key.get();
            let km = match derived {
                Some(d) => keys.tribute_offer_key_material_with(d.secret()),
                None => keys.tribute_offer_key_material(),
            };
            let (results, inputs_canonical_hash) = process_tribute_offer_batch(&km, &offers);
            // Sign (inputs_canonical_hash || results) with the enclave's
            // Ed25519 attestation key. The host verifies this against the
            // attestation key it pinned from the quote, proving the results were
            // produced inside this attested enclave (not substituted by the host).
            let preimage = outbe_tee::protocol::tribute_offer_attestation_preimage(
                inputs_canonical_hash,
                &results,
            );
            let attestation_tag = keys.sign_attestation(&preimage).to_vec();
            EnclaveResponse::TributeOfferBatch {
                results,
                inputs_canonical_hash,
                attestation_tag,
            }
        }
        EnclaveRequest::ReservedGratisOp
        | EnclaveRequest::ReservedFidelityCohortOp
        | EnclaveRequest::ReservedFidelitySnapshot
        | EnclaveRequest::ReservedFidelityQuery => EnclaveResponse::Error {
            message: "retired ledger operation; use ApplyPledgeLedger".into(),
        },
        EnclaveRequest::ApplyPromisOp { request } => {
            // Promis retains its independent encrypted balance and key domain.
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "ApplyPromisOp: no resident group key (DKG not complete)".to_string(),
                };
            };
            let state_key =
                match crate::promis::derive_promis_state_key(derived.group_sig(), chain_id, 0) {
                    Ok(k) => k,
                    Err(e) => {
                        return EnclaveResponse::Error {
                            message: e.to_string(),
                        };
                    }
                };
            let mut result = crate::promis::apply_op(&state_key, &request);
            let preimage = outbe_tee::protocol::promis_op_attestation_preimage(
                result.inputs_canonical_hash,
                &result,
            );
            result.attestation_tag = keys.sign_attestation(&preimage).to_vec();
            EnclaveResponse::PromisOpApplied {
                result: Box::new(result),
            }
        }
        EnclaveRequest::DeriveAccountKeys {
            ledger,
            account,
            requester_ephemeral_pubkey,
            owner_sig,
        } => {
            // OFF-CHAIN key delivery only (served over RPC, never during block
            // execution): derive the account's view + modify keys and seal them to
            // the requester's ephemeral X25519 key.
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "DeriveAccountKeys: no resident group key (DKG not complete)"
                        .to_string(),
                };
            };
            // Prove the caller controls `account` INSIDE the enclave before releasing
            // its (secret) view/modify keys. The host RPC recovers the same signature
            // as a fast reject, but a compromised host reaches this transport directly,
            // so the release decision must run in the enclave's trust domain, not the
            // host's. Same shared preimage + recover_signer the host uses (one impl, no
            // divergence).
            let Ok(sig65) = <[u8; 65]>::try_from(owner_sig.as_slice()) else {
                return EnclaveResponse::Error {
                    message: "DeriveAccountKeys: owner signature must be 65 bytes".to_string(),
                };
            };
            let prehash = outbe_tee::protocol::eip191_hash(
                &outbe_tee::protocol::derive_account_keys_message(
                    ledger,
                    account,
                    alloy_primitives::B256::from(requester_ephemeral_pubkey),
                ),
            );
            match outbe_primitives::tee_signatures::recover_signer(&prehash, &sig65) {
                Ok(signer) if signer == account => {}
                _ => {
                    return EnclaveResponse::Error {
                        message: "DeriveAccountKeys: owner signature does not control account"
                            .to_string(),
                    };
                }
            }
            let sealed = (|| -> crate::errors::Result<crate::crypto::EncryptedShare> {
                let domain = crate::confidential::domain_for(ledger);
                let state_key = domain.derive_state_key(derived.group_sig(), chain_id, 0)?;
                let view_key = domain.derive_view_key(&state_key, account)?;
                let modify_key = domain.derive_modify_key(&state_key, account)?;
                let mut plaintext = view_key.to_vec();
                plaintext.extend_from_slice(&modify_key);
                crate::crypto::encrypt_share(&requester_ephemeral_pubkey, &plaintext)
            })();
            match sealed {
                Ok(blob) => EnclaveResponse::AccountKeysSealed {
                    account,
                    sealed: blob.ciphertext,
                    nonce: blob.nonce,
                    enclave_ephemeral_pubkey: blob.ephemeral_pub,
                },
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EnclaveRequest::BeginDcapOnboardingArtifactIngestV1 { .. }
        | EnclaveRequest::DcapOnboardingArtifactChunkV1 { .. }
        | EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 { .. }
        | EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 { .. } => EnclaveResponse::Error {
            message: "onboarding artifact ingest requires an authenticated production session"
                .into(),
        },
        EnclaveRequest::DkgParticipantAnnounceV1 {
            ceremony_id,
            round,
            participant_bls,
        } => {
            let result = (|| {
                let network_binding = initialization
                    .ok_or_else(|| {
                        crate::errors::TeeError::Dkg(
                            "DKG announcement requires initialized state".to_string(),
                        )
                    })?
                    .network_binding()
                    .map_err(crate::errors::TeeError::Dkg)?
                    .ok_or_else(|| {
                        crate::errors::TeeError::Dkg(
                            "DKG announcement requires a committed network binding".to_string(),
                        )
                    })?;
                let participant_set_hash =
                    outbe_primitives::tee_attestation_v1::dkg_participant_set_hash_v1(
                        &participant_bls,
                    )
                    .map_err(|error| {
                        crate::errors::TeeError::Dkg(format!(
                            "invalid DKG participant set: {error}"
                        ))
                    })?;
                let expected_ceremony = outbe_primitives::tee_attestation_v1::dkg_ceremony_id_v1(
                    &network_binding,
                    round,
                    participant_set_hash,
                )
                .map_err(|error| {
                    crate::errors::TeeError::Dkg(format!("invalid DKG ceremony context: {error}"))
                })?;
                if ceremony_id != expected_ceremony {
                    return Err(crate::errors::TeeError::Dkg(
                        "DKG announcement ceremony id does not match network and participant set"
                            .to_string(),
                    ));
                }
                let bls_pub = keys.tee_bls_public_bytes();
                if !participant_bls
                    .iter()
                    .any(|candidate| candidate == &bls_pub)
                {
                    return Err(crate::errors::TeeError::Dkg(
                        "local DKG identity is absent from participant set".to_string(),
                    ));
                }
                let enc_pub = keys.dkg_enc_public();
                let enc_sig = keys.sign_dkg_enc_binding(
                    &network_binding,
                    ceremony_id,
                    round,
                    participant_set_hash,
                )?;
                Ok(EnclaveResponse::DkgParticipantAnnounceV1 {
                    participant: outbe_tee::protocol::ParticipantAnnounce {
                        bls_pub,
                        enc_pub,
                        ceremony_id,
                        round,
                        participant_set_hash,
                        enc_sig,
                    },
                })
            })();
            into_response(result)
        }
        EnclaveRequest::DkgOpen {
            ceremony_id,
            round,
            participants,
        } => {
            let network_binding = initialization
                .ok_or_else(|| "DkgOpen requires initialized state".to_string())
                .and_then(InitializationState::network_binding)
                .and_then(|binding| {
                    binding
                        .ok_or_else(|| "DkgOpen requires a committed network binding".to_string())
                });
            match network_binding {
                Ok(network_binding) => dispatch_dkg_open(
                    keys,
                    dkg,
                    &network_binding,
                    ceremony_id,
                    round,
                    participants,
                ),
                Err(message) => EnclaveResponse::Error { message },
            }
        }
        EnclaveRequest::DkgStartDealer { ceremony_id } => {
            into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
                let (pub_msg, sealed_shares) = s.start_dealer_encoded()?;
                Ok(EnclaveResponse::DkgDealt {
                    pub_msg,
                    sealed_shares,
                })
            }))
        }
        EnclaveRequest::DkgPlayerIngest {
            ceremony_id,
            dealer_bls,
            pub_msg,
            sealed_share,
        } => into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
            let ack = s.player_ingest_encoded(&dealer_bls, &pub_msg, &sealed_share)?;
            Ok(EnclaveResponse::DkgPlayerAck { ack })
        })),
        EnclaveRequest::DkgDealerReceiveAck {
            ceremony_id,
            player_bls,
            ack,
        } => into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
            s.dealer_receive_ack_encoded(&player_bls, &ack)?;
            Ok(EnclaveResponse::Ack)
        })),
        EnclaveRequest::DkgDealerFinalize { ceremony_id } => {
            into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
                Ok(EnclaveResponse::DkgSignedLog {
                    signed_log: s.dealer_finalize_encoded()?,
                })
            }))
        }
        EnclaveRequest::DkgPlayerFinalize {
            ceremony_id,
            signed_logs,
        } => {
            // The session stays resident after finalize: Seam F (below) needs the
            // recovered share. It is released by `DkgFinalizeTributeOffer`.
            into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
                let (group_public, share_commitment) = s.player_finalize_encoded(&signed_logs)?;
                Ok(EnclaveResponse::DkgPlayerFinalized {
                    group_public,
                    share_commitment,
                })
            }))
        }
        EnclaveRequest::DkgTributeOfferPartial { ceremony_id } => {
            into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
                let sealed = s
                    .tribute_offer_partials_sealed()?
                    .into_iter()
                    .map(|(pk, blob)| {
                        (
                            commonware_codec::Encode::encode(&pk).to_vec(),
                            blob.to_bytes(),
                        )
                    })
                    .collect();
                Ok(EnclaveResponse::DkgTributeOfferPartial { sealed })
            }))
        }
        EnclaveRequest::DkgFinalizeTributeOffer {
            ceremony_id,
            sealed_partials,
            chain_id,
            tribute_offer_epoch,
        } => {
            let expected_chain_id = initialization
                .ok_or_else(|| "DkgFinalizeTributeOffer requires initialized state".to_string())
                .and_then(InitializationState::network_binding)
                .and_then(|binding| {
                    binding
                        .map(|binding| B256::from(binding.chain_id))
                        .ok_or_else(|| {
                            "DkgFinalizeTributeOffer requires a committed network binding"
                                .to_string()
                        })
                });
            let Ok(expected_chain_id) = expected_chain_id else {
                return EnclaveResponse::Error {
                    message: expected_chain_id.unwrap_err(),
                };
            };
            if chain_id != expected_chain_id {
                return EnclaveResponse::Error {
                    message: "DkgFinalizeTributeOffer chain id does not match initialized network"
                        .to_string(),
                };
            }
            // Complete founding Seam F and retain the group threshold signature
            // in sealed restart state. Authorization has already proved this is
            // a keyless Validator; a ready enclave cannot enter this arm.
            let result = dkg.get_mut(&ceremony_id.0).and_then(|s| {
                // The group public KEY (constant term) is the public verification key
                // carried into the bootstrap payload for later reshare-endorsement
                // checks; capture it while the session is still resident.
                let group_public_key = s.group_public_key_bytes()?;
                let (secret, public, group_sig) = s.recover_tribute_offer_secret(
                    &sealed_partials,
                    chain_id,
                    tribute_offer_epoch,
                )?;
                Ok((secret, public, group_sig, group_public_key))
            });
            match result {
                Ok((secret, public, group_sig, group_public_key)) => {
                    let derived = DerivedTributeOfferKey::from_parts(secret, public, group_sig)
                        .with_epochs(0, tribute_offer_epoch);
                    if let Some(boot) = boot {
                        let network_binding = match initialization
                            .ok_or_else(|| {
                                "founding DKG persistence requires initialized production state"
                                    .to_string()
                            })
                            .and_then(InitializationState::network_binding)
                            .and_then(|binding| {
                                binding.ok_or_else(|| {
                                    "founding DKG persistence requires an initialization manifest"
                                        .to_string()
                                })
                            }) {
                            Ok(binding) => binding,
                            Err(message) => return EnclaveResponse::Error { message },
                        };
                        if let Err(message) = persist_then_activate_offer_key(
                            boot,
                            network_binding,
                            offer_key,
                            derived,
                        ) {
                            return EnclaveResponse::Error { message };
                        }
                    } else {
                        if initialization.map(InitializationState::mode)
                            == Some(crate::initialization::InitializationMode::Production)
                        {
                            return EnclaveResponse::Error {
                                message: "founding DKG requires durable production sealed storage"
                                    .into(),
                            };
                        }
                        // Hardware-free DKG tests have no durable store.
                        if let Err(rejected) = offer_key.set(derived) {
                            if offer_key.get().map(|key| key.public) != Some(rejected.public) {
                                return EnclaveResponse::Error {
                                    message: "offer key divergence: recovered key differs from \
                                              the resident offer key"
                                        .to_string(),
                                };
                            }
                        }
                    }
                    // Release the ceremony's resident secret state.
                    dkg.remove(&ceremony_id.0);
                    EnclaveResponse::DkgTributeOfferKey {
                        tribute_offer_public: public,
                        group_public_key,
                    }
                }
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
    }
}

/// Map a seam `Result` into an `EnclaveResponse`, turning errors into the typed
/// `Error` response the host surfaces (never a panic).
pub(in crate::transport) fn into_response(
    result: crate::errors::Result<EnclaveResponse>,
) -> EnclaveResponse {
    result.unwrap_or_else(|e| EnclaveResponse::Error {
        message: e.to_string(),
    })
}
