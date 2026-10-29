use super::*;
use tempfile::NamedTempFile;

#[test]
fn test_bootstrap_dkg_3_validators() {
    let result = bootstrap_dkg(3).unwrap();
    assert_eq!(result.shares.len(), 3);

    // Each share should validate against the polynomial
    for share in &result.shares {
        assert!(validate_share(share, &result.polynomial).unwrap());
    }
}

#[test]
fn test_bootstrap_dkg_128_validators() {
    let result = bootstrap_dkg(128).unwrap();
    assert_eq!(result.shares.len(), 128);

    // Spot-check first and last share
    assert!(validate_share(&result.shares[0], &result.polynomial).unwrap());
    assert!(validate_share(&result.shares[127], &result.polynomial).unwrap());
}

#[test]
fn test_individual_key_roundtrip_file() {
    use commonware_math::algebra::Random;
    let key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let backend = KeyBackend::Plaintext;

    let file = NamedTempFile::new().unwrap();
    save_individual_key(file.path(), &key, &backend).unwrap();
    let loaded = load_individual_key(file.path(), &backend).unwrap();

    assert_eq!(key, loaded);
}

#[test]
fn test_individual_key_sign_verify() {
    use commonware_cryptography::{Signer as _, Verifier as _};
    use commonware_math::algebra::Random;

    let key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let pk = key.public_key();

    let sig = key.sign(b"test-ns", b"hello");
    assert!(pk.verify(b"test-ns", b"hello", &sig));
    assert!(!pk.verify(b"test-ns", b"wrong", &sig));
}

#[test]
fn test_polynomial_roundtrip_file() {
    let result = bootstrap_dkg(3).unwrap();
    let backend = KeyBackend::Plaintext;

    let file = NamedTempFile::new().unwrap();
    save_public_polynomial(file.path(), &result.polynomial, &backend).unwrap();
    let loaded = load_public_polynomial(file.path(), &backend).unwrap();

    // Loaded polynomial should produce the same partial public keys
    for share in &result.shares {
        assert!(validate_share(share, &loaded).unwrap());
    }
}

#[test]
fn encrypted_backend_atomically_replaces_existing_secret() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("retry.hex");
    let backend = KeyBackend::Encrypted("replacement-password".into());

    save_raw(&path, b"first", &backend).unwrap();
    save_raw(&path, b"second", &backend).unwrap();

    assert_eq!(load_raw(&path, &backend).unwrap(), b"second");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn test_plaintext_backend_roundtrip() {
    let result = bootstrap_dkg(3).unwrap();
    let share = &result.shares[0];
    let backend = KeyBackend::Plaintext;

    let file = NamedTempFile::new().unwrap();
    save_signing_share(file.path(), share, &backend).unwrap();

    // File should be hex text
    let content = std::fs::read_to_string(file.path()).unwrap();
    assert!(
        hex::decode(content.trim()).is_ok(),
        "plaintext file should be valid hex"
    );

    let loaded = load_signing_share(file.path(), &backend).unwrap();
    assert_eq!(share.index, loaded.index);
    assert!(validate_share(&loaded, &result.polynomial).unwrap());

    // Legacy raw hex string without trailing newline also loads cleanly.
    let legacy_file = NamedTempFile::new().unwrap();
    std::fs::write(legacy_file.path(), hex::encode(share.encode())).unwrap();
    let legacy_loaded = load_signing_share(legacy_file.path(), &KeyBackend::Plaintext).unwrap();
    assert_eq!(share.index, legacy_loaded.index);
    assert!(validate_share(&legacy_loaded, &result.polynomial).unwrap());
}

