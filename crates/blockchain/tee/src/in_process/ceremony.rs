//! Purpose-bound founding ceremony transport; no secret leaves the resident owner.
use super::{InProcessError, InProcessTeeContext, Result};
use crate::{
    protocol::{EnclaveRequest, EnclaveResponse, ParticipantAnnounce},
    tee_dkg::EnclaveChannel,
};
use alloy_primitives::B256;
use commonware_codec::{Encode as _, ReadExt as _};
use commonware_cryptography::{bls12381, Signer as _, Verifier as _};
use outbe_primitives::tee_attestation_v1::*;
use std::collections::{BTreeMap, BTreeSet};

const ANNOUNCE_NAMESPACE: &[u8] = b"outbe/tee/dkg-participant-announce/v1";

impl InProcessTeeContext {
    fn ceremony_binding(&self, id: B256, round: u64, participants: &[Vec<u8>]) -> Result<B256> {
        let hash = dkg_participant_set_hash_v1(participants)?;
        if id != dkg_ceremony_id_v1(&self.manifest.network_binding(), round, hash)?
            || !participants.contains(&self.tee_bls_public_bytes())
        {
            return Err(InProcessError::IdentityMismatch);
        }
        Ok(hash)
    }

    /// Sign only the exact local encryption key in a network-bound participant set.
    pub fn founding_announcement(
        &self,
        id: B256,
        round: u64,
        participants: &[Vec<u8>],
    ) -> Result<ParticipantAnnounce> {
        let hash = self.ceremony_binding(id, round, participants)?;
        let digest = dkg_participant_announce_hash_v1(
            &self.manifest.network_binding(),
            id,
            round,
            hash,
            &self.dkg_enc_public(),
        )?;
        Ok(ParticipantAnnounce {
            bls_pub: self.tee_bls_public_bytes(),
            enc_pub: self.dkg_enc_public(),
            ceremony_id: id,
            round,
            participant_set_hash: hash,
            enc_sig: self
                .keys
                .tee_bls_key
                .sign(ANNOUNCE_NAMESPACE, digest.as_slice())
                .encode()
                .to_vec(),
        })
    }

    fn open_founding(
        &self,
        id: B256,
        round: u64,
        participants: &[ParticipantAnnounce],
    ) -> Result<()> {
        if self.offer_key_ready()? {
            return Err(InProcessError::ActivationConflict);
        }
        let bls: Vec<_> = participants.iter().map(|p| p.bls_pub.clone()).collect();
        let hash = self.ceremony_binding(id, round, &bls)?;
        let mut keys = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for p in participants {
            let mut pk_bytes = p.bls_pub.as_slice();
            let pk = bls12381::PublicKey::read(&mut pk_bytes)?;
            let mut sig_bytes = p.enc_sig.as_slice();
            let sig = <bls12381::PrivateKey as commonware_cryptography::Signer>::Signature::read(
                &mut sig_bytes,
            )?;
            let digest = dkg_participant_announce_hash_v1(
                &self.manifest.network_binding(),
                id,
                round,
                hash,
                &p.enc_pub,
            )?;
            let context_matches =
                (p.ceremony_id, p.round, p.participant_set_hash) == (id, round, hash);
            let canonical = pk_bytes.is_empty() && sig_bytes.is_empty();
            if !canonical
                || !context_matches
                || !pk.verify(ANNOUNCE_NAMESPACE, digest.as_slice(), &sig)
            {
                return Err(InProcessError::IdentityMismatch);
            }
            if !seen.insert(p.enc_pub) || keys.insert(pk, p.enc_pub).is_some() {
                return Err(InProcessError::IdentityMismatch);
            }
        }
        self.open_dkg_session(id.0, round, &bls, keys)
    }

    /// Sign only a canonical development intent bound to this initialized manifest.
    pub fn sign_registration_intent_dev_v1(
        &self,
        intent: &RegistrationIntentV1,
    ) -> Result<[u8; 64]> {
        self.manifest.validate_intent_binding(intent)?;
        if self.manifest.attestation_mode != AttestationMode::NoAttestationDev {
            return Err(InProcessError::IdentityMismatch);
        }
        Ok(self.sign_domain_attestation(intent.intent_hash()?.as_slice()))
    }

