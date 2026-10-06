//! Blocking canary stages. Telemetry and admission verdicts stay distinct.
use super::*;

struct ProbeTelemetry {
    supported: Option<bool>,
    payload: Option<EnclaveHealthStatusV1>,
}
struct CanaryBatch<'a> {
    offers: &'a [EncryptedTributeOffer],
    results: &'a [outbe_tee::protocol::TributeOfferResult],
    reported_hash: alloy_primitives::B256,
    attestation_tag: &'a [u8],
}
pub(super) fn run(
    requester: &dyn EnclaveRequester,
    supported: Option<bool>,
) -> (
    CanaryTickOutcome,
    Option<bool>,
    Option<EnclaveHealthStatusV1>,
) {
    let telemetry = detect_health(requester, supported);
    let outcome = match read_offer_key(requester) {
        Ok(key) => decrypt_canary(requester, &key),
        Err(outcome) => outcome,
    };
    (outcome, telemetry.supported, telemetry.payload)
}
fn detect_health(requester: &dyn EnclaveRequester, supported: Option<bool>) -> ProbeTelemetry {
    let unchanged = ProbeTelemetry {
        supported,
        payload: None,
    };
    if supported == Some(false) {
        return unchanged;
    }
    // A Health failure is ambiguous on old enclaves. A successful fallback
    // request rules out a dead connection. The probe still reads key readiness
    // afresh.
    match requester.request(&EnclaveRequest::Health) {
        Ok(EnclaveResponse::HealthStatus { status }) => ProbeTelemetry {
            supported: Some(true),
            payload: Some(*status),
        },
        Ok(_) | Err(_) if supported.is_none() => {
            if requester.request(&EnclaveRequest::GetPublicKeys).is_ok() {
                ProbeTelemetry {
                    supported: Some(false),
                    payload: None,
                }
            } else {
                unchanged
            }
        }
        Ok(_) | Err(_) => unchanged,
    }
}
fn read_offer_key(requester: &dyn EnclaveRequester) -> Result<[u8; 32], CanaryTickOutcome> {
    // 2. Offer-key state (fresh each tick, so key epochs are followed).
    match requester.request(&EnclaveRequest::GetPublicKeys) {
        Ok(EnclaveResponse::PublicKeys {
            offer_key_ready,
            recipient_x25519_pub,
            ..
        }) => {
            if !offer_key_ready {
                return Err(CanaryTickOutcome::OfferKeyNotReady);
            }
            Ok(recipient_x25519_pub)
        }
        Ok(other) => Err(CanaryTickOutcome::Failure {
            unreachable: false,
            reason: format!("unexpected GetPublicKeys response: {other:?}"),
        }),
        Err(error) => Err(CanaryTickOutcome::Failure {
            unreachable: error.is_connection_fault()
                || matches!(
                    error,
                    TransportError::SessionRevoked(_) | TransportError::EnclaveError(_)
                ),
            reason: format!("GetPublicKeys failed: {}", error.metric_class()),
        }),
    }
}
fn encrypted_canary_offers(
    offer_pub: &[u8; 32],
) -> Result<Vec<EncryptedTributeOffer>, CanaryTickOutcome> {
    // 3. Known-plaintext canary decrypt through the real consensus request.
    let plaintext = outbe_tee::offer_encrypt::canary_offer_json(CANARY_DAY);
    let cipher_text = match outbe_tee::offer_encrypt::encrypt_tribute_offer_with(
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
        tribute_currency: 840,
        reference_currency: 840,
        exclude_from_intex_issuance: false,
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
    let started = SystemTime::now();
    let response = requester.request(&EnclaveRequest::ProcessTributeOfferBatch {
        offers: offers.clone(),
    });
    let latency_ms = SystemTime::now()
        .duration_since(started)
        .map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    match response {
        Ok(EnclaveResponse::TributeOfferBatch {
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
        Err(error) => CanaryTickOutcome::Failure {
            unreachable: error.is_connection_fault()
                || matches!(error, TransportError::SessionRevoked(_)),
            reason: format!("canary decrypt failed: {}", error.metric_class()),
        },
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
            reason,
        },
    }
}
fn validate_batch_echo(batch: &CanaryBatch<'_>) -> Result<(), String> {
    let results = batch.results;
    if results.len() != 1 {
        return Err(format!("canary expected 1 result, got {}", results.len()));
    }
    let result = &results[0];
    if let TributeOfferStatus::Rejected { reason } = &result.status {
        return Err(format!("canary offer rejected: {reason}"));
    }
    if result.owner != CANARY_OWNER {
        return Err("canary result echoes a different owner".to_string());
    }
    Ok(())
}
fn validate_batch_proof(
    requester: &dyn EnclaveRequester,
    batch: &CanaryBatch<'_>,
) -> Result<(), String> {
    let CanaryBatch {
        offers,
        results,
        reported_hash,
        attestation_tag,
    } = *batch;
    if reported_hash != inputs_canonical_hash(offers) {
        return Err("canary inputs_canonical_hash mismatch (non-determinism)".to_string());
    }
    let Some(attestation_pub) = requester.attestation_pub() else {
        return Err("no pinned attestation key".to_string());
    };
    if let Err(error) = outbe_tee::verify_tribute_offer_attestation(
        &attestation_pub,
        reported_hash,
        results,
        attestation_tag,
    ) {
        return Err(format!("canary attestation tag invalid: {error}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    use std::sync::atomic::{AtomicUsize, Ordering};

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
    fn created_result() -> outbe_tee::protocol::TributeOfferResult {
        outbe_tee::protocol::TributeOfferResult {
            token_id: alloy_primitives::B256::repeat_byte(1),
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
    fn requester(key: Option<[u8; 32]>) -> AttestationOnly {
        AttestationOnly {
            key,
            reads: AtomicUsize::new(0),
        }
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
                    reported_hash: alloy_primitives::B256::ZERO,
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
            .sign(&outbe_tee::protocol::tribute_offer_attestation_preimage(
                hash, &results,
            ))
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
}