#[cfg(unix)]
#[test]
fn test_plaintext_backend_writes_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let result = bootstrap_dkg(3).unwrap();
    let share = &result.shares[0];
    let backend = KeyBackend::Plaintext;

    let file = NamedTempFile::new().unwrap();
    std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    save_signing_share(file.path(), share, &backend).unwrap();

    let mode = std::fs::metadata(file.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn test_encrypted_individual_key_roundtrip() {
    use commonware_math::algebra::Random;
    let key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let backend = KeyBackend::Encrypted("key-secret".into());

    let file = NamedTempFile::new().unwrap();
    save_individual_key(file.path(), &key, &backend).unwrap();
    let loaded = load_individual_key(file.path(), &backend).unwrap();

    assert_eq!(key, loaded);
}

/// Runtime-level test: save and load full threshold material (share + polynomial)
/// through the encrypted backend. This is the same flow as save_dkg_state() /
/// obtain_threshold_material().
#[test]
fn test_encrypted_threshold_material_full_flow() {
    let result = bootstrap_dkg(3).unwrap();
    let backend = KeyBackend::Encrypted("threshold-secret-123".into());

    let dir = tempfile::tempdir().unwrap();
    let share_path = dir.path().join("dkg_share.hex");
    let poly_path = dir.path().join("dkg_polynomial.hex");

    // Save (same calls as `save_dkg_state` in `outbe-engine` `stack::dkg::persistence`).
    save_signing_share(&share_path, &result.shares[0], &backend).unwrap();
    save_public_polynomial(&poly_path, &result.polynomial, &backend).unwrap();

    // Files must be encrypted (not plaintext hex).
    let share_raw = std::fs::read(&share_path).unwrap();
    let poly_raw = std::fs::read(&poly_path).unwrap();
    assert!(share_raw.starts_with(ENC_MAGIC), "share must be encrypted");
    assert!(
        poly_raw.starts_with(ENC_MAGIC),
        "polynomial must be encrypted"
    );

    // Load (same calls as `obtain_threshold_material` Path 2 in
    // `outbe-engine` `stack::dkg::startup`).
    let loaded_share = load_signing_share(&share_path, &backend).unwrap();
    let loaded_poly = load_public_polynomial(&poly_path, &backend).unwrap();

    // Validate roundtrip across all shares.
    assert_eq!(result.shares[0].index, loaded_share.index);
    assert!(validate_share(&loaded_share, &loaded_poly).unwrap());
    for share in &result.shares {
        assert!(validate_share(share, &loaded_poly).unwrap());
    }

    // Cannot load with wrong passphrase.
    let wrong_backend = KeyBackend::Encrypted("wrong-password".into());
    assert!(load_signing_share(&share_path, &wrong_backend).is_err());
    assert!(load_public_polynomial(&poly_path, &wrong_backend).is_err());

    // Cannot load with plaintext backend (encrypted file).
    assert!(load_signing_share(&share_path, &KeyBackend::Plaintext).is_err());
}

fn assert_bls_key_error(error: eyre::Report, expected: BlsKeyError) {
    let actual = error
        .downcast_ref::<BlsKeyError>()
        .unwrap_or_else(|| panic!("expected BlsKeyError, got {error}"));
    assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
}

#[test]
fn bootstrap_dkg_rejects_count_above_load_ceiling() {
    let Err(error) = bootstrap_dkg(MAX_VALIDATORS + 1) else {
        panic!("count above the ceiling must fail before generation");
    };
    assert_bls_key_error(
        error,
        BlsKeyError::ValidatorCountExceedsMax {
            count: MAX_VALIDATORS + 1,
            max: MAX_VALIDATORS,
        },
    );
}

#[test]
fn bootstrap_dkg_for_participants_rejects_count_above_load_ceiling() {
    use commonware_cryptography::Signer as _;
    use commonware_math::algebra::Random;
    use commonware_utils::ordered::Set;

    let keys = (0..=MAX_VALIDATORS)
        .map(|_| {
            bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                rand_commonware::rngs::SysRng,
            ))
            .public_key()
        })
        .collect::<Vec<_>>();
    let participants = Set::from_iter_dedup(keys);
    let count = u32::try_from(participants.len()).unwrap();
    let Err(error) = bootstrap_dkg_for_participants(participants) else {
        panic!("count above the ceiling must fail before generation");
    };
    assert_bls_key_error(
        error,
        BlsKeyError::ValidatorCountExceedsMax {
            count,
            max: MAX_VALIDATORS,
        },
    );
}

#[test]
fn foreign_share_and_foreign_output_keep_distinct_rejections() {
    use commonware_cryptography::Signer as _;
    use commonware_math::algebra::Random;
    use commonware_utils::ordered::Set;

    let random_key = || {
        bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
            rand_commonware::rngs::SysRng,
        ))
        .public_key()
    };
    let left_keys = (0..2).map(|_| random_key()).collect::<Vec<_>>();
    let right_keys = (0..2).map(|_| random_key()).collect::<Vec<_>>();
    let left = bootstrap_dkg_for_participants(Set::from_iter_dedup(left_keys)).unwrap();
    let right = bootstrap_dkg_for_participants(Set::from_iter_dedup(right_keys)).unwrap();

    let share_error =
        validate_dkg_triplet(&right.shares[0], &left.polynomial, &left.output).unwrap_err();
    assert_bls_key_error(share_error, BlsKeyError::SigningSharePolynomialMismatch);

    let output_error =
        validate_dkg_triplet(&left.shares[0], &left.polynomial, &right.output).unwrap_err();
    assert_bls_key_error(output_error, BlsKeyError::DkgOutputPolynomialMismatch);
}

#[test]
fn encrypted_backend_debug_redacts_passphrase() {
    let secret = "passphrase-should-not-appear";
    let backend = KeyBackend::Encrypted(secret.to_string());
    let rendered = format!("{backend:?}");
    assert!(
        !rendered.contains(secret),
        "Debug rendered the passphrase: {rendered}"
    );
    assert!(
        rendered.contains("Encrypted"),
        "Debug dropped the backend name: {rendered}"
    );
}

fn os_level_file(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    path
}

