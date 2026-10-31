use super::*;
use clap::Parser;

fn make_cli(args: &[&str]) -> Cli {
    let mut full = vec!["keygen"];
    full.extend_from_slice(args);
    Cli::parse_from(full)
}

#[test]
fn show_address_requires_private_key() {
    assert!(Cli::try_parse_from(["keygen", "show-address", "--private-key", "hex-key",]).is_ok());
    assert!(Cli::try_parse_from(["keygen", "show-address"]).is_err());
    assert!(Cli::try_parse_from([
        "keygen",
        "show-address",
        "--key",
        "evm-key.hex",
        "--private-key",
        "hex-key",
    ])
    .is_err());
}

#[test]
fn show_address_matches_known_evm_address() {
    // Scalar one is public test material; derive its hex instead of storing a key.
    let mut scalar = [0u8; 32];
    scalar[31] = 1;
    let encoded = hex::encode(scalar);
    let expected: Address = "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf"
        .parse()
        .unwrap();
    for text in [
        encoded.clone(),
        format!("0x{encoded}"),
        format!("{encoded}\n"),
    ] {
        assert_eq!(show_address(text).unwrap(), expected);
    }
}

#[test]
fn show_address_rejects_invalid_keys() {
    for text in [
        String::new(),
        "ab".repeat(31),
        "ab".repeat(33),
        "zz".repeat(32),
        "00".repeat(32),
        "ff".repeat(32),
    ] {
        assert!(show_address(text).is_err());
    }
}

#[test]
fn test_resolve_backend_default_is_plaintext() {
    let cli = make_cli(&["generate", "--output-dir", "/tmp"]);
    let backend = resolve_backend(&cli).unwrap();
    assert!(matches!(backend, KeyBackend::Plaintext));
}

#[test]
fn radicle_subcommand_parses_without_bls_backend_inputs() {
    let cli = make_cli(&["radicle", "--output-dir", "/tmp/radicle-home"]);
    assert!(matches!(cli.command, Commands::Radicle { .. }));
}

#[test]
fn test_resolve_backend_plaintext_explicit() {
    let cli = make_cli(&[
        "--key-backend",
        "plaintext",
        "generate",
        "--output-dir",
        "/tmp",
    ]);
    let backend = resolve_backend(&cli).unwrap();
    assert!(matches!(backend, KeyBackend::Plaintext));
}

#[test]
fn test_resolve_backend_encrypted_with_passphrase() {
    let cli = make_cli(&[
        "--key-backend",
        "encrypted",
        "--passphrase",
        "mypass",
        "generate",
        "--output-dir",
        "/tmp",
    ]);
    let backend = resolve_backend(&cli).unwrap();
    assert!(matches!(backend, KeyBackend::Encrypted(ref p) if p == "mypass"));
}

#[test]
fn test_resolve_backend_encrypted_missing_passphrase() {
    let cli = make_cli(&[
        "--key-backend",
        "encrypted",
        "generate",
        "--output-dir",
        "/tmp",
    ]);
    assert!(resolve_backend(&cli).is_err());
}

#[test]
fn test_resolve_backend_os_level() {
    let cli = make_cli(&[
        "--key-backend",
        "os-level",
        "generate",
        "--output-dir",
        "/tmp",
    ]);
    let backend = resolve_backend(&cli).unwrap();
    assert!(matches!(backend, KeyBackend::OsLevel));
}

#[test]
fn test_resolve_backend_invalid_rejected_by_clap() {
    let result = Cli::try_parse_from([
        "keygen",
        "--key-backend",
        "bogus",
        "generate",
        "--output-dir",
        "/tmp",
    ]);
    assert!(result.is_err());
}

// --- TC-006: keygen command e2e tests ---

#[test]
fn test_cmd_generate_creates_key_file() {
    let dir = tempfile::tempdir().unwrap();
    cmd_generate(dir.path().to_path_buf(), &KeyBackend::Plaintext, false).unwrap();
    let key_path = dir.path().join("signing-key.hex");
    assert!(key_path.exists());
    let contents = std::fs::read_to_string(&key_path).unwrap();
    assert!(!contents.is_empty());
}

