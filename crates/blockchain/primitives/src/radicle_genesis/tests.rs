use super::*;

#[test]
fn publication_window_matches_effective_dkg_freeze_cadence() {
    for (epoch, prepare) in [(300, 30), (65, 1), (64, 0), (u64::MAX, 0)] {
        assert!(validate_publication_window(epoch, prepare).is_ok());
    }
    for (epoch, prepare) in [
        (0, 0),
        (63, 0),
        (64, 1),
        (65, 2),
        (300, 237),
        (300, 300),
        (300, u64::MAX),
    ] {
        assert!(validate_publication_window(epoch, prepare).is_err());
    }
}

fn descriptor() -> GovernmentsDescriptor {
    GovernmentsDescriptor {
        chain_id: 2026,
        genesis_timestamp: 1700000000,
        description: "Network governance".into(),
        initial_tree_oid: "0x4b825dc642cb6eb9a060e54bf8d69288fbee4904"
            .parse()
            .unwrap(),
        initial_content_manifest_sha256:
            "0x4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
                .parse()
                .unwrap(),
    }
}

#[test]
fn ogd1_matches_frozen_p0_preimage_and_hash_without_genesis_hash_or_future_key() {
    let descriptor = descriptor();
    let bytes = descriptor.to_bytes().unwrap();
    let seeded_hash = descriptor.hash().unwrap();
    assert_eq!(hex::encode(&bytes), "4f474431000100000000000007ea000000006553f100000b676f7665726e6d656e747300046d61696e00124e6574776f726b20676f7665726e616e63654b825dc642cb6eb9a060e54bf8d69288fbee49044f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b9450101");
    assert_eq!(
        hex::encode(descriptor.hash().unwrap()),
        "0349a0178f4f477562b46c3c9690408ff65cdeb41d059898d38b85791fa92bcf"
    );
    assert_eq!(
        GovernmentsDescriptor::from_bytes(&bytes).unwrap(),
        descriptor
    );
    let field = serde_json::Value::String(format!("0x{}", hex::encode(&bytes)));
    assert_eq!(
        GovernmentsDescriptor::from_genesis_field(Some(&field), 2026, 1700000000, seeded_hash)
            .unwrap(),
        Some(descriptor)
    );
    assert!(
        GovernmentsDescriptor::from_genesis_field(None, 2026, 1700000000, B256::ZERO)
            .unwrap()
            .is_none()
    );
    assert!(
        GovernmentsDescriptor::from_genesis_field(Some(&field), 2027, 1700000000, seeded_hash)
            .is_err()
    );
    assert!(
        GovernmentsDescriptor::from_genesis_field(Some(&field), 2026, 1700000001, seeded_hash)
            .is_err()
    );
}

#[test]
fn descriptor_rejects_bad_version_name_branch_policy_utf8_missing_commitments_and_lengths() {
    let original = descriptor();
    let bytes = original.to_bytes().unwrap();
    for length in 0..bytes.len() {
        assert!(GovernmentsDescriptor::from_bytes(&bytes[..length]).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(GovernmentsDescriptor::from_bytes(&trailing).is_err());
    for index in [0, 5, 23, 24, 36, 37, 43, bytes.len() - 2, bytes.len() - 1] {
        let mut invalid = bytes.clone();
        invalid[index] = 255;
        assert!(GovernmentsDescriptor::from_bytes(&invalid).is_err());
    }
    for description in [
        "bad\rtext".to_owned(),
        "bad\0text".to_owned(),
        "x".repeat(1025),
    ] {
        let mut invalid = original.clone();
        invalid.description = description;
        assert!(invalid.to_bytes().is_err());
    }
    let mut maximum = original.clone();
    maximum.description = "x".repeat(1024);
    assert_eq!(
        maximum.to_bytes().unwrap().len(),
        MAX_GOVERNMENTS_DESCRIPTOR_BYTES
    );
    assert_eq!(
        GovernmentsDescriptor::from_bytes(&maximum.to_bytes().unwrap()).unwrap(),
        maximum
    );
    let mut invalid = original.clone();
    invalid.initial_tree_oid = FixedBytes::ZERO;
    assert!(invalid.to_bytes().is_err());
    let mut invalid = original;
    invalid.initial_content_manifest_sha256 = B256::ZERO;
    assert!(invalid.to_bytes().is_err());
    for field in [
        serde_json::Value::Bool(false),
        serde_json::Value::String("abcd".into()),
        serde_json::Value::String("0xgg".into()),
        serde_json::Value::String(format!("0x{}", "00".repeat(1122))),
    ] {
        assert!(GovernmentsDescriptor::from_genesis_field(
            Some(&field),
            2026,
            1700000000,
            B256::ZERO
        )
        .is_err());
    }
}