#[test]
fn keychain_resave_deletes_prior_account_and_keeps_current() {
    let guard = install_test_keychain();
    let dir = tempfile::tempdir().unwrap();
    let path = os_level_file(dir.path(), "dkg_polynomial.hex");
    let backend = KeyBackend::OsLevel;

    save_raw(&path, b"payload-A", &backend).unwrap();
    let prior_account = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .nth(2)
        .unwrap()
        .to_owned();
    save_raw(&path, b"payload-B", &backend).unwrap();

    assert_eq!(load_raw(&path, &backend).unwrap(), b"payload-B");
    assert!(
        !guard.contains_account("outbe-chain", &prior_account),
        "resave left the superseded account {prior_account}"
    );
    assert_eq!(guard.len(), 1);
}

#[test]
fn keychain_resave_identical_bytes_preserves_account() {
    let guard = install_test_keychain();
    let dir = tempfile::tempdir().unwrap();
    let path = os_level_file(dir.path(), "dkg_polynomial.hex");
    let backend = KeyBackend::OsLevel;

    save_raw(&path, b"payload-B", &backend).unwrap();
    let account = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .nth(2)
        .unwrap()
        .to_owned();
    save_raw(&path, b"payload-B", &backend).unwrap();

    assert_eq!(load_raw(&path, &backend).unwrap(), b"payload-B");
    assert!(guard.contains_account("outbe-chain", &account));
    assert_eq!(guard.len(), 1);
}

#[test]
fn keychain_resave_across_distinct_paths_with_same_stem_and_data_stays_isolated() {
    let guard = install_test_keychain();
    let dir = tempfile::tempdir().unwrap();
    let validator_0 = os_level_file(dir.path(), "validator-0/keys/dkg_polynomial.hex");
    let validator_1 = os_level_file(dir.path(), "validator-1/keys/dkg_polynomial.hex");
    let backend = KeyBackend::OsLevel;

    save_raw(&validator_0, b"payload-A", &backend).unwrap();
    save_raw(&validator_1, b"payload-A", &backend).unwrap();
    let validator_1_account = std::fs::read_to_string(&validator_1)
        .unwrap()
        .lines()
        .nth(2)
        .unwrap()
        .to_owned();
    save_raw(&validator_0, b"payload-B", &backend).unwrap();

    assert_eq!(load_raw(&validator_0, &backend).unwrap(), b"payload-B");
    assert_eq!(load_raw(&validator_1, &backend).unwrap(), b"payload-A");
    assert!(guard.contains_account("outbe-chain", &validator_1_account));
    assert_eq!(guard.len(), 2);
}

#[test]
fn keychain_resave_preserves_legacy_untagged_shared_account() {
    let guard = install_test_keychain();
    let dir = tempfile::tempdir().unwrap();
    let path = os_level_file(dir.path(), "dkg_polynomial.hex");
    let legacy_bytes = b"payload-A";
    let legacy_account = format!(
        "dkg_polynomial-{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, legacy_bytes))
    );
    keychain_set_password("outbe-chain", &legacy_account, &hex::encode(legacy_bytes)).unwrap();
    std::fs::write(
        &path,
        format!("OUTBE_KEYCHAIN\noutbe-chain\n{legacy_account}\n"),
    )
    .unwrap();

    save_raw(&path, b"payload-B", &KeyBackend::OsLevel).unwrap();

    assert_eq!(load_raw(&path, &KeyBackend::OsLevel).unwrap(), b"payload-B");
    assert!(
        guard.contains_account("outbe-chain", &legacy_account),
        "resave deleted the legacy untagged account"
    );
}

#[test]
fn keychain_resave_overwrites_malformed_marker_and_remove_raw_fails_closed() {
    let _guard = install_test_keychain();
    let dir = tempfile::tempdir().unwrap();
    let path = os_level_file(dir.path(), "dkg_polynomial.hex");
    let malformed = b"OUTBE_KEYCHAIN\nnot-a-marker";
    std::fs::write(&path, malformed).unwrap();

    save_raw(&path, b"payload-B", &KeyBackend::OsLevel).unwrap();
    assert_eq!(load_raw(&path, &KeyBackend::OsLevel).unwrap(), b"payload-B");

    std::fs::write(&path, malformed).unwrap();
    let error = remove_raw(&path, &KeyBackend::OsLevel).unwrap_err();
    assert_bls_key_error(error, BlsKeyError::InvalidKeychainMarker);
    assert_eq!(std::fs::read(&path).unwrap(), malformed);
}

#[test]
fn keychain_resave_succeeds_when_prune_delete_fails() {
    let guard = install_test_keychain();
    let dir = tempfile::tempdir().unwrap();
    let path = os_level_file(dir.path(), "dkg_polynomial.hex");
    let backend = KeyBackend::OsLevel;

    save_raw(&path, b"payload-A", &backend).unwrap();
    let prior_account = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .nth(2)
        .unwrap()
        .to_owned();
    guard.fail_next_delete();
    save_raw(&path, b"payload-B", &backend).unwrap();

    assert_eq!(load_raw(&path, &backend).unwrap(), b"payload-B");
    assert!(guard.contains_account("outbe-chain", &prior_account));
    assert_eq!(guard.len(), 2);
}
