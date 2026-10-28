// --- parse_dkg_key_backend ---

fn make_dkg_cli(args: &[&str]) -> super::DkgCli {
    use clap::Parser;
    let mut full = vec!["cmd"];
    full.extend_from_slice(args);
    super::DkgCli::parse_from(full)
}

#[test]
fn test_parse_dkg_key_backend_plaintext() {
    let cli = make_dkg_cli(&[
        "--bls-key-backend",
        "plaintext",
        "status",
        "--storage-dir",
        "/tmp",
    ]);
    let backend = super::parse_dkg_key_backend(&cli).unwrap();
    assert!(matches!(
        backend,
        outbe_consensus::bls::KeyBackend::Plaintext
    ));
}

#[test]
fn test_parse_dkg_key_backend_default_is_plaintext() {
    let cli = make_dkg_cli(&["status", "--storage-dir", "/tmp"]);
    let backend = super::parse_dkg_key_backend(&cli).unwrap();
    assert!(matches!(
        backend,
        outbe_consensus::bls::KeyBackend::Plaintext
    ));
}

#[test]
fn test_parse_dkg_key_backend_encrypted_with_passphrase() {
    let cli = make_dkg_cli(&[
        "--bls-key-backend",
        "encrypted",
        "--bls-passphrase",
        "hunter2",
        "status",
        "--storage-dir",
        "/tmp",
    ]);
    let backend = super::parse_dkg_key_backend(&cli).unwrap();
    assert!(matches!(
        backend,
        outbe_consensus::bls::KeyBackend::Encrypted(ref p) if p == "hunter2"
    ));
}

#[test]
fn test_parse_dkg_key_backend_encrypted_missing_passphrase() {
    let cli = make_dkg_cli(&[
        "--bls-key-backend",
        "encrypted",
        "status",
        "--storage-dir",
        "/tmp",
    ]);
    assert!(super::parse_dkg_key_backend(&cli).is_err());
}

#[test]
fn test_parse_dkg_key_backend_os_level() {
    let cli = make_dkg_cli(&[
        "--bls-key-backend",
        "os-level",
        "status",
        "--storage-dir",
        "/tmp",
    ]);
    let backend = super::parse_dkg_key_backend(&cli).unwrap();
    assert!(matches!(backend, outbe_consensus::bls::KeyBackend::OsLevel));
}

#[test]
fn test_parse_dkg_key_backend_unknown() {
    let cli = make_dkg_cli(&[
        "--bls-key-backend",
        "foo",
        "status",
        "--storage-dir",
        "/tmp",
    ]);
    assert!(super::parse_dkg_key_backend(&cli).is_err());
}

// --- TC-002: DKG command routing via run_dkg_command ---

fn dkg_args(args: &[&str]) -> Vec<String> {
    let mut v = vec!["outbe-chain".to_string(), "dkg".to_string()];
    v.extend(args.iter().map(|s| s.to_string()));
    v
}

#[test]
fn test_dkg_bootstrap_3_validators() {
    let dir = tempfile::tempdir().unwrap();
    let dir_str = dir.path().to_str().unwrap();
    let args = dkg_args(&["bootstrap", "--output-dir", dir_str, "--validators", "3"]);
    super::run_dkg_command(&args).unwrap();

    // Verify output structure: every validator gets a ready keys directory.
    assert!(dir.path().join("polynomial.hex").exists());
    assert!(dir.path().join("dkg_output.hex").exists());
    assert!(dir.path().join("validators.json").exists());
    for i in 0..3 {
        let vdir = dir.path().join(format!("validator-{i}"));
        assert!(vdir.join("signing-key.hex").exists());
        assert!(vdir.join("evm-key.hex").exists());
        for name in ["dkg_share.hex", "dkg_polynomial.hex", "dkg_output.hex"] {
            assert!(vdir.join("dkg-keys").join(name).exists(), "{name}");
        }
    }
}