    /// Obtain hardware evidence for an exact manifest-bound intent, never synthetic evidence.
    pub fn generate_dcap_quote(
        &self,
        intent: &RegistrationIntentV1,
    ) -> Result<crate::GeneratedDcapQuoteV1> {
        self.manifest.validate_intent_binding(intent)?;
        if self.manifest.attestation_mode != AttestationMode::DcapRequired {
            return Err(InProcessError::IdentityMismatch);
        }
        let report_data = intent.report_data()?;
        let quote_body = self.proof_source.quote(report_data)?;
        let parsed = crate::quote::parse_quote_measurements(&quote_body)?;
        if parsed.report_data != report_data {
            return Err(InProcessError::IdentityMismatch);
        }
        let proof = if intent.operation == AttestationOperationV1::TransitionEnclaveMeasurement {
            Some(self.transition_key_ready_proof_v1(intent)?)
        } else {
            None
        };
        Ok(crate::GeneratedDcapQuoteV1 {
            quote_body,
            enclave_signature: self.sign_domain_attestation(intent.intent_hash()?.as_slice()),
            transition_key_ready_proof: proof,
        })
    }

    fn founding_request(&self, req: &EnclaveRequest) -> Result<EnclaveResponse> {
        match req {
            EnclaveRequest::GetPublicKeys => Ok(EnclaveResponse::PublicKeys {
                offer_key_ready: self.offer_key_ready()?,
                recipient_x25519_pub: if self.offer_key_ready()? {
                    self.network_encryption_public()?
                } else {
                    self.recipient_public()
                },
                attestation_pub: self.attestation_public(),
                noise_static_pub: self.noise_public(),
                tee_bls_pub: self.tee_bls_public_bytes(),
                dkg_enc_pub: self.dkg_enc_public(),
                dkg_enc_sig: Vec::new(),
            }),
            EnclaveRequest::DkgParticipantAnnounceV1 {
                ceremony_id,
                round,
                participant_bls,
            } => Ok(EnclaveResponse::DkgParticipantAnnounceV1 {
                participant: self.founding_announcement(*ceremony_id, *round, participant_bls)?,
            }),
            EnclaveRequest::DkgOpen {
                ceremony_id,
                round,
                participants,
            } => {
                self.open_founding(*ceremony_id, *round, participants)?;
                Ok(EnclaveResponse::Ack)
            }
            EnclaveRequest::DkgStartDealer { ceremony_id } => {
                self.with_dkg_session(&ceremony_id.0, |s| {
                    let (pub_msg, sealed_shares) = s.start_dealer_encoded()?;
                    Ok(EnclaveResponse::DkgDealt {
                        pub_msg,
                        sealed_shares,
                    })
                })
            }
            EnclaveRequest::DkgPlayerIngest {
                ceremony_id,
                dealer_bls,
                pub_msg,
                sealed_share,
            } => self.with_dkg_session(&ceremony_id.0, |s| {
                Ok(EnclaveResponse::DkgPlayerAck {
                    ack: s.player_ingest_encoded(dealer_bls, pub_msg, sealed_share)?,
                })
            }),
            EnclaveRequest::DkgDealerReceiveAck {
                ceremony_id,
                player_bls,
                ack,
            } => self.with_dkg_session(&ceremony_id.0, |s| {
                s.dealer_receive_ack_encoded(player_bls, ack)?;
                Ok(EnclaveResponse::Ack)
            }),
            EnclaveRequest::DkgDealerFinalize { ceremony_id } => {
                self.with_dkg_session(&ceremony_id.0, |s| {
                    Ok(EnclaveResponse::DkgSignedLog {
                        signed_log: s.dealer_finalize_encoded()?,
                    })
                })
            }
            EnclaveRequest::DkgPlayerFinalize {
                ceremony_id,
                signed_logs,
            } => self.with_dkg_session(&ceremony_id.0, |s| {
                let (group_public, share_commitment) = s.player_finalize_encoded(signed_logs)?;
                Ok(EnclaveResponse::DkgPlayerFinalized {
                    group_public,
                    share_commitment,
                })
            }),
            EnclaveRequest::DkgNetworkKeyPartial { ceremony_id } => {
                self.with_dkg_session(&ceremony_id.0, |s| {
                    Ok(EnclaveResponse::DkgNetworkKeyPartial {
                        sealed: s
                            .tribute_offer_partials_sealed()?
                            .into_iter()
                            .map(|(pk, blob)| (pk.encode().to_vec(), blob.to_wire_bytes()))
                            .collect(),
                    })
                })
            }
            EnclaveRequest::DkgFinalizeNetworkKey {
                ceremony_id,
                sealed_partials,
                chain_id,
                network_key_epoch,
            } => {
                if *chain_id != B256::from(self.manifest.chain_id) {
                    return Err(InProcessError::IdentityMismatch);
                }
                let group_public_key =
                    self.with_dkg_session(&ceremony_id.0, |s| s.group_public_key_bytes())?;
                let network_encryption_public_key = self.complete_founding_dkg(
                    &ceremony_id.0,
                    sealed_partials,
                    *network_key_epoch,
                )?;
                Ok(EnclaveResponse::DkgNetworkEncryptionKey {
                    network_encryption_public_key,
                    group_public_key,
                })
            }
            _ => Err(InProcessError::IdentityMismatch),
        }
    }
}

