//! Handle dkg requests requests after session admission.

use super::requests::RequestContext;
use crate::transport::*;

pub(super) fn dispatch(
    req: EnclaveRequest,
    context: RequestContext<'_>,
    dkg: &mut DkgSessionStore,
) -> EnclaveResponse {
    match req {
        EnclaveRequest::DkgParticipantAnnounceV1 {
            ceremony_id,
            round,
            participant_bls,
        } => announce(context, ceremony_id, round, participant_bls),
        EnclaveRequest::DkgOpen {
            ceremony_id,
            round,
            participants,
        } => open(
            context,
            dkg,
            DkgOpenRequest {
                ceremony_id,
                round,
                participants,
            },
        ),
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
            // recovered share. `DkgFinalizeTributeOffer` releases it.
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
        } => finalize_offer_key(
            context,
            dkg,
            FoundingKeyRequest {
                ceremony_id,
                sealed_partials,
                chain_id,
                tribute_offer_epoch,
            },
        ),
        _ => unreachable!("request family is checked by the exhaustive dispatcher"),
    }
}

struct DkgOpenRequest {
    ceremony_id: B256,
    round: u64,
    participants: Vec<outbe_tee::protocol::ParticipantAnnounce>,
}

struct FoundingKeyRequest {
    ceremony_id: B256,
    sealed_partials: Vec<Vec<u8>>,
    chain_id: B256,
    tribute_offer_epoch: u64,
}

fn announce(
    context: RequestContext<'_>,
    ceremony_id: B256,
    round: u64,
    participant_bls: Vec<Vec<u8>>,
) -> EnclaveResponse {
    let RequestContext {
        keys,
        initialization: context,
        ..
    } = context;
    let DispatchInitializationContext { initialization, .. } = context;
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
            outbe_primitives::tee_attestation_v1::dkg_participant_set_hash_v1(&participant_bls)
                .map_err(|error| {
                    crate::errors::TeeError::Dkg(format!("invalid DKG participant set: {error}"))
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
        let enc_sig =
            keys.sign_dkg_enc_binding(&network_binding, ceremony_id, round, participant_set_hash)?;
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

fn open(
    context: RequestContext<'_>,
    dkg: &mut DkgSessionStore,
    request: DkgOpenRequest,
) -> EnclaveResponse {
    let RequestContext {
        keys,
        initialization: context,
        ..
    } = context;
    let DispatchInitializationContext { initialization, .. } = context;
    let DkgOpenRequest {
        ceremony_id,
        round,
        participants,
    } = request;
    let network_binding = initialization
        .ok_or_else(|| "DkgOpen requires initialized state".to_string())
        .and_then(InitializationState::network_binding)
        .and_then(|binding| {
            binding.ok_or_else(|| "DkgOpen requires a committed network binding".to_string())
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

fn finalize_offer_key(
    context: RequestContext<'_>,
    dkg: &mut DkgSessionStore,
    request: FoundingKeyRequest,
) -> EnclaveResponse {
    let initialization = context.initialization.initialization;
    let FoundingKeyRequest {
        ceremony_id,
        sealed_partials,
        chain_id,
        tribute_offer_epoch,
    } = request;
    let expected_chain_id = initialization
        .ok_or_else(|| "DkgFinalizeTributeOffer requires initialized state".to_string())
        .and_then(InitializationState::network_binding)
        .and_then(|binding| {
            binding
                .map(|binding| B256::from(binding.chain_id))
                .ok_or_else(|| {
                    "DkgFinalizeTributeOffer requires a committed network binding".to_string()
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
    // in sealed restart state. Authorization already proved that this is
    // a keyless Validator. A ready enclave cannot enter this arm.
    let result = dkg.get_mut(&ceremony_id.0).and_then(|s| {
        // The group public KEY (constant term) is the public verification key
        // carried into the bootstrap payload for later reshare-endorsement
        // checks; capture it while the session is still resident.
        let group_public_key = s.group_public_key_bytes()?;
        let (secret, public, group_sig) =
            s.recover_tribute_offer_secret(&sealed_partials, chain_id, tribute_offer_epoch)?;
        Ok((secret, public, group_sig, group_public_key))
    });
    match result {
        Ok((secret, public, group_sig, group_public_key)) => {
            let derived = DerivedTributeOfferKey::from_parts(secret, public, group_sig)
                .with_epochs(0, tribute_offer_epoch);
            if let Err(message) = activate_founding_key(context, derived) {
                return EnclaveResponse::Error { message };
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

fn activate_founding_key(
    context: RequestContext<'_>,
    derived: DerivedTributeOfferKey,
) -> Result<(), String> {
    let initialization = context.initialization.initialization;
    if let Some(boot) = context.initialization.boot {
        let network_binding = initialization
            .ok_or_else(|| {
                "founding DKG persistence requires initialized production state".to_string()
            })
            .and_then(InitializationState::network_binding)
            .and_then(|binding| {
                binding.ok_or_else(|| {
                    "founding DKG persistence requires an initialization manifest".to_string()
                })
            })?;
        persist_then_activate_offer_key(boot, network_binding, context.offer_key, derived)?;
    } else {
        if initialization.map(InitializationState::mode)
            == Some(crate::initialization::InitializationMode::Production)
        {
            return Err("founding DKG requires durable production sealed storage".into());
        }
        // Hardware-free DKG tests have no durable store.
        if let Err(rejected) = context.offer_key.set(derived) {
            if context.offer_key.get().map(|key| key.public) != Some(rejected.public) {
                return Err(
                    "offer key divergence: recovered key differs from the resident offer key"
                        .into(),
                );
            }
        }
    }
    Ok(())
}
