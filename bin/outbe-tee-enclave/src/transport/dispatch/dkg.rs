use crate::transport::*;

pub(in crate::transport) fn dispatch_dkg_open(
    keys: &EnclaveKeys,
    dkg: &mut DkgSessionStore,
    network_binding: &outbe_primitives::tee_attestation_v1::NetworkBindingV1,
    ceremony_id: alloy_primitives::B256,
    round: u64,
    participants: Vec<outbe_tee::protocol::ParticipantAnnounce>,
) -> EnclaveResponse {
    let result = (|| {
        // The host relays each `(bls, enc, sig)` it gathered from peers' GetPublicKeys.
        // Before trusting any pairing: verify every enc key is signed by the BLS
        // identity it is paired with, and reject duplicate enc keys / identities - so
        // an untrusted host cannot mis-pair an enc key onto a foreign identity or
        // collapse two participants onto one enc key (cross-decryption of shares).
        let mut enc_by_bls = std::collections::BTreeMap::new();
        let mut seen_enc = std::collections::BTreeSet::new();
        let participant_bls: Vec<Vec<u8>> =
            participants.iter().map(|p| p.bls_pub.clone()).collect();
        let participant_set_hash =
            outbe_primitives::tee_attestation_v1::dkg_participant_set_hash_v1(&participant_bls)
                .map_err(|error| {
                    crate::errors::TeeError::Dkg(format!(
                        "DkgOpen: invalid participant set: {error}"
                    ))
                })?;
        let expected_ceremony = outbe_primitives::tee_attestation_v1::dkg_ceremony_id_v1(
            network_binding,
            round,
            participant_set_hash,
        )
        .map_err(|error| {
            crate::errors::TeeError::Dkg(format!("DkgOpen: invalid ceremony: {error}"))
        })?;
        if ceremony_id != expected_ceremony {
            return Err(crate::errors::TeeError::Dkg(
                "DkgOpen: ceremony id does not match network and participant set".to_string(),
            ));
        }
        for p in &participants {
            if p.ceremony_id != ceremony_id
                || p.round != round
                || p.participant_set_hash != participant_set_hash
                || !crate::keys::verify_dkg_enc_binding(
                    &p.bls_pub,
                    network_binding,
                    ceremony_id,
                    round,
                    participant_set_hash,
                    &p.enc_pub,
                    &p.enc_sig,
                )
            {
                return Err(crate::errors::TeeError::Dkg(
                    "DkgOpen: enc-key identity binding failed verification".to_string(),
                ));
            }
            if !seen_enc.insert(p.enc_pub) {
                return Err(crate::errors::TeeError::Dkg(
                    "DkgOpen: duplicate enc key across participants".to_string(),
                ));
            }
            if enc_by_bls.insert(p.bls_pub.clone(), p.enc_pub).is_some() {
                return Err(crate::errors::TeeError::Dkg(
                    "DkgOpen: duplicate BLS identity across participants".to_string(),
                ));
            }
        }
        let (info, pubkeys) = build_ceremony_info(round, &participant_bls)?;
        let mut recipient_enc_keys = std::collections::BTreeMap::new();
        for pk in &pubkeys {
            let bls_bytes = commonware_codec::Encode::encode(pk).to_vec();
            let enc = enc_by_bls.get(&bls_bytes).ok_or_else(|| {
                crate::errors::TeeError::Dkg("DkgOpen: enc key missing for participant".to_string())
            })?;
            recipient_enc_keys.insert(pk.clone(), *enc);
        }
        dkg.open(
            ceremony_id.0,
            info,
            keys.tee_bls_key().clone(),
            keys.dkg_enc_secret(),
            recipient_enc_keys,
        )?;
        Ok(EnclaveResponse::Ack)
    })();
    into_response(result)
}
