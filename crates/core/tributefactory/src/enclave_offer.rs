//! Resident TEE tribute-offer decryption and authenticated public results.
//!
//! The shared offer key stays behind the in-process handle API. Missing
//! resident context is a node-local fault, not a deterministic EVM revert.

#[cfg(test)]
use alloy_primitives::B256;
use outbe_primitives::error::PrecompileError;
#[cfg(test)]
use outbe_tribute::confidential::verify_tribute_offer_attestation;
use outbe_tribute::confidential::{EncryptedTributeOffer, TributeOfferResult, TributeOfferStatus};

use crate::errors::TributeFactoryError;

/// Process a batch of offers through the enclave. The enclave decrypts the offers
/// (the offer key stays resident) and applies each offer's node-resolved price. It
/// computes economics + Poseidon `token_id` and returns the public `TributeOfferResult[]`.
/// The host then issues the Tributes from those fields plus its own request inputs
/// (no host recompute of the private economics).
///
/// The resident host recomputes `inputs_canonical_hash` from the request it sent
/// and verifies the attestation tag before this function returns. A mismatch is
/// enclave non-determinism. A bad tag is `tee_offer_attestation_invalid`. The tag
/// is discarded and never written to chain state. This function then checks that
/// each created offer echoes the host price.
///
/// Failure classification (aligned with `gratis::enclave_client::apply_gratis_op`):
/// every failure of THIS function is a node-local fault (`PrecompileError::EnclaveUnavailable`):
/// - dead sidecar
/// - transport error after the session's bounded reconnect+retry
/// - enclave authorization denial (keyless enclave)
/// - non-determinism
/// - bad attestation
///
/// Executing the tx differently from healthy validators would diverge state, so the
/// node fails the block instead. Deterministic per-offer rejections ride inside the
/// returned results, and the caller handles them.
pub fn process_tribute_offer_batch_via_enclave(
    offers: &[EncryptedTributeOffer],
) -> Result<Vec<TributeOfferResult>, PrecompileError> {
    let results = outbe_tribute::confidential::host::process_offer_batch(offers.to_vec()).map_err(
        |error| match error {
            outbe_tee::TransportError::EnclaveCause { source } => {
                TributeFactoryError::EnclaveBatchError(source)
            }
            other => TributeFactoryError::sidecar_unavailable(other),
        },
    )?;
    validate_effective_reference_prices(offers, &results)?;
    Ok(results)
}

/// Validate a scripted offer batch without a resident runtime.
#[cfg(test)]
fn validate_tribute_offer_batch_response(
    offers: &[EncryptedTributeOffer],
    results: Vec<TributeOfferResult>,
    inputs_canonical_hash: B256,
    attestation_pub: &[u8; 32],
    attestation_tag: &[u8],
) -> Result<Vec<TributeOfferResult>, PrecompileError> {
    let expected = outbe_tribute::confidential::inputs_canonical_hash(offers);
    if inputs_canonical_hash != expected {
        return Err(TributeFactoryError::InputsCanonicalHashMismatch.into());
    }
    verify_tribute_offer_attestation(
        attestation_pub,
        inputs_canonical_hash,
        &results,
        attestation_tag,
    )
    .map_err(TributeFactoryError::offer_attestation_invalid)?;
    validate_effective_reference_prices(offers, &results)?;
    Ok(results)
}

