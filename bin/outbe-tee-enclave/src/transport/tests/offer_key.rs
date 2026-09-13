use crate::transport::tests::*;

#[test]
fn persistence_failure_never_activates_offer_key() {
    let cfg = EnclaveBootConfig::new(
        testnet_chain_word(),
        std::path::PathBuf::from("/dev/null"),
        1,
    );
    let binding = sealed_test_binding(testnet_chain_word());
    let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
    let derived = DerivedTributeOfferKey::from_secret_and_group_sig(
        Zeroizing::new([0xA1; 32]),
        Zeroizing::new(vec![0xA2; 96]),
    );

    assert!(persist_then_activate_offer_key(&cfg, binding, &offer_key, derived).is_err());
    assert!(
        offer_key.get().is_none(),
        "failed durable persistence must leave the resident slot keyless"
    );
}

/// Seal the DKG-derived offer key + group signature, then a fresh boot unseals
/// and restores the byte-identical offer public key AND the group signature -
/// the restart fast-path that skips the ceremony.
#[test]
fn ws_c_seal_then_unseal_restores_tribute_offer_key_and_group_sig() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EnclaveBootConfig::new([0xCD; 32], dir.path().to_path_buf(), 2);
    let binding = sealed_test_binding([0xCD; 32]);
    let group_sig = vec![0x9b_u8; 96];
    let (offer_key, public) = install_tribute_offer_key([0x5a; 32], group_sig.clone());

    persist_test_offer_key(&cfg, binding, &offer_key).unwrap();
    assert!(cfg.sealed_root_path().exists(), "sealed blob written");

    let restored = unseal_tribute_offer_and_group_sig_on_boot(&cfg, binding)
        .expect("read sealed state")
        .expect("unseal on boot");
    assert_eq!(restored.public(), public);
    assert_eq!(
        restored.group_sig(),
        group_sig.as_slice(),
        "group signature restored"
    );
}

/// vA seals at SVN 1; a vB enclave of the SAME signer (same mock MRSIGNER key,
/// different build) boots at SVN 2 and unseals vA's blob - cross-version
/// unseal with the anti-rollback floor satisfied.
#[test]
fn ws_c_cross_version_unseal_same_signer_key() {
    let dir = tempfile::tempdir().unwrap();
    let chain = [0xAB; 32];
    let cfg_a = EnclaveBootConfig::new(chain, dir.path().to_path_buf(), 1);
    let binding = sealed_test_binding(chain);
    let (offer_key, public) = install_tribute_offer_key([0x77; 32], vec![0x11; 36]);
    persist_test_offer_key(&cfg_a, binding, &offer_key).unwrap();

    let cfg_b = EnclaveBootConfig::new(chain, dir.path().to_path_buf(), 2);
    let restored = unseal_tribute_offer_and_group_sig_on_boot(&cfg_b, binding)
        .expect("read sealed state")
        .expect("vB unseals vA blob");
    assert_eq!(restored.public(), public);
}

/// A blob sealed for one chain does not unseal under a different `chain_id`
/// (it is bound into the AEAD AAD).
#[test]
fn ws_c_unseal_rejects_wrong_chain_id() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EnclaveBootConfig::new([0x01; 32], dir.path().to_path_buf(), 1);
    let binding = sealed_test_binding([0x01; 32]);
    let (offer_key, _public) = install_tribute_offer_key([0x33; 32], vec![0x22; 36]);
    persist_test_offer_key(&cfg, binding, &offer_key).unwrap();

    let cfg_wrong = EnclaveBootConfig::new([0x02; 32], dir.path().to_path_buf(), 1);
    let error = match unseal_tribute_offer_and_group_sig_on_boot(
        &cfg_wrong,
        sealed_test_binding([0x02; 32]),
    ) {
        Err(error) => error,
        Ok(_) => panic!("wrong-chain sealed state must fail closed"),
    };
    assert!(error.contains("lost its key"), "{error}");
    assert!(error.contains("no recovery or fallback"), "{error}");
}

#[test]
fn ws_c_missing_blob_is_the_only_keyless_boot_result() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EnclaveBootConfig::new([0x18; 32], dir.path().to_path_buf(), 1);
    assert!(
        unseal_tribute_offer_and_group_sig_on_boot(&cfg, sealed_test_binding([0x18; 32]))
            .expect("missing file is not corruption")
            .is_none()
    );
}

#[test]
fn ws_c_corrupt_blob_is_terminal_and_is_not_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EnclaveBootConfig::new([0x19; 32], dir.path().to_path_buf(), 1);
    let corrupt = b"corrupt-permanent-offer-key";
    std::fs::write(cfg.sealed_root_path(), corrupt).unwrap();

    let error =
        match unseal_tribute_offer_and_group_sig_on_boot(&cfg, sealed_test_binding([0x19; 32])) {
            Err(error) => error,
            Ok(_) => panic!("corrupt sealed state must fail closed"),
        };
    assert!(error.contains("lost its key"), "{error}");
    assert!(error.contains("no recovery or fallback"), "{error}");
    assert_eq!(std::fs::read(cfg.sealed_root_path()).unwrap(), corrupt);
}

/// Sealing is write-once: a second call cannot replace the persisted key.
#[test]
fn ws_c_seal_is_write_once() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EnclaveBootConfig::new([0x09; 32], dir.path().to_path_buf(), 1);
    let binding = sealed_test_binding([0x09; 32]);
    let (offer_key, public) = install_tribute_offer_key([0x44; 32], vec![0x33; 36]);
    persist_test_offer_key(&cfg, binding, &offer_key).unwrap();
    let first = std::fs::read(cfg.sealed_root_path()).unwrap();

    // A different resident key must not overwrite the existing blob.
    let (offer_key2, _other) = install_tribute_offer_key([0x55; 32], vec![0x44; 36]);
    assert!(persist_test_offer_key(&cfg, binding, &offer_key2).is_err());
    let second = std::fs::read(cfg.sealed_root_path()).unwrap();
    assert_eq!(first, second, "seal is write-once");
    // The persisted key is still the first one.
    assert_eq!(
        unseal_tribute_offer_and_group_sig_on_boot(&cfg, binding)
            .unwrap()
            .unwrap()
            .public(),
        public
    );
}

/// The encoded share is only present inside the AEAD ciphertext - it never
/// appears as plaintext bytes in the on-disk blob (secret-at-rest invariant).
#[test]
fn ws_c_share_never_in_host_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EnclaveBootConfig::new([0x07; 32], dir.path().to_path_buf(), 1);
    let binding = sealed_test_binding([0x07; 32]);
    let share = vec![0xC3_u8; 40];
    let (offer_key, _public) = install_tribute_offer_key([0x66; 32], share.clone());
    persist_test_offer_key(&cfg, binding, &offer_key).unwrap();

    let blob = std::fs::read(cfg.sealed_root_path()).unwrap();
    assert!(
        !blob.windows(share.len()).any(|w| w == share.as_slice()),
        "raw share bytes must not appear in the sealed blob"
    );
}
