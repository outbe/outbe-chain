//! Domain Tribute decrypt probe plugged into [`outbe_execution::tee_canary`].

use std::time::SystemTime;

use alloy_primitives::{Address, B256, U256};
use outbe_enclave::protocol::{
    inputs_canonical_hash, EnclaveRequest as DomainRequest, EnclaveResponse as DomainResponse,
    EncryptedTributeOffer, TributeOfferResult, TributeOfferStatus, WorldwideDay,
};
use outbe_execution::tee_canary::{CanaryTickOutcome, EnclaveRequester, TeeCanaryProbe};
use outbe_tee::protocol::PayloadLane;

/// Fixed canary identity: a marker owner address and a fixed worldwide day so
/// the payload (and its Poseidon `token_id`) is identical on every tick and
/// every validator. The offer is never submitted as a transaction. It exists
/// only inside the probe request.
const CANARY_OWNER: Address = Address::repeat_byte(0xCA);
const CANARY_DAY: u32 = 20250115;
const CANARY_EPH_SK: [u8; 32] = [0xC5; 32];
const CANARY_NONCE: [u8; 12] = [0xC6; 12];
const CANARY_PRICE_MINOR: u64 = 2_000_000;
/// ISO 4217 numeric code 999 (no currency). The canary names no real currency.
const CANARY_CURRENCY: u16 = 999;

/// Known-plaintext Tribute offer batch probe executed on the canary lane.
#[derive(Clone, Copy, Debug, Default)]
pub struct TributeCanaryProbe;

impl TeeCanaryProbe for TributeCanaryProbe {
    fn probe_decrypt(
        &self,
        requester: &dyn EnclaveRequester,
        offer_pub: &[u8; 32],
    ) -> CanaryTickOutcome {
        decrypt_canary(requester, offer_pub)
    }
}

/// Typed validation errors for a canary `TributeOfferBatch` response.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CanaryBatchValidationError {
    #[error("canary expected 1 result, got {0}")]
    UnexpectedResultCount(usize),
    #[error("canary offer rejected: {0}")]
    OfferRejected(String),
    #[error("canary result echoes a different owner")]
    OwnerMismatch,
    #[error("canary inputs_canonical_hash mismatch (non-determinism)")]
    InputsCanonicalHashMismatch,
    #[error("no pinned attestation key")]
    NoPinnedAttestationKey,
    #[error("canary attestation tag invalid: {0}")]
    InvalidAttestationTag(String),
}

impl CanaryBatchValidationError {
    #[must_use]
    fn invalid_attestation_tag(error: impl core::fmt::Display) -> Self {
        Self::InvalidAttestationTag(error.to_string())
    }
}

struct CanaryBatch<'a> {
    offers: &'a [EncryptedTributeOffer],
    results: &'a [TributeOfferResult],
    reported_hash: B256,
    attestation_tag: &'a [u8],
}

fn encrypted_canary_offers(
    offer_pub: &[u8; 32],
) -> Result<Vec<EncryptedTributeOffer>, CanaryTickOutcome> {
    let plaintext = outbe_enclave::offer_encrypt::canary_offer_json(CANARY_DAY);
    let cipher_text = match outbe_enclave::offer_encrypt::encrypt_tribute_offer_with(
        offer_pub,
        CANARY_EPH_SK,
        CANARY_NONCE,
        plaintext.as_bytes(),
    ) {
        Ok(cipher_text) => cipher_text,
        Err(reason) => {
            return Err(CanaryTickOutcome::Failure {
                unreachable: false,
                reason: format!("canary encryption failed: {reason}"),
            });
        }
    };
    let eph_pub =
        x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(CANARY_EPH_SK)).to_bytes();
    Ok(vec![EncryptedTributeOffer {
        owner: CANARY_OWNER,
        cipher_text,
        nonce: CANARY_NONCE.to_vec(),
        ephemeral_pubkey: U256::from_be_bytes(eph_pub),
        worldwide_day: WorldwideDay::new(CANARY_DAY),
        tribute_currency: CANARY_CURRENCY,
        reference_currency: CANARY_CURRENCY,
        issuance_wwd_vwap_minor: U256::from(CANARY_PRICE_MINOR),
        reference_wwd_vwap_minor: U256::from(CANARY_PRICE_MINOR),
        reference_scurve_minor: U256::ZERO,
        zk_context: None,
    }])
}

