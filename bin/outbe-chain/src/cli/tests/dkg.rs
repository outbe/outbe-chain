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

    // Verify output structure
    assert!(dir.path().join("polynomial.hex").exists());
    assert!(dir.path().join("dkg-output.hex").exists());
    assert!(dir.path().join("validators.json").exists());
    for i in 0..3 {
        let vdir = dir.path().join(format!("validator-{i}"));
        assert!(vdir.join("signing-key.hex").exists());
        assert!(vdir.join("evm-key.hex").exists());
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
    assert!(!dir.path().join("dkg-output.hex").exists());
    for index in 0..4 {
        let validator = dir.path().join(format!("validator-{index}"));
        assert!(validator.join("signing-key.hex").exists());
        assert!(validator.join("evm-key.hex").exists());
        assert!(validator.join("reth-p2p-secret.hex").exists());
        assert!(!validator.join("signing-share.hex").exists());
    }
}

#[test]
fn test_dkg_status_after_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    let dir_str = dir.path().to_str().unwrap();
    let args = dkg_args(&["bootstrap", "--output-dir", dir_str, "--validators", "3"]);
    super::run_dkg_command(&args).unwrap();

    // Status on a validator directory (has share + poly from bootstrap)
    let v0 = dir.path().join("validator-0");
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

    // Bootstrap output keeps the shared dkg-output.hex at the output root.
    // Runtime export must still reject validator storage that lacks its local
    // complete triplet instead of producing an import bundle startup cannot load.
    let v0 = dir.path().join("validator-0");
    std::fs::copy(v0.join("signing-share.hex"), v0.join("dkg_share.hex")).unwrap();
    std::fs::copy(
        dir.path().join("polynomial.hex"),
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
    // Copy into runtime filenames
    std::fs::copy(v0.join("signing-share.hex"), v0.join("dkg_share.hex")).unwrap();
    std::fs::copy(
        dir.path().join("polynomial.hex"),
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