#[test]
fn radicle_command_creates_native_heartwood_identity_with_strict_modes() {
    use radicle_crypto::Signer as _;
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    cmd_radicle(dir.path().to_path_buf()).unwrap();

    let keys = dir.path().join("keys");
    let secret = keys.join("radicle");
    let public = keys.join("radicle.pub");
    assert_eq!(
        std::fs::metadata(&keys).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(public.is_file());

    let keystore = radicle_crypto::ssh::Keystore::new(&keys);
    let secret_key = keystore.secret_key(None).unwrap().unwrap();
    let public_key = keystore.public_key().unwrap().unwrap();
    assert_eq!(secret_key.public_key(), &public_key);
}

#[test]
fn radicle_command_refuses_to_overwrite_an_existing_identity() {
    let dir = tempfile::tempdir().unwrap();
    cmd_radicle(dir.path().to_path_buf()).unwrap();
    let first_secret = std::fs::read(dir.path().join("keys/radicle")).unwrap();

    assert!(cmd_radicle(dir.path().to_path_buf()).is_err());
    assert_eq!(
        std::fs::read(dir.path().join("keys/radicle")).unwrap(),
        first_secret
    );
}

#[test]
fn test_cmd_generate_encrypted_backend() {
    let dir = tempfile::tempdir().unwrap();
    let backend = KeyBackend::Encrypted("testpass".to_string());
    cmd_generate(dir.path().to_path_buf(), &backend, false).unwrap();
    assert!(dir.path().join("signing-key.hex").exists());
}

#[test]
fn test_cmd_show_pubkey_after_generate() {
    let dir = tempfile::tempdir().unwrap();
    cmd_generate(dir.path().to_path_buf(), &KeyBackend::Plaintext, false).unwrap();
    let key_path = dir.path().join("signing-key.hex");
    cmd_show_pubkey(key_path, &KeyBackend::Plaintext).unwrap();
}

#[test]
fn test_cmd_show_pubkey_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let result = cmd_show_pubkey(dir.path().join("nonexistent.hex"), &KeyBackend::Plaintext);
    assert!(result.is_err());
}

#[test]
fn test_cmd_sign_registration_valid() {
    let dir = tempfile::tempdir().unwrap();
    cmd_generate(dir.path().to_path_buf(), &KeyBackend::Plaintext, false).unwrap();
    let key_path = dir.path().join("signing-key.hex");
    let addr: Address = "0x1111111111111111111111111111111111111111"
        .parse()
        .unwrap();
    cmd_sign_registration(
        key_path,
        addr,
        1,
        B256::repeat_byte(0x11),
        &KeyBackend::Plaintext,
    )
    .unwrap();
}

#[test]
fn test_cmd_sign_registration_missing_key() {
    let dir = tempfile::tempdir().unwrap();
    let addr: Address = "0x1111111111111111111111111111111111111111"
        .parse()
        .unwrap();
    let result = cmd_sign_registration(
        dir.path().join("none.hex"),
        addr,
        1,
        B256::repeat_byte(0x11),
        &KeyBackend::Plaintext,
    );
    assert!(result.is_err());
}

// TC-007: verify signature end-to-end with registration DST
#[test]
fn sign_registration_verifies_with_dst() {
    let dir = tempfile::tempdir().unwrap();
    cmd_generate(dir.path().to_path_buf(), &KeyBackend::Plaintext, false).unwrap();
    let key_path = dir.path().join("signing-key.hex");
    let key = load_key(&key_path, &KeyBackend::Plaintext).unwrap();
    let pk = key.public_key();
    let pk_bytes = pk.encode();

    let addr: Address = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
        .parse()
        .unwrap();
    let chain_id = 54322345;

    // Sign the same way cmd_sign_registration does
    let sk_bytes = key.encode();
    let blst_sk = blst::min_pk::SecretKey::from_bytes(&sk_bytes).unwrap();
    let node_id = B256::repeat_byte(0x12);
    let message = validator_registration_message(chain_id, addr, node_id);
    let sig = blst_sk.sign(&message, VALIDATOR_REGISTRATION_DST, &[]);

    // Verify
    let blst_pk = blst::min_pk::PublicKey::from_bytes(&pk_bytes).unwrap();
    let result = sig.verify(
        true,
        &message,
        VALIDATOR_REGISTRATION_DST,
        &[],
        &blst_pk,
        true,
    );
    assert_eq!(result, blst::BLST_ERROR::BLST_SUCCESS);

    let other_chain_message = validator_registration_message(chain_id + 1, addr, node_id);
    let replay_result = sig.verify(
        true,
        &other_chain_message,
        VALIDATOR_REGISTRATION_DST,
        &[],
        &blst_pk,
        true,
    );
    assert_ne!(replay_result, blst::BLST_ERROR::BLST_SUCCESS);

    let other_node_message =
        validator_registration_message(chain_id, addr, B256::repeat_byte(0x13));
    let node_replay_result = sig.verify(
        true,
        &other_node_message,
        VALIDATOR_REGISTRATION_DST,
        &[],
        &blst_pk,
        true,
    );
    assert_ne!(node_replay_result, blst::BLST_ERROR::BLST_SUCCESS);
}