fn decrypt_canary(requester: &dyn EnclaveRequester, offer_pub: &[u8; 32]) -> CanaryTickOutcome {
    let offers = match encrypted_canary_offers(offer_pub) {
        Ok(offers) => offers,
        Err(outcome) => return outcome,
    };
    let domain_req = DomainRequest::ProcessTributeOfferBatch {
        offers: offers.clone(),
    };
    let wire_req = match domain_req.encode_wire(PayloadLane::Canary) {
        Ok(wire_req) => wire_req,
        Err(error) => return CanaryTickOutcome::decrypt_transport_failure(&error),
    };
    let started = SystemTime::now();
    let response = requester
        .request_with_meta(domain_req.call_meta(), &wire_req)
        .and_then(|wire_resp| DomainResponse::from_wire_response(&wire_resp));
    let latency_ms = SystemTime::now()
        .duration_since(started)
        .map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    match response {
        Ok(DomainResponse::TributeOfferBatch {
            results,
            inputs_canonical_hash: reported_hash,
            attestation_tag,
        }) => validate_canary_batch(
            requester,
            CanaryBatch {
                offers: &offers,
                results: &results,
                reported_hash,
                attestation_tag: &attestation_tag,
            },
            latency_ms,
        ),
        Ok(other) => CanaryTickOutcome::Failure {
            unreachable: false,
            reason: format!("unexpected canary response: {other:?}"),
        },
        Err(error) => CanaryTickOutcome::decrypt_transport_failure(&error),
    }
}

fn validate_canary_batch(
    requester: &dyn EnclaveRequester,
    batch: CanaryBatch<'_>,
    latency_ms: u64,
) -> CanaryTickOutcome {
    let result = validate_batch_echo(&batch).and_then(|()| validate_batch_proof(requester, &batch));
    match result {
        Ok(()) => CanaryTickOutcome::Success { latency_ms },
        Err(reason) => CanaryTickOutcome::Failure {
            unreachable: false,
            reason: reason.to_string(),
        },
    }
}

fn validate_batch_echo(batch: &CanaryBatch<'_>) -> Result<(), CanaryBatchValidationError> {
    let results = batch.results;
    if results.len() != 1 {
        return Err(CanaryBatchValidationError::UnexpectedResultCount(
            results.len(),
        ));
    }
    let result = &results[0];
    if let TributeOfferStatus::Rejected { reason } = &result.status {
        return Err(CanaryBatchValidationError::OfferRejected(reason.clone()));
    }
    if result.owner != CANARY_OWNER {
        return Err(CanaryBatchValidationError::OwnerMismatch);
    }
    Ok(())
}