#[test]
fn test_dkg_identities_do_not_precompute_genesis_threshold_material() {
    let dir = tempfile::tempdir().unwrap();
    let args = dkg_args(&[
        "identities",
        "--output-dir",
        dir.path().to_str().unwrap(),
        "--validators",
        "4",
    ]);
    super::run_dkg_command(&args).unwrap();

    assert!(dir.path().join("validators.json").exists());
    assert!(dir.path().join("reth-bootnodes.txt").exists());
    assert!(!dir.path().join("polynomial.hex").exists());
    assert!(!dir.path().join("dkg_output.hex").exists());
    for index in 0..4 {
        let validator = dir.path().join(format!("validator-{index}"));
        assert!(validator.join("signing-key.hex").exists());
        assert!(validator.join("evm-key.hex").exists());
        assert!(validator.join("reth-p2p-secret.hex").exists());
        assert!(!validator.join("dkg-keys").join("dkg_share.hex").exists());
    }
}

#[test]
fn test_dkg_status_after_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    let dir_str = dir.path().to_str().unwrap();
    let args = dkg_args(&["bootstrap", "--output-dir", dir_str, "--validators", "3"]);
    super::run_dkg_command(&args).unwrap();

    // Status on a validator keys directory (has share + poly from bootstrap)
    let v0 = dir.path().join("validator-0").join("dkg-keys");
    let v0_str = v0.to_str().unwrap();
    let status_args = dkg_args(&["status", "--storage-dir", v0_str]);
    super::run_dkg_command(&status_args).unwrap();
}

#[test]
fn test_dkg_status_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let dir_str = dir.path().to_str().unwrap();
    let args = dkg_args(&["status", "--storage-dir", dir_str]);
    // Should succeed but print "NOT READY"
    super::run_dkg_command(&args).unwrap();
}

#[test]
fn test_dkg_export_requires_complete_runtime_state() {
    let dir = tempfile::tempdir().unwrap();
    let dir_str = dir.path().to_str().unwrap();
    let args = dkg_args(&["bootstrap", "--output-dir", dir_str, "--validators", "3"]);
    super::run_dkg_command(&args).unwrap();

    // Runtime export must reject validator storage that lacks its local
    // complete triplet instead of producing an import bundle startup cannot load.
    let v0 = dir.path().join("validator-0");
    let keys = v0.join("dkg-keys");
    std::fs::copy(keys.join("dkg_share.hex"), v0.join("dkg_share.hex")).unwrap();
    std::fs::copy(
        keys.join("dkg_polynomial.hex"),
        v0.join("dkg_polynomial.hex"),
    )
    .unwrap();

    let export_dir = tempfile::tempdir().unwrap();
    let export_args = dkg_args(&[
        "export-share",
        "--storage-dir",
        v0.to_str().unwrap(),
        "--output",
        export_dir.path().to_str().unwrap(),
    ]);
    assert!(super::run_dkg_command(&export_args).is_err());
}

#[test]
fn test_dkg_force_restart_only_removes_consensus_material() {
    let dir = tempfile::tempdir().unwrap();
    let dir_str = dir.path().to_str().unwrap();
    let args = dkg_args(&["bootstrap", "--output-dir", dir_str, "--validators", "3"]);
    super::run_dkg_command(&args).unwrap();

    let v0 = dir.path().join("validator-0");
    let keys = v0.join("dkg-keys");
    // Copy into runtime filenames
    std::fs::copy(keys.join("dkg_share.hex"), v0.join("dkg_share.hex")).unwrap();
    std::fs::copy(
        keys.join("dkg_polynomial.hex"),
        v0.join("dkg_polynomial.hex"),
    )
    .unwrap();
    std::fs::write(v0.join("dkg_output.hex"), "placeholder").unwrap();
    let tee_sentinels = [
        ("sealed_root.bin", b"permanent-offer-key".as_slice()),
        ("sealed_identity.bin", b"enclave-identity".as_slice()),
        (
            "sealed_node_authorization_v1.bin",
            b"node-host-authorization".as_slice(),
        ),
    ];
    for (name, bytes) in tee_sentinels {
        std::fs::write(v0.join(name), bytes).unwrap();
    }
    assert!(v0.join("dkg_share.hex").exists());
    assert!(v0.join("dkg_output.hex").exists());

    let restart_args = dkg_args(&["force-restart", "--storage-dir", v0.to_str().unwrap()]);
    super::run_dkg_command(&restart_args).unwrap();

    assert!(!v0.join("dkg_share.hex").exists());
    assert!(!v0.join("dkg_polynomial.hex").exists());
    assert!(!v0.join("dkg_output.hex").exists());
    for (name, bytes) in tee_sentinels {
        assert_eq!(std::fs::read(v0.join(name)).unwrap(), bytes);
    }
}