fn validate_effective_reference_prices(
    offers: &[EncryptedTributeOffer],
    results: &[TributeOfferResult],
) -> Result<(), PrecompileError> {
    if offers.len() != results.len() {
        return Err(TributeFactoryError::ResultCountMismatch {
            expected: offers.len(),
            got: results.len(),
        }
        .into());
    }
    for (offer, result) in offers.iter().zip(results) {
        if !matches!(result.status, TributeOfferStatus::Created) {
            continue;
        }
        let expected = offer
            .reference_wwd_vwap_minor
            .max(offer.reference_scurve_minor);
        if result.effective_reference_price_minor != expected {
            return Err(TributeFactoryError::EffectiveReferencePriceMismatch.into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, U256};
    use outbe_tribute::confidential::{inputs_canonical_hash, WorldwideDay};

    const NEXT_DAY: WorldwideDay = WorldwideDay::new(20250116);

    fn sample_tribute_offer() -> EncryptedTributeOffer {
        EncryptedTributeOffer {
            owner: Address::repeat_byte(0x11),
            cipher_text: vec![1, 2, 3, 4],
            nonce: vec![0u8; 12],
            ephemeral_pubkey: U256::from(7u64),
            worldwide_day: WorldwideDay::new(20250115),
            tribute_currency: 840,
            reference_currency: 840,
            issuance_wwd_vwap_minor: U256::from(1_000u64),
            reference_wwd_vwap_minor: U256::from(2_000u64),
            reference_scurve_minor: U256::from(3_000u64),
            zk_context: None,
        }
    }

    fn sample_result() -> TributeOfferResult {
        TributeOfferResult {
            token_id: B256::repeat_byte(0x22),
            owner: Address::repeat_byte(0x11),
            issuance_amount_minor: U256::from(100u64),
            nominal_amount_minor: U256::from(10u64),
            effective_reference_price_minor: U256::from(3_000u64),
            su_hashes: Vec::new(),
            wallet_addresses: Vec::new(),
            sra_addresses: Vec::new(),
            zk_expected_hashes: None,
            status: TributeOfferStatus::Created,
        }
    }

    /// Regression (2026-08-22 testnet incident): a dead/unconfigured sidecar is
    /// a NODE-LOCAL fault. It must surface as `PrecompileError::EnclaveUnavailable`, never as
    /// a deterministic revert. Reverting a tx that healthy validators execute
    /// diverges state.
    #[test]
    fn transport_unavailability_is_fatal_not_revert() {
        // No enclave session is installed in this test process.
        let err = process_tribute_offer_batch_via_enclave(&[sample_tribute_offer()])
            .expect_err("no enclave configured");
        match err {
            PrecompileError::EnclaveUnavailable(message) => assert!(
                message.contains("tee_sidecar_unavailable"),
                "unexpected enclave unavailable message: {message}"
            ),
            other => panic!("expected EnclaveUnavailable, got: {other:?}"),
        }
    }

    /// A returned `inputs_canonical_hash` that disagrees with the host's
    /// recompute is enclave non-determinism -> `tee_enclave_nondeterminism`. The
    /// hash check runs before attestation, so a bogus tag is irrelevant here.
    #[test]
    fn validate_rejects_inputs_canonical_hash_divergence() {
        let offers = vec![sample_tribute_offer()];
        let wrong_hash = B256::repeat_byte(0xFF);
        // Sanity: the wrong hash really differs from the canonical recompute.
        assert_ne!(wrong_hash, inputs_canonical_hash(&offers));

        let err =
            validate_tribute_offer_batch_response(&offers, Vec::new(), wrong_hash, &[0u8; 32], &[])
                .expect_err("hash divergence must be rejected");
        assert!(
            err.to_string().contains("tee_enclave_nondeterminism"),
            "unexpected error: {err}"
        );
    }

    /// The day, currency and price are request inputs the node resolved, so a
    /// response hashed over different values must not validate. Otherwise they
    /// ride to the enclave unattested.
    #[test]
    fn validate_binds_the_priced_request_fields() {
        let offers = vec![sample_tribute_offer()];

        for mutate in [
            (|o: &mut EncryptedTributeOffer| o.worldwide_day = NEXT_DAY) as fn(&mut _),
            |o: &mut EncryptedTributeOffer| o.tribute_currency = 978,
            |o: &mut EncryptedTributeOffer| o.issuance_wwd_vwap_minor = U256::from(2_000u64),
            |o: &mut EncryptedTributeOffer| o.reference_wwd_vwap_minor = U256::from(4_000u64),
            |o: &mut EncryptedTributeOffer| o.reference_scurve_minor = U256::from(5_000u64),
        ] {
            let mut other = offers.clone();
            mutate(&mut other[0]);
            let hash_of_other = inputs_canonical_hash(&other);

            let err = validate_tribute_offer_batch_response(
                &offers,
                Vec::new(),
                hash_of_other,
                &[0u8; 32],
                &[],
            )
            .expect_err("a hash over different request fields must be rejected");
            assert!(
                err.to_string().contains("tee_enclave_nondeterminism"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn effective_reference_price_must_match_the_public_request_inputs() {
        let offers = vec![sample_tribute_offer()];
        let results = vec![sample_result()];
        validate_effective_reference_prices(&offers, &results).unwrap();

        let mut wrong = results;
        wrong[0].effective_reference_price_minor -= U256::ONE;
        let error = validate_effective_reference_prices(&offers, &wrong)
            .expect_err("a forged enclave effective reference price must fail closed");
        assert!(error.to_string().contains("tee_enclave_nondeterminism"));
    }
}