#[test]
fn test_cmd_verify_valid_key() {
    let dir = tempfile::tempdir().unwrap();
    cmd_generate(dir.path().to_path_buf(), &KeyBackend::Plaintext, false).unwrap();
    let key_path = dir.path().join("signing-key.hex");
    cmd_verify(key_path, &KeyBackend::Plaintext).unwrap();
}

#[test]
#[should_panic(expected = "exit(1)")]
fn test_cmd_verify_corrupted_file_exits() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("signing-key.hex");
    std::fs::write(&key_path, "not-a-valid-key-at-all").unwrap();
    // exit_process(1) in test mode panics with "exit(1)"
    let _ = cmd_verify(key_path, &KeyBackend::Plaintext);
}

// TC-006: hybrid
#[test]
fn test_cmd_hybrid_creates_both_keys() {
    let dir = tempfile::tempdir().unwrap();
    cmd_hybrid(dir.path().to_path_buf(), &KeyBackend::Plaintext, false).unwrap();
    assert!(dir.path().join("signing-key.hex").exists());
    assert!(dir.path().join("evm-key.hex").exists());
}

// TC-028: hybrid ECDSA key validity
#[test]
fn test_cmd_hybrid_ecdsa_key_is_valid_32_bytes() {
    let dir = tempfile::tempdir().unwrap();
    cmd_hybrid(dir.path().to_path_buf(), &KeyBackend::Plaintext, false).unwrap();
    let ecdsa_hex = std::fs::read_to_string(dir.path().join("evm-key.hex")).unwrap();
    assert_eq!(ecdsa_hex.len(), 64); // 32 bytes = 64 hex chars
    let ecdsa_bytes = hex::decode(&ecdsa_hex).unwrap();
    assert!(!ecdsa_bytes.iter().all(|&b| b == 0)); // non-zero
}

#[test]
fn validator_subcommand_parses_native_chain_identity() {
    let cli = make_cli(&["validator", "--output-dir", "/tmp/v", "--chain-id", "7"]);
    assert!(matches!(
        cli.command,
        Commands::Validator { chain_id: 7, .. }
    ));
}

#[test]
fn validator_bundle_creates_all_keys_and_valid_registration_signature() {
    let dir = tempfile::tempdir().unwrap();
    let chain_id = 54321;
    let bundle = generate_validator_bundle(dir.path(), chain_id, &KeyBackend::Plaintext).unwrap();

    // All on-disk artifacts exist.
    assert!(dir.path().join("signing-key.hex").is_file());
    assert!(dir.path().join("evm-key.hex").is_file());
    assert!(dir.path().join("radicle/keys/radicle").is_file());
    assert!(dir.path().join("radicle/keys/radicle.pub").is_file());

    // The validator address is derivable from the written EVM key file.
    let evm_hex = std::fs::read_to_string(dir.path().join("evm-key.hex")).unwrap();
    let evm_key = SigningKey::from_slice(&hex::decode(evm_hex.trim()).unwrap()).unwrap();
    let point = evm_key.verifying_key().to_encoded_point(false);
    assert_eq!(
        bundle.validator_address,
        Address::from_raw_public_key(&point.as_bytes()[1..])
    );

    // The BLS public key matches the written signing key file.
    let bls_key = load_key(&bundle.bls_key_path, &KeyBackend::Plaintext).unwrap();
    assert_eq!(
        bls_key.public_key().encode().as_ref(),
        bundle.bls_public_key
    );

    // The registration signature verifies for the bundle's own binding.
    let message = validator_registration_message(
        chain_id,
        bundle.validator_address,
        bundle.radicle.node_id_bytes,
    );
    let blst_pk = blst::min_pk::PublicKey::from_bytes(&bundle.bls_public_key).unwrap();
    let sig = blst::min_pk::Signature::from_bytes(&bundle.registration_signature).unwrap();
    assert_eq!(
        sig.verify(
            true,
            &message,
            VALIDATOR_REGISTRATION_DST,
            &[],
            &blst_pk,
            true
        ),
        blst::BLST_ERROR::BLST_SUCCESS
    );
}

