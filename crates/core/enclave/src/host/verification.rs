//! Domain Ed25519 attestation verification built over [`outbe_tee::tee_call`].

use alloy_primitives::B256;
use outbe_tee::{tee_call::verify_enclave_attestation_lazy, TransportError};

use super::protocol::{
    fidelity_cohort_attestation_preimage, fidelity_query_attestation_preimage,
    fidelity_snapshot_attestation_preimage, gratis_op_attestation_preimage,
    mine_gratis_from_promis_attestation_preimage, promis_op_attestation_preimage,
    tribute_offer_attestation_preimage, FidelityCohortResult, FidelityLeagueEntry,
    FidelityQueryResult, GratisOpResult, MineGratisFromPromisResultV1, PromisOpResult,
    TributeOfferResult,
};

fn verify_domain_attestation(
    attestation_pub: &[u8; 32],
    tag: &[u8],
    (context, metric_class): (&'static str, &'static str),
    preimage: impl FnOnce() -> Result<Vec<u8>, serde_json::Error>,
) -> Result<(), TransportError> {
    verify_enclave_attestation_lazy(attestation_pub, tag, preimage)
        .map_err(|cause| TransportError::payload_attestation(context, metric_class, cause))
}

/// Verify a per-offer attestation tag against the peer's session attestation public key.
pub fn verify_tribute_offer_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    results: &[TributeOfferResult],
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_domain_attestation(
        attestation_pub,
        tag,
        ("offer", "tribute_offer_attestation"),
        || tribute_offer_attestation_preimage(inputs_canonical_hash, results),
    )
}

/// Verify a Gratis-op attestation tag against the peer's session attestation key.
pub fn verify_gratis_op_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &GratisOpResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_domain_attestation(
        attestation_pub,
        tag,
        ("gratis-op", "gratis_op_attestation"),
        || gratis_op_attestation_preimage(inputs_canonical_hash, result),
    )
}

/// Verify a Promis-op attestation tag.
pub fn verify_promis_op_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &PromisOpResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_domain_attestation(
        attestation_pub,
        tag,
        ("promis-op", "promis_op_attestation"),
        || promis_op_attestation_preimage(inputs_canonical_hash, result),
    )
}

/// Verify a paired `MineGratisFromPromisV1` attestation tag.
pub fn verify_mine_gratis_from_promis_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &MineGratisFromPromisResultV1,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_domain_attestation(
        attestation_pub,
        tag,
        ("promis-op", "promis_op_attestation"),
        || mine_gratis_from_promis_attestation_preimage(inputs_canonical_hash, result),
    )
}

/// Verify a standalone Fidelity cohort-op attestation tag.
pub fn verify_fidelity_cohort_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &FidelityCohortResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_domain_attestation(
        attestation_pub,
        tag,
        ("fidelity", "fidelity_attestation"),
        || fidelity_cohort_attestation_preimage(inputs_canonical_hash, result),
    )
}

/// Verify a Fidelity league-snapshot attestation tag.
pub fn verify_fidelity_snapshot_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    leagues: &[FidelityLeagueEntry],
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_domain_attestation(
        attestation_pub,
        tag,
        ("fidelity", "fidelity_attestation"),
        || fidelity_snapshot_attestation_preimage(inputs_canonical_hash, leagues),
    )
}