impl EnclaveChannel for &InProcessTeeContext {
    fn request(
        &mut self,
        req: &EnclaveRequest,
    ) -> std::result::Result<EnclaveResponse, crate::TransportError> {
        self.founding_request(req)
            .map_err(|error| crate::InProcessDispatchError::from(error).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_process::{test_support, HardwareProofSourceV1, ProofSourceV1};
    use std::sync::Arc;

    #[test]
    fn announcement_and_open_enforce_ceremony_and_signed_recipient_binding() {
        let context =
            test_support::context(AttestationMode::NoAttestationDev, ProofSourceV1::NoHardware);
        let participants = vec![context.tee_bls_public_bytes()];
        let hash = dkg_participant_set_hash_v1(&participants).unwrap();
        let id = dkg_ceremony_id_v1(&context.manifest.network_binding(), 0, hash).unwrap();
        assert!(context
            .founding_announcement(B256::ZERO, 0, &participants)
            .is_err());
        let announcement = context.founding_announcement(id, 0, &participants).unwrap();
        let mut forged = announcement.clone();
        forged.enc_pub[0] ^= 1;
        assert!(context.open_founding(id, 0, &[forged]).is_err());
        context.open_founding(id, 0, &[announcement]).unwrap();
        assert!(context.open_founding(id, 0, &[]).is_err());
    }

    struct QuoteSource {
        stale: bool,
    }
    impl HardwareProofSourceV1 for QuoteSource {
        fn code_identity(&self) -> Result<(B256, B256, u16, u16)> {
            Ok((B256::repeat_byte(1), B256::repeat_byte(2), 1, 1))
        }
        fn quote(&self, report: [u8; 64]) -> Result<Vec<u8>> {
            let mut bytes = vec![0; crate::quote::MIN_QUOTE_LEN];
            let offset = bytes.len() - 64;
            bytes[offset..].copy_from_slice(&report);
            if self.stale {
                bytes[offset] ^= 1;
            }
            Ok(bytes)
        }
    }
    #[test]
    fn registration_proofs_bind_manifest_mode_and_exact_hardware_report_data() {
        let dev =
            test_support::context(AttestationMode::NoAttestationDev, ProofSourceV1::NoHardware);
        let mut request = test_support::registration(&dev);
        let sig = dev.sign_registration_intent_dev_v1(&request).unwrap();
        assert!(request.verify_enclave_signature(&sig));
        request.genesis_hash = B256::repeat_byte(99);
        assert!(dev.sign_registration_intent_dev_v1(&request).is_err());
        for stale in [false, true] {
            let hardware = test_support::context(
                AttestationMode::DcapRequired,
                ProofSourceV1::Hardware(Arc::new(QuoteSource { stale })),
            );
            let request = test_support::registration(&hardware);
            assert!(hardware.sign_registration_intent_dev_v1(&request).is_err());
            let result = hardware.generate_dcap_quote(&request);
            if stale {
                assert!(result.is_err());
            } else {
                assert!(request.verify_enclave_signature(&result.unwrap().enclave_signature));
            }
        }
        assert!(dev
            .generate_dcap_quote(&test_support::registration(&dev))
            .is_err());
    }
}
