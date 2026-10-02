use super::*;

#[test]
fn public_operator_commands_parse_for_the_dev_lane() {
    for command in ["bootstrap", "start", "status", "stop"] {
        let cli = LocalnetCli::try_parse_from([
            "outbe-e2e localnet",
            command,
            "--data-dir",
            "/tmp/localnet-a",
            "--validators",
            "5",
        ])
        .unwrap();
        assert_eq!(cli.data_dir, PathBuf::from("/tmp/localnet-a"));
        assert_eq!(cli.validators, 5);
    }
}

#[test]
fn persistent_inventory_rejects_an_external_supervisor_process() {
    let error = ensure_ocomp_process_inventory(
        &[OwnedOcompProcessV1 {
            validator_index: Some(0),
            role: "supervisor".to_owned(),
            worker_ordinal: None,
            pid: 1,
            process_identity: "test".to_owned(),
        }],
        OcompRuntimeCountsV1 {
            supervisors: 1,
            snapshot_exporters: 0,
            workers: 0,
            registered_workers: 0,
            connected_workers: 0,
        },
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("unexpected persistent LocalNet OCOMP role supervisor"));
}

#[tokio::test]
async fn real_sgx_command_is_explicit_and_fail_closed() {
    let error = run_from(
        LocalnetLane::RealSgx,
        ["outbe-e2e localnet-sgx", "bootstrap"],
    )
    .await
    .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("outbe-chain-8lp"));
    assert!(message.contains("no mock or GramineDirectDev fallback"));
}

#[test]
fn state_is_atomic_and_bound_to_the_exact_owner_process() {
    let dir = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let identity = process_identity(pid).expect("current process identity");
    let state = LocalnetStateV1 {
        version: STATE_VERSION,
        lane: "dev_mock".to_owned(),
        phase: DriverPhase::Running,
        owner_pid: pid,
        owner_process_identity: identity,
        repo: PathBuf::from("/repo"),
        data_dir: dir.path().to_path_buf(),
        validators: 4,
        port_blocks: vec![18545, 18558, 18571, 18584],
        rpc_ports: vec![18545, 18558, 18571, 18584],
        heights: vec![7, 7, 7, 7],
        ocomp_supervisors: 4,
        ocomp_snapshot_exporters: 4,
        ocomp_workers: 4,
        ocomp_registered_workers: 4,
        ocomp_connected_workers: 4,
        ocomp_processes: Vec::new(),
    };
    write_json_atomic(&state_path(dir.path()), &state).unwrap();
    let decoded = read_state(dir.path()).unwrap().unwrap();
    assert_eq!(decoded, state);
    assert!(state_is_live(&decoded));

    let mut stale = decoded;
    stale.owner_process_identity.push_str("-stale");
    assert!(!state_is_live(&stale));
}

#[test]
fn destructive_bootstrap_targets_reject_broad_paths() {
    assert!(validate_data_dir(Path::new("/repo"), Path::new("/")).is_err());
    assert!(validate_data_dir(Path::new("/repo"), Path::new("/repo")).is_err());
    assert!(validate_data_dir(Path::new("/repo/project"), Path::new("/repo")).is_err());
    validate_data_dir(Path::new("/repo"), Path::new("/tmp/outbe-testnet")).unwrap();
}

/// Linux keeps the Gramine container; every other host runs the enclave
/// natively, because the test image is amd64-only and does not survive
/// emulation. Asserted on both sides so neither can drift silently.
#[test]
fn the_driver_resolves_the_enclave_profile_this_host_can_run() {
    let mode = localnet_tee_mode();
    if cfg!(target_os = "linux") {
        assert_eq!(mode, TeeMode::Mock);
        assert!(!mode.runs_native_host_enclave());
    } else {
        assert_eq!(mode, TeeMode::MockNative);
        assert!(mode.runs_native_host_enclave());
    }
    assert!(mode.uses_mock_binary());
    assert!(enclave_profile_banner().contains(mode.evidence_name()));
}

#[test]
fn destructive_targets_are_resolved_before_validation() {
    let repo = tempfile::tempdir().unwrap();
    let resolved_repo = fs::canonicalize(repo.path()).unwrap();

    let root_alias = resolve_maybe_missing_path(Path::new("/tmp/..")).unwrap();
    assert_eq!(root_alias, Path::new("/"));
    assert!(validate_data_dir(&resolved_repo, &root_alias).is_err());

    let repo_alias = repo.path().join("target/..");
    let resolved_alias = resolve_maybe_missing_path(&repo_alias).unwrap();
    assert_eq!(resolved_alias, resolved_repo);
    assert!(validate_data_dir(&resolved_repo, &resolved_alias).is_err());
}

#[cfg(unix)]
#[test]
fn symlinked_destructive_target_is_resolved_before_validation() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().unwrap();
    let repo = parent.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let alias = parent.path().join("alias");
    symlink(&repo, &alias).unwrap();

    let resolved_repo = resolve_existing_path(&repo).unwrap();
    let resolved_alias = resolve_maybe_missing_path(&alias).unwrap();
    assert_eq!(resolved_alias, resolved_repo);
    assert!(validate_data_dir(&resolved_repo, &resolved_alias).is_err());
}
