//! Offer decrypt probe plugged into [`outbe_execution::tee_canary`].

use std::time::{Instant, SystemTime};

use alloy_primitives::{Address, B256, U256};
use outbe_execution::tee_canary::{CanaryTickOutcome, EnclaveRequester, TeeCanaryProbe};
use outbe_nod::nod_host::OfferCanaryV1;
use outbe_nod::offer::protocol::{inputs_canonical_hash, EncryptedOffer, WorldwideDay};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::TransportError;

/// Fixed canary identity: a marker owner address and a fixed worldwide day so
/// the payload is identical on every tick and every validator. The offer is
/// never submitted as a transaction. It exists only inside the probe request.
const CANARY_OWNER: Address = Address::repeat_byte(0xCA);
const CANARY_DAY: u32 = 20250115;
const CANARY_EPH_SK: [u8; 32] = [0xC5; 32];
const CANARY_NONCE: [u8; 12] = [0xC6; 12];
const CANARY_PRICE_MINOR: u64 = 2_000_000;
/// ISO 4217 numeric code 999 (no currency). The canary names no real currency.
const CANARY_CURRENCY: u16 = 999;
/// The `creator` the canary payload names.
const CANARY_CREATOR: &str = "canary";

/// Known-plaintext offer probe executed on the canary lane.
#[derive(Clone, Copy, Debug, Default)]
pub struct OfferCanaryProbe;

#[cfg(feature = "test-hooks")]
impl OfferCanaryProbe {
    /// Production `BoundCanary` readiness probe (`decrypt_canary_on`).
    ///
    /// The probe checks that an activated resident key can open the canary.
    /// It does not read `TeeAuthorityState` and it does not check a lease.
    #[must_use]
    pub fn bound_canary_decrypt(
        context: &InProcessTeeContext,
        offer_pub: &[u8; 32],
    ) -> CanaryTickOutcome {
        struct Pinned([u8; 32]);
        impl EnclaveRequester for Pinned {
            fn request(
                &self,
                _: &outbe_tee::protocol::EnclaveRequest,
            ) -> Result<outbe_tee::protocol::EnclaveResponse, TransportError> {
                Err(TransportError::UnexpectedResponse)
            }

            fn attestation_pub(&self) -> Option<[u8; 32]> {
                Some(self.0)
            }
        }
        decrypt_canary_on(&Pinned(context.attestation_public()), context, offer_pub)
    }
}

impl TeeCanaryProbe for OfferCanaryProbe {
    fn probe_decrypt(
        &self,
        requester: &dyn EnclaveRequester,
        offer_pub: &[u8; 32],
    ) -> CanaryTickOutcome {
        decrypt_canary(requester, offer_pub)
    }
}

/// Typed validation errors for a canary response.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CanaryValidationError {
    #[error("canary result echoes a different creator")]
    CreatorMismatch,
    #[error("canary inputs_canonical_hash mismatch (non-determinism)")]
    InputsCanonicalHashMismatch,
    #[error("no pinned attestation key")]
    NoPinnedAttestationKey,
    #[error("canary attestation tag invalid: {0}")]
    InvalidAttestationTag(#[source] TransportError),
}

