use k256::ecdsa::SigningKey;
use outbe_snapshot::provenance::{signing_digest, SignatureEnvelope};
use sha2::{Digest, Sha256};

fn signer(seed: u8) -> SigningKey {
    SigningKey::from_bytes((&[seed; 32]).into()).unwrap()
}

fn sign(raw: &[u8]) -> ([u8; 65], [u8; 33]) {
    let key = signer(7);
    let (signature, recovery) = key.sign_prehash_recoverable(&signing_digest(raw)).unwrap();
    let mut bytes = [0; 65];
    bytes[..64].copy_from_slice(&signature.to_bytes());
    bytes[64] = recovery.to_byte();
    let public = key.verifying_key().to_encoded_point(true);
    (bytes, public.as_bytes().try_into().unwrap())
}

#[test]
fn exact_manifest_bytes_use_the_planned_domain_and_identify_the_creator() {
    let raw = br#"{"height":100,"creator":"operator"}"#;
    let mut digest = Sha256::new();
    digest.update(b"outbe/snapshot/manifest/v1\0");
    digest.update(Sha256::digest(raw));
    assert_eq!(signing_digest(raw).as_slice(), digest.finalize().as_slice());
    let (signature, public) = sign(raw);
    let envelope = SignatureEnvelope::from_signature(raw, signature).unwrap();
    let encoded = serde_json::to_vec(&envelope).unwrap();
    let parsed = SignatureEnvelope::from_bytes(&encoded).unwrap();
    assert_eq!(parsed.verify(raw, None).unwrap(), public);
    assert_eq!(parsed.verify(raw, Some(&public)).unwrap(), public);

    let other: [u8; 33] = signer(8)
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    assert!(parsed.verify(raw, Some(&other)).is_err());
    assert!(parsed
        .verify(br#"{ "height":100,"creator":"operator"}"#, None)
        .is_err());
    assert!(parsed
        .verify(br#"{"height":101,"creator":"operator"}"#, None)
        .is_err());
    let altered = br#"{"height":101,"creator":"operator"}"#;
    let mut forged = parsed.clone();
    forged.bundle_id = hex::encode(Sha256::digest(altered));
    assert!(forged.verify(altered, None).is_err());
}

#[test]
fn missing_signature_and_changed_embedded_key_or_digest_are_rejected() {
    let raw = b"original manifest bytes";
    let (signature, _) = sign(raw);
    let envelope = SignatureEnvelope::from_signature(raw, signature).unwrap();
    assert!(SignatureEnvelope::from_bytes(b"").is_err());
    let original = serde_json::to_value(envelope).unwrap();
    for (field, value) in [
        ("version", serde_json::json!(2)),
        ("scheme", serde_json::json!("unsigned")),
        ("signature", serde_json::json!("00")),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        assert!(
            SignatureEnvelope::from_bytes(&serde_json::to_vec(&changed).unwrap())
                .unwrap()
                .verify(raw, None)
                .is_err()
        );
    }
    for field in ["signature", "public_key", "bundle_id"] {
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(SignatureEnvelope::from_bytes(&serde_json::to_vec(&missing).unwrap()).is_err());
    }
    for (field, value) in [
        ("bundle_id", hex::encode([0; 32])),
        (
            "public_key",
            hex::encode(signer(8).verifying_key().to_encoded_point(true)),
        ),
        ("signature", hex::encode([0; 65])),
    ] {
        let mut changed = original.clone();
        changed[field] = value.into();
        let parsed = SignatureEnvelope::from_bytes(&serde_json::to_vec(&changed).unwrap());
        assert!(
            parsed.is_err() || parsed.unwrap().verify(raw, None).is_err(),
            "{field}"
        );
    }
}

#[test]
fn noncanonical_high_s_and_invalid_recovery_do_not_authenticate() {
    let raw = b"manifest";
    let (signature, _) = sign(raw);
    let mut high = signature;
    let order =
        hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141").unwrap();
    let mut borrow = 0_i16;
    for i in (0..32).rev() {
        let difference = i16::from(order[i]) - i16::from(signature[32 + i]) - borrow;
        high[32 + i] = difference as u8;
        borrow = i16::from(difference < 0);
    }
    high[64] ^= 1;
    assert!(SignatureEnvelope::from_signature(raw, high).is_err());
    let mut invalid = signature;
    invalid[64] = 255;
    assert!(SignatureEnvelope::from_signature(raw, invalid).is_err());
}