#[test]
fn validator_command_refuses_existing_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("evm-key.hex"), "0011").unwrap();
    let result = cmd_validator(dir.path().to_path_buf(), 1, &KeyBackend::Plaintext);
    assert!(result.is_err());
    // The command wrote nothing else next to the pre-existing artifact.
    assert!(!dir.path().join("signing-key.hex").exists());
    assert!(!dir.path().join("radicle").exists());
    // The pre-existing file is untouched.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("evm-key.hex")).unwrap(),
        "0011"
    );
}

// TC-008: encrypted save/load roundtrip
#[test]
fn test_encrypted_generate_and_show() {
    let dir = tempfile::tempdir().unwrap();
    let backend = KeyBackend::Encrypted("secret123".to_string());
    cmd_generate(dir.path().to_path_buf(), &backend, false).unwrap();
    let key_path = dir.path().join("signing-key.hex");
    cmd_show_pubkey(key_path.clone(), &backend).unwrap();
    // Wrong passphrase fails
    let wrong_backend = KeyBackend::Encrypted("wrong".to_string());
    assert!(cmd_show_pubkey(key_path, &wrong_backend).is_err());
}

fn keygen_error(err: &eyre::Report) -> &KeygenError {
    if let Some(error) = err.downcast_ref::<KeygenError>() {
        return error;
    }
    err.chain()
        .find_map(|source| source.downcast_ref::<KeygenError>())
        .unwrap_or_else(|| panic!("expected KeygenError, got: {err:#}"))
}

fn assert_key_file_already_exists(err: &eyre::Report, path: &Path) {
    match keygen_error(err) {
        KeygenError::KeyFileAlreadyExists(found) => assert_eq!(found, path),
        other => panic!(
            "expected KeyFileAlreadyExists for {}, got {other:?}",
            path.display()
        ),
    }
}

fn write_passphrase_file(dir: &Path, contents: &str, mode: u32) -> PathBuf {
    let path = dir.join("passphrase.txt");
    std::fs::write(&path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
    }
    path
}

fn encrypted_cli(extra: &[&str]) -> Cli {
    let mut args = vec!["--key-backend", "encrypted"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["generate", "--output-dir", "/tmp"]);
    make_cli(&args)
}

// T-01: refuse to overwrite an existing signing key unless --force is set.
#[test]
fn test_cmd_generate_refuses_existing_key() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();
    cmd_generate(dir_path.clone(), &KeyBackend::Plaintext, false).unwrap();
    let key_path = dir.path().join("signing-key.hex");
    let first_bytes = std::fs::read(&key_path).unwrap();

    let err = cmd_generate(dir_path, &KeyBackend::Plaintext, false).unwrap_err();
    assert_key_file_already_exists(&err, &key_path);
    assert_eq!(std::fs::read(&key_path).unwrap(), first_bytes);
}

#[test]
fn test_cmd_generate_force_replaces_key() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();
    cmd_generate(dir_path.clone(), &KeyBackend::Plaintext, false).unwrap();
    let key_path = dir.path().join("signing-key.hex");
    let first_bytes = std::fs::read(&key_path).unwrap();

    cmd_generate(dir_path, &KeyBackend::Plaintext, true).unwrap();
    let second_bytes = std::fs::read(&key_path).unwrap();
    assert_ne!(second_bytes, first_bytes);
}

#[test]
fn test_cmd_hybrid_refuses_existing_key_before_writing() {
    for (existing_name, missing_name) in [
        ("evm-key.hex", "signing-key.hex"),
        ("signing-key.hex", "evm-key.hex"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join(existing_name);
        let marker = format!("preexisting-{existing_name}");
        std::fs::write(&existing, &marker).unwrap();

        let err = cmd_hybrid(dir.path().to_path_buf(), &KeyBackend::Plaintext, false).unwrap_err();
        assert_key_file_already_exists(&err, &existing);
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), marker);
        assert!(!dir.path().join(missing_name).exists());
    }
}