fn encrypted_canary_offer(offer_pub: &[u8; 32]) -> Result<EncryptedOffer, CanaryTickOutcome> {
    let plaintext = outbe_core_common::confidential_tee::canary_offer_json(CANARY_DAY);
    let cipher_text = match outbe_core_common::confidential_tee::encrypt_offer_with(
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
    Ok(EncryptedOffer {
        owner: CANARY_OWNER,
        cipher_text,
        nonce: CANARY_NONCE.to_vec(),
        ephemeral_pubkey: U256::from_be_bytes(eph_pub),
        worldwide_day: WorldwideDay::new(CANARY_DAY),
        issuance_currency: CANARY_CURRENCY,
        reference_currency: CANARY_CURRENCY,
        issuance_wwd_vwap_minor: U256::from(CANARY_PRICE_MINOR),
        reference_wwd_vwap_minor: U256::from(CANARY_PRICE_MINOR),
        reference_scurve_minor: U256::ZERO,
        zk_context: None,
    })
}

fn decrypt_canary(requester: &dyn EnclaveRequester, offer_pub: &[u8; 32]) -> CanaryTickOutcome {
    let Some(context) = outbe_tee::in_process_tee_context() else {
        let started = Instant::now();
        let error = TransportError::from(outbe_tee::InProcessDispatchError::ContextNotInstalled);
        note_offer_open(started, Some(&error));
        return CanaryTickOutcome::decrypt_transport_failure(&error);
    };
    decrypt_canary_on(requester, &context, offer_pub)
}

fn note_offer_open(started: Instant, error: Option<&outbe_tee::TransportError>) {
    outbe_tee::metrics::record_request_duration("process_tribute_offer_batch", started.elapsed());
    if let Some(error) = error {
        outbe_tee::metrics::record_request_error(
            "process_tribute_offer_batch",
            error.metric_class(),
        );
    }
}

pub(crate) fn decrypt_canary_on(
    requester: &dyn EnclaveRequester,
    context: &InProcessTeeContext,
    offer_pub: &[u8; 32],
) -> CanaryTickOutcome {
    let offer = match encrypted_canary_offer(offer_pub) {
        Ok(offer) => offer,
        Err(outcome) => return outcome,
    };
    let started = SystemTime::now();
    let call_started = Instant::now();
    let response = outbe_nod::nod_host::open_offer_canary_with(context, &offer);
    note_offer_open(call_started, response.as_ref().err());
    let latency_ms = SystemTime::now()
        .duration_since(started)
        .map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    finish_canary(requester, &offer, response, latency_ms)
}

fn finish_canary(
    requester: &dyn EnclaveRequester,
    offer: &EncryptedOffer,
    response: Result<OfferCanaryV1, TransportError>,
    latency_ms: u64,
) -> CanaryTickOutcome {
    match response {
        Ok(answer) => validate_canary(requester, offer, &answer, latency_ms),
        Err(error) => CanaryTickOutcome::decrypt_transport_failure(&error),
    }
}

fn validate_canary(
    requester: &dyn EnclaveRequester,
    offer: &EncryptedOffer,
    answer: &OfferCanaryV1,
    latency_ms: u64,
) -> CanaryTickOutcome {
    match check_canary(requester, offer, answer) {
        Ok(()) => CanaryTickOutcome::Success { latency_ms },
        Err(reason) => CanaryTickOutcome::Failure {
            unreachable: false,
            reason: reason.to_string(),
        },
    }
}

fn check_canary(
    requester: &dyn EnclaveRequester,
    offer: &EncryptedOffer,
    answer: &OfferCanaryV1,
) -> Result<(), CanaryValidationError> {
    if answer.creator != CANARY_CREATOR {
        return Err(CanaryValidationError::CreatorMismatch);
    }
    let expected: B256 = inputs_canonical_hash(std::slice::from_ref(offer));
    if answer.inputs_canonical_hash != expected {
        return Err(CanaryValidationError::InputsCanonicalHashMismatch);
    }
    let Some(attestation_pub) = requester.attestation_pub() else {
        return Err(CanaryValidationError::NoPinnedAttestationKey);
    };
    outbe_nod::nod_host::verify_offer_canary(&attestation_pub, expected, answer)
        .map_err(CanaryValidationError::InvalidAttestationTag)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct AttestationOnly {
        key: Option<[u8; 32]>,
        reads: AtomicUsize,
    }

    impl EnclaveRequester for AttestationOnly {
        fn request(
            &self,
            _: &outbe_tee::protocol::EnclaveRequest,
        ) -> Result<outbe_tee::protocol::EnclaveResponse, TransportError> {
            panic!("canary validation started transport I/O");
        }
        fn attestation_pub(&self) -> Option<[u8; 32]> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.key
        }
    }

    fn offer() -> EncryptedOffer {
        let offer_pub =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([7u8; 32])).to_bytes();
        encrypted_canary_offer(&offer_pub).expect("canary offer encrypts")
    }

    fn answer(creator: &str, inputs_canonical_hash: B256) -> OfferCanaryV1 {
        OfferCanaryV1 {
            creator: creator.into(),
            amount_minor: U256::from(100_000_000u64),
            inputs_canonical_hash,
            attestation_tag: Vec::new(),
        }
    }

    #[test]
    fn canary_offer_names_no_currency() {
        let offer = offer();
        assert_eq!(offer.issuance_currency, 999);
        assert_eq!(offer.reference_currency, 999);
    }

    #[test]
    fn echo_checks_precede_pinned_key_lookup() {
        let offer = offer();
        let hash = inputs_canonical_hash(std::slice::from_ref(&offer));
        for (answer, reason) in [
            (
                answer("other", hash),
                "canary result echoes a different creator",
            ),
            (
                answer(CANARY_CREATOR, B256::repeat_byte(0xFF)),
                "canary inputs_canonical_hash mismatch (non-determinism)",
            ),
        ] {
            let requester = AttestationOnly {
                key: None,
                reads: AtomicUsize::new(0),
            };
            assert_eq!(
                validate_canary(&requester, &offer, &answer, 42),
                CanaryTickOutcome::Failure {
                    unreachable: false,
                    reason: reason.into()
                }
            );
            assert_eq!(requester.reads.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn a_valid_echo_requires_a_pinned_attestation_key() {
        let offer = offer();
        let hash = inputs_canonical_hash(std::slice::from_ref(&offer));
        let requester = AttestationOnly {
            key: None,
            reads: AtomicUsize::new(0),
        };
        assert_eq!(
            validate_canary(&requester, &offer, &answer(CANARY_CREATOR, hash), 42),
            CanaryTickOutcome::Failure {
                unreachable: false,
                reason: "no pinned attestation key".into()
            }
        );
        assert_eq!(requester.reads.load(Ordering::SeqCst), 1);
    }
}

mod in_process;
#[cfg(test)]
pub(crate) mod test_support;