fn validate_batch_proof(
    requester: &dyn EnclaveRequester,
    batch: &CanaryBatch<'_>,
) -> Result<(), CanaryBatchValidationError> {
    let CanaryBatch {
        offers,
        results,
        reported_hash,
        attestation_tag,
    } = *batch;
    if reported_hash != inputs_canonical_hash(offers) {
        return Err(CanaryBatchValidationError::InputsCanonicalHashMismatch);
    }
    let Some(attestation_pub) = requester.attestation_pub() else {
        return Err(CanaryBatchValidationError::NoPinnedAttestationKey);
    };
    outbe_enclave::verify_tribute_offer_attestation(
        &attestation_pub,
        reported_hash,
        results,
        attestation_tag,
    )
    .map_err(CanaryBatchValidationError::invalid_attestation_tag)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    use outbe_execution::tee_canary::run_canary_probe;
    use outbe_tee::errors::TransportError;
    use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct AttestationOnly {
        key: Option<[u8; 32]>,
        reads: AtomicUsize,
    }

    impl EnclaveRequester for AttestationOnly {
        fn request(&self, _: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
            panic!("batch validation started transport I/O");
        }
        fn attestation_pub(&self) -> Option<[u8; 32]> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.key
        }
    }

    struct ScriptedRequester {
        script: Mutex<Vec<(&'static str, Result<EnclaveResponse, TransportError>)>>,
    }

    impl ScriptedRequester {
        fn new(script: Vec<(&'static str, Result<EnclaveResponse, TransportError>)>) -> Self {
            Self {
                script: Mutex::new(script),
            }
        }
    }

    impl EnclaveRequester for ScriptedRequester {
        fn request(&self, req: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
            let mut script = self.script.lock().expect("script lock");
            let (expected, response) = script.remove(0);
            assert_eq!(expected, req.label(), "unexpected request order");
            response
        }

        fn request_with_meta(
            &self,
            meta: outbe_tee::CallMeta,
            _: &EnclaveRequest,
        ) -> Result<EnclaveResponse, TransportError> {
            let mut script = self.script.lock().expect("script lock");
            let (expected, response) = script.remove(0);
            assert_eq!(expected, meta.label, "unexpected request meta label");
            response
        }

        fn attestation_pub(&self) -> Option<[u8; 32]> {
            Some([0xAA; 32])
        }
    }

    fn created_result() -> TributeOfferResult {
        TributeOfferResult {
            token_id: B256::repeat_byte(1),
            owner: CANARY_OWNER,
            issuance_amount_minor: U256::from(1),
            nominal_amount_minor: U256::from(1),
            effective_reference_price_minor: U256::from(CANARY_PRICE_MINOR),
            su_hashes: Vec::new(),
            wallet_addresses: Vec::new(),
            sra_addresses: Vec::new(),
            zk_expected_hashes: None,
            status: TributeOfferStatus::Created,
        }
    }

    fn public_keys_ready() -> EnclaveResponse {
        EnclaveResponse::PublicKeys {
            offer_key_ready: true,
            recipient_x25519_pub: [0x0B; 32],
            attestation_pub: [0xAA; 32],
            noise_static_pub: [0x0C; 32],
            tee_bls_pub: Vec::new(),
            dkg_enc_pub: [0x0D; 32],
            dkg_enc_sig: Vec::new(),
        }
    }

    fn requester(key: Option<[u8; 32]>) -> AttestationOnly {
        AttestationOnly {
            key,
            reads: AtomicUsize::new(0),
        }
    }

    #[test]
    fn canary_offer_names_no_currency() {
        let offer_pub =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([7u8; 32])).to_bytes();
        let offers = encrypted_canary_offers(&offer_pub).expect("canary offer encrypts");
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].tribute_currency, 999);
        assert_eq!(offers[0].reference_currency, 999);
    }

    #[test]
    fn malformed_batch_checks_precede_pinned_key_lookup() {
        let mut rejected = created_result();
        rejected.owner = Address::ZERO;
        rejected.status = TributeOfferStatus::Rejected {
            reason: "rejected".into(),
        };
        let mut wrong_owner = created_result();
        wrong_owner.owner = Address::ZERO;
        let cases = [
            (vec![], "canary expected 1 result, got 0"),
            (vec![rejected], "canary offer rejected: rejected"),
            (vec![wrong_owner], "canary result echoes a different owner"),
            (
                vec![created_result()],
                "canary inputs_canonical_hash mismatch (non-determinism)",
            ),
        ];
        for (results, reason) in cases {
            let requester = requester(None);
            let outcome = validate_canary_batch(
                &requester,
                CanaryBatch {
                    offers: &[],
                    results: &results,
                    reported_hash: B256::ZERO,
                    attestation_tag: &[],
                },
                42,
            );
            assert_eq!(
                outcome,
                CanaryTickOutcome::Failure {
                    unreachable: false,
                    reason: reason.into()
                }
            );
            assert_eq!(requester.reads.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn valid_batch_requires_a_pinned_attestation_key() {
        let requester = requester(None);
        let results = [created_result()];
        let outcome = validate_canary_batch(
            &requester,
            CanaryBatch {
                offers: &[],
                results: &results,
                reported_hash: inputs_canonical_hash(&[]),
                attestation_tag: &[],
            },
            42,
        );
        assert_eq!(
            outcome,
            CanaryTickOutcome::Failure {
                unreachable: false,
                reason: "no pinned attestation key".into()
            }
        );
        assert_eq!(requester.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn signed_batch_preserves_latency_and_rejects_a_different_pinned_key() {
        let signer = SigningKey::from_bytes(&[9; 32]);
        let results = [created_result()];
        let hash = inputs_canonical_hash(&[]);
        let signature = signer
            .sign(
                &outbe_enclave::protocol::tribute_offer_attestation_preimage(hash, &results)
                    .expect("tribute preimage"),
            )
            .to_bytes();
        let pinned = requester(Some(signer.verifying_key().to_bytes()));
        let batch = || CanaryBatch {
            offers: &[],
            results: &results,
            reported_hash: hash,
            attestation_tag: &signature,
        };
        assert_eq!(
            validate_canary_batch(&pinned, batch(), 42),
            CanaryTickOutcome::Success { latency_ms: 42 }
        );
        assert_eq!(pinned.reads.load(Ordering::SeqCst), 1);
        let wrong = requester(Some(
            SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes(),
        ));
        let outcome = validate_canary_batch(&wrong, batch(), 42);
        assert!(
            matches!(outcome, CanaryTickOutcome::Failure { unreachable: false, reason } if reason.starts_with("canary attestation tag invalid:"))
        );
        assert_eq!(wrong.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn wrong_canonical_hash_is_a_failure() {
        let bad_batch = DomainResponse::TributeOfferBatch {
            results: vec![created_result()],
            inputs_canonical_hash: B256::repeat_byte(0xFF),
            attestation_tag: Vec::new(),
        }
        .into_wire_response()
        .expect("encode bad batch");
        let fake = ScriptedRequester::new(vec![
            ("get_public_keys", Ok(public_keys_ready())),
            ("process_tribute_offer_batch", Ok(bad_batch)),
        ]);
        let (outcome, _, _) = run_canary_probe(&fake, &TributeCanaryProbe, Some(false));
        match outcome {
            CanaryTickOutcome::Failure {
                unreachable,
                reason,
            } => {
                assert!(!unreachable);
                assert!(
                    reason.contains("inputs_canonical_hash"),
                    "unexpected reason: {reason}"
                );
            }
            other => panic!("expected failure, got {other:?}"),
        }
    }
}