#[test]
fn test_ensure_can_write_paths_interactive_confirm() {
    assert!(is_affirmative_answer("y\n"));
    assert!(is_affirmative_answer("YES\r\n"));
    assert!(!is_affirmative_answer(""));
    assert!(!is_affirmative_answer("n\n"));

    let dir = tempfile::tempdir().unwrap();
    let signing = dir.path().join("signing-key.hex");
    std::fs::write(&signing, b"signing").unwrap();

    ensure_can_write_paths(&[&signing], false, |_path| -> eyre::Result<bool> {
        Ok(true)
    })
    .unwrap();

    let err = ensure_can_write_paths(&[&signing], false, |_path| -> eyre::Result<bool> {
        Ok(false)
    })
    .unwrap_err();
    assert_key_file_already_exists(&err, &signing);

    let evm = dir.path().join("evm-key.hex");
    std::fs::write(&evm, b"evm").unwrap();
    let mut calls = 0;
    let answers = [true, false];
    let err = ensure_can_write_paths(&[&signing, &evm], false, |_path| -> eyre::Result<bool> {
        let answer = answers[calls];
        calls += 1;
        Ok(answer)
    })
    .unwrap_err();
    assert_eq!(calls, 2);
    assert_key_file_already_exists(&err, &evm);
}

// T-02: evm-key.hex is mode 0600 and the temp file is gone.
#[cfg(unix)]
#[test]
fn test_cmd_hybrid_evm_key_is_mode_600_and_leaves_no_tmp_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    cmd_hybrid(dir.path().to_path_buf(), &KeyBackend::Plaintext, false).unwrap();

    let evm_key = dir.path().join("evm-key.hex");
    let mode = std::fs::metadata(&evm_key).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with(".evm-key.hex.tmp."))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

// T-04: passphrase CLI, env, and file.
#[test]
fn test_passphrase_arg_preserves_bls_passphrase_env() {
    let cmd = <Cli as clap::CommandFactory>::command();
    let passphrase = cmd
        .get_arguments()
        .find(|arg| arg.get_id() == "passphrase")
        .expect("passphrase argument");
    assert_eq!(
        passphrase.get_env(),
        Some(std::ffi::OsStr::new("BLS_PASSPHRASE"))
    );
}

#[test]
fn test_resolve_backend_reads_passphrase_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_passphrase_file(dir.path(), "secret 123 \n", 0o600);
    let cli = encrypted_cli(&["--passphrase-file", path.to_str().unwrap()]);
    let backend = resolve_backend(&cli).unwrap();
    assert!(matches!(backend, KeyBackend::Encrypted(p) if p == "secret 123 "));
}

#[cfg(unix)]
#[test]
fn test_resolve_backend_rejects_group_readable_passphrase_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_passphrase_file(dir.path(), "secret123\n", 0o640);
    let cli = encrypted_cli(&["--passphrase-file", path.to_str().unwrap()]);
    let err = resolve_backend(&cli).unwrap_err();
    assert!(matches!(
        keygen_error(&err),
        KeygenError::InsecurePassphraseFileMode {
            path: found,
            mode: 0o640
        } if found == &path
    ));
}

#[test]
fn test_resolve_backend_rejects_empty_passphrase_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_passphrase_file(dir.path(), "\n", 0o600);
    let cli = encrypted_cli(&["--passphrase-file", path.to_str().unwrap()]);
    let err = resolve_backend(&cli).unwrap_err();
    assert!(matches!(
        keygen_error(&err),
        KeygenError::EmptyPassphraseFile(found) if found == &path
    ));
}

#[test]
fn test_resolve_backend_passphrase_file_takes_precedence_over_passphrase() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_passphrase_file(dir.path(), "from-file\n", 0o600);
    let cli = encrypted_cli(&[
        "--passphrase",
        "from-cli",
        "--passphrase-file",
        path.to_str().unwrap(),
    ]);
    let backend = resolve_backend(&cli).unwrap();
    assert!(matches!(backend, KeyBackend::Encrypted(p) if p == "from-file"));
}

// T-06: roundtrip failure takes the second exit_process branch.
#[test]
#[should_panic(expected = "exit(1)")]
fn test_cmd_verify_roundtrip_failure_exits() {
    let dir = tempfile::tempdir().unwrap();
    cmd_generate(dir.path().to_path_buf(), &KeyBackend::Plaintext, false).unwrap();
    let key_path = dir.path().join("signing-key.hex");
    let _ = cmd_verify_with_roundtrip(key_path, &KeyBackend::Plaintext, |_, _| false);
}