/// Verify a Fidelity index-query attestation tag.
pub fn verify_fidelity_query_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &FidelityQueryResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_domain_attestation(
        attestation_pub,
        tag,
        ("fidelity", "fidelity_attestation"),
        || fidelity_query_attestation_preimage(inputs_canonical_hash, result),
    )
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, U256};
    use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
    use outbe_tee::TeeAttestationError;

    use super::*;
    use crate::host::protocol::{GratisOpStatus, PromisOpStatus, TributeOfferStatus};

    #[test]
    fn verify_tribute_offer_attestation_accepts_valid_rejects_tampering() {
        let results = vec![TributeOfferResult {
            token_id: B256::repeat_byte(0x11),
            owner: Address::repeat_byte(0x22),
            issuance_amount_minor: U256::from(1_000u64),
            nominal_amount_minor: U256::from(2_000u64),
            effective_reference_price_minor: U256::from(3_000u64),
            su_hashes: vec!["0xabc".to_string()],
            wallet_addresses: vec![],
            sra_addresses: vec![],
            zk_expected_hashes: None,
            status: TributeOfferStatus::Created,
        }];
        let hash = B256::repeat_byte(0xAB);

        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = sk.verifying_key().to_bytes();
        let json = serde_json::to_vec(&results).expect("tribute fixture json");
        let mut expected = Vec::with_capacity(30 + 32 + 4 + json.len());
        expected.extend_from_slice(b"outbe/tee/offer-attestation/v1");
        expected.extend_from_slice(hash.as_slice());
        expected.extend_from_slice(
            &u32::try_from(json.len())
                .expect("json length")
                .to_be_bytes(),
        );
        expected.extend_from_slice(&json);
        let preimage =
            tribute_offer_attestation_preimage(hash, &results).expect("tribute preimage");
        assert_eq!(preimage, expected);
        let tag = sk.sign(&expected).to_bytes();

        verify_tribute_offer_attestation(&pk, hash, &results, &tag).expect("valid tag verifies");

        let mut tampered = results.clone();
        tampered[0].effective_reference_price_minor += U256::ONE;
        assert!(verify_tribute_offer_attestation(&pk, hash, &tampered, &tag).is_err());

        assert!(
            verify_tribute_offer_attestation(&pk, B256::repeat_byte(0xCD), &results, &tag).is_err()
        );

        let other = SigningKey::from_bytes(&[8u8; 32])
            .verifying_key()
            .to_bytes();
        assert!(verify_tribute_offer_attestation(&other, hash, &results, &tag).is_err());
        assert!(verify_tribute_offer_attestation(&pk, hash, &results, &[0u8; 10]).is_err());
    }

    #[test]
    fn gratis_promis_and_fidelity_tags_keep_domains_and_error_order() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = sk.verifying_key().to_bytes();
        let hash = B256::repeat_byte(0xAB);
        let gratis = GratisOpResult {
            status: GratisOpStatus::Applied,
            new_balance: vec![1],
            new_pledged: vec![2],
            event_amount: U256::from(3),
            encrypted_event_amount: vec![4],
            next_op_nonce: 4,
            fidelity: None,
            inputs_canonical_hash: hash,
            attestation_tag: vec![],
        };
        let promis = PromisOpResult {
            status: PromisOpStatus::Applied,
            new_balance: vec![1],
            event_amount: U256::from(3),
            encrypted_event_amount: vec![4],
            next_op_nonce: 4,
            inputs_canonical_hash: hash,
            attestation_tag: vec![],
        };
        let fidelity = FidelityQueryResult {
            rcfi: U256::from(5),
            efficiency: U256::from(6),
            league: 7,
            inputs_canonical_hash: hash,
            attestation_tag: vec![],
        };
        let gratis_tag = sk
            .sign(&gratis_op_attestation_preimage(hash, &gratis).expect("gratis preimage"))
            .to_bytes();
        let promis_tag = sk
            .sign(&promis_op_attestation_preimage(hash, &promis).expect("promis preimage"))
            .to_bytes();
        let fidelity_tag = sk
            .sign(&fidelity_query_attestation_preimage(hash, &fidelity).expect("fidelity preimage"))
            .to_bytes();

        verify_gratis_op_attestation(&pk, hash, &gratis, &gratis_tag).expect("gratis tag");
        verify_promis_op_attestation(&pk, hash, &promis, &promis_tag).expect("promis tag");
        verify_fidelity_query_attestation(&pk, hash, &fidelity, &fidelity_tag)
            .expect("fidelity tag");

        let cross_promis = verify_promis_op_attestation(&pk, hash, &promis, &gratis_tag)
            .expect_err("cross-domain promis tag must fail");
        assert_eq!(cross_promis.metric_class(), "promis_op_attestation");
        assert!(matches!(
            &cross_promis,
            TransportError::PayloadAttestation(inner)
                if inner.context == "promis-op"
                    && matches!(inner.cause, TeeAttestationError::SignatureMismatch(_))
        ));

        let cross_gratis = verify_gratis_op_attestation(&pk, hash, &gratis, &promis_tag)
            .expect_err("cross-domain gratis tag must fail");
        assert_eq!(cross_gratis.metric_class(), "gratis_op_attestation");

        let cross_fidelity = verify_fidelity_query_attestation(&pk, hash, &fidelity, &gratis_tag)
            .expect_err("cross-domain fidelity tag must fail");
        assert_eq!(cross_fidelity.metric_class(), "fidelity_attestation");

        let mut malformed_key = [0u8; 32];
        malformed_key[0] = 2;
        assert!(VerifyingKey::from_bytes(&malformed_key).is_err());
        let bad_key = verify_gratis_op_attestation(&malformed_key, hash, &gratis, &[0u8; 1])
            .expect_err("bad key precedes tag length");
        assert!(matches!(
            &bad_key,
            TransportError::PayloadAttestation(inner)
                if inner.context == "gratis-op"
                    && matches!(inner.cause, TeeAttestationError::InvalidPublicKey(_))
        ));

        let bad_len = verify_promis_op_attestation(&pk, hash, &promis, &[0u8; 1])
            .expect_err("bad tag length");
        assert!(matches!(
            &bad_len,
            TransportError::PayloadAttestation(inner)
                if inner.context == "promis-op"
                    && matches!(inner.cause, TeeAttestationError::InvalidSignatureLength { actual: 1 })
        ));
    }

    #[test]
    fn mutated_event_amount_fails_gratis_and_promis_attestation() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = sk.verifying_key().to_bytes();
        let hash = B256::repeat_byte(0xAB);
        let mut gratis = GratisOpResult {
            status: GratisOpStatus::Applied,
            new_balance: vec![1, 2, 3],
            new_pledged: Vec::new(),
            event_amount: U256::from(3),
            encrypted_event_amount: vec![4, 5, 6, 7],
            next_op_nonce: 4,
            fidelity: None,
            inputs_canonical_hash: hash,
            attestation_tag: vec![],
        };
        let mut promis = PromisOpResult {
            status: PromisOpStatus::Applied,
            new_balance: vec![1, 2, 3],
            event_amount: U256::from(3),
            encrypted_event_amount: vec![4, 5, 6, 7],
            next_op_nonce: 4,
            inputs_canonical_hash: hash,
            attestation_tag: vec![],
        };
        let gratis_tag = sk
            .sign(&gratis_op_attestation_preimage(hash, &gratis).expect("gratis preimage"))
            .to_bytes();
        let promis_tag = sk
            .sign(&promis_op_attestation_preimage(hash, &promis).expect("promis preimage"))
            .to_bytes();
        verify_gratis_op_attestation(&pk, hash, &gratis, &gratis_tag).expect("gratis tag");
        verify_promis_op_attestation(&pk, hash, &promis, &promis_tag).expect("promis tag");

        gratis.encrypted_event_amount[0] ^= 0xff;
        promis.encrypted_event_amount[0] ^= 0xff;
        assert!(verify_gratis_op_attestation(&pk, hash, &gratis, &gratis_tag).is_err());
        assert!(verify_promis_op_attestation(&pk, hash, &promis, &promis_tag).is_err());
    }
}