#[test]
fn test_dkg_force_restart_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let dir_str = dir.path().to_str().unwrap();
    let args = dkg_args(&["force-restart", "--storage-dir", dir_str]);
    super::run_dkg_command(&args).unwrap(); // no-op, succeeds
}

#[test]
fn test_role_backends_default_and_reject_incomplete_selection() {
    use crate::errors::CliError;

    let cli = make_dkg_cli(&["status", "--storage-dir", "/tmp"]);
    let roles = crate::cli::dkg::parse_role_key_backends(&cli).unwrap();
    assert!(matches!(
        roles.bls,
        outbe_consensus::bls::KeyBackend::Plaintext
    ));
    assert!(matches!(
        roles.evm,
        outbe_consensus::bls::KeyBackend::Plaintext
    ));
    assert!(matches!(
        roles.reth,
        outbe_consensus::bls::KeyBackend::Plaintext
    ));

    let cli = make_dkg_cli(&[
        "--bls-key-backend",
        "plaintext",
        "--evm-key-backend",
        "encrypted",
        "--evm-passphrase",
        "hunter2",
        "--reth-key-backend",
        "os-level",
        "status",
        "--storage-dir",
        "/tmp",
    ]);
    let roles = crate::cli::dkg::parse_role_key_backends(&cli).unwrap();
    assert!(matches!(
        roles.bls,
        outbe_consensus::bls::KeyBackend::Plaintext
    ));
    assert!(matches!(
        roles.evm,
        outbe_consensus::bls::KeyBackend::Encrypted(ref passphrase) if passphrase == "hunter2"
    ));
    assert!(matches!(
        roles.reth,
        outbe_consensus::bls::KeyBackend::OsLevel
    ));

    let missing_evm = make_dkg_cli(&[
        "--evm-key-backend",
        "encrypted",
        "status",
        "--storage-dir",
        "/tmp",
    ]);
    let error = crate::cli::dkg::parse_role_key_backends(&missing_evm).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<CliError>(),
        Some(CliError::MissingEvmPassphrase)
    ));

    let missing_reth = make_dkg_cli(&[
        "--reth-key-backend",
        "encrypted",
        "status",
        "--storage-dir",
        "/tmp",
    ]);
    let error = crate::cli::dkg::parse_role_key_backends(&missing_reth).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<CliError>(),
        Some(CliError::MissingRethPassphrase)
    ));

    let unknown_evm = make_dkg_cli(&[
        "--evm-key-backend",
        "foo",
        "status",
        "--storage-dir",
        "/tmp",
    ]);
    let error = crate::cli::dkg::parse_role_key_backends(&unknown_evm).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<CliError>(),
        Some(CliError::UnknownEvmKeyBackend { .. })
    ));

    let unknown_reth = make_dkg_cli(&[
        "--reth-key-backend",
        "foo",
        "status",
        "--storage-dir",
        "/tmp",
    ]);
    let error = crate::cli::dkg::parse_role_key_backends(&unknown_reth).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<CliError>(),
        Some(CliError::UnknownRethKeyBackend { .. })
    ));

    let args = dkg_args(&[
        "--evm-key-backend",
        "encrypted",
        "status",
        "--storage-dir",
        "/tmp",
    ]);
    assert!(super::run_dkg_command(&args).is_err());
}

#[test]
fn test_dkg_export_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let export_dir = tempfile::tempdir().unwrap();
    let args = dkg_args(&[
        "export-share",
        "--storage-dir",
        dir.path().to_str().unwrap(),
        "--output",
        export_dir.path().to_str().unwrap(),
    ]);
    assert!(super::run_dkg_command(&args).is_err());
}
