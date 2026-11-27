use std::os::unix::{
    fs::{symlink, PermissionsExt},
    net::UnixListener,
};

use super::*;

fn mountinfo_for(root: &Path, options: &str) -> String {
    format!(
        "100 1 8:1 / {} {} - ext4 /dev/vda1 rw\n",
        root.display(),
        options
    )
}

fn write_file_mode(path: &Path, content: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn build_fake_bin_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let keygen = dir.path().join("outbe-keygen");
    write_file_mode(&keygen, b"#!/bin/sh\nexit 0\n", 0o755);
    dir
}

fn valid_env() -> String {
    valid_env_for_role("founder")
}

fn valid_env_for_role(role: &str) -> String {
    let peer_key = "a".repeat(96);
    format!(
        concat!(
            "OUTBE_NODE_ROLE=\"{role}\"\n",
            "OUTBE_EXTERNAL_IP=\"10.0.0.1\"\n",
            "OUTBE_RPC_BIND_IP=\"10.77.0.10\"\n",
            "OUTBE_VM_IP=\"10.77.0.10\"\n",
            "OUTBE_VM_PREFIX=\"24\"\n",
            "OUTBE_VM_GATEWAY=\"10.77.0.1\"\n",
            "OUTBE_VM_DNS=\"1.1.1.1,8.8.8.8\"\n",
            "OUTBE_RPC_PORT=\"8545\"\n",
            "OUTBE_CONSENSUS_PORT=\"30400\"\n",
            "OUTBE_RETH_P2P_PORT=\"30303\"\n",
            "OUTBE_RETH_DISCOVERY_PORT=\"31303\"\n",
            "OUTBE_RADICLE_PORT=\"8776\"\n",
            "OUTBE_CERTIFIED_RPC=\"http://127.0.0.1:8545\"\n",
            "OUTBE_VALIDATOR_ADDRESS=\"0x1111111111111111111111111111111111111111\"\n",
            "OUTBE_BOOTNODES=\"enode://abcd@10.0.0.1:30303\"\n",
            "OUTBE_CONSENSUS_PEERS=\"{peer_key}@10.0.0.1:30400\"\n",
            "OUTBE_CONSENSUS_EXTRA_ARGS=\"--consensus.use-local-defaults\"\n",
            "OUTBE_HYPERLANE_FUNDING_WEI=\"1000000000000000000\"\n",
            "OUTBE_CHAIN_ID=\"707\"\n",
        ),
        role = role,
        peer_key = peer_key
    )
}

fn build_valid_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_file_mode(&root.join("validator.env"), valid_env().as_bytes(), 0o640);
    write_file_mode(
        &root.join("feeder-public.toml"),
        b"pairs = \"pairs.toml\"\n",
        0o640,
    );
    write_file_mode(
        &root.join("hyperlane-public.toml"),
        concat!(
            "[hyperlane]\n",
            "poll_interval_secs = 30\n",
            "gasless = true\n\n",
            "[health]\n",
            "enabled = true\n",
            "bind_address = \"127.0.0.1:9003\"\n\n",
            "[storage]\n",
            "db_path = \"/opt/outbe-chain/state/hyperlane/db\"\n\n",
            "[server]\n",
            "bind_address = \"0.0.0.0:9000\"\n",
            "public_url = \"http://10.0.0.1:9000\"\n\n",
            "[[origin]]\n",
            "domain = 707\n",
            "rpc_urls = [\"http://127.0.0.1:8545\"]\n",
            "rpc_consensus = \"single\"\n",
            "reorg_period = 0\n",
        )
        .as_bytes(),
        0o640,
    );
    write_file_mode(
        &root.join("reth.toml"),
        b"[peers]\nincoming_ip_throttle_duration = \"30s\"\n",
        0o640,
    );
    write_file_mode(
        &root.join("genesis.json"),
        br#"{"config":{"chainId":707}}"#,
        0o644,
    );
    write_file_mode(&root.join(".bb-crs/bn254_g1.dat"), b"crs", 0o644);

    let key_hex = "a".repeat(64);
    for relative in install::REQUIRED_SECRETS {
        write_file_mode(&root.join("keys").join(relative), key_hex.as_bytes(), 0o600);
    }
    dir
}

fn test_options(bin_dir: &Path) -> BootOptions<'_> {
    BootOptions {
        bin_dir: Some(bin_dir),
        require_root_owner: false,
        apply_ownership: false,
        prepare_runtime_dirs: true,
        configure_network: false,
        start_role_services: false,
        expected_role: None,
        cancel: None,
    }
}

#[test]
fn valid_layout_passes_and_creates_state_dirs() {
    let dir = build_valid_root();
    let bins = build_fake_bin_dir();
    let root = dir.path();
    let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec,relatime");

    boot_root(root, &mountinfo, &test_options(bins.path())).unwrap();

    for relative in [
        "validator-0/data",
        "validator-0/consensus",
        "validator-0/logs",
        "keys/radicle/storage",
        "keys/radicle/node",
        "keys/radicle/cobs",
        "state/operator",
        "state/feeder",
        "state/hyperlane",
    ] {
        assert!(root.join(relative).is_dir(), "missing dir: {relative}");
    }
}

#[test]
fn second_boot_with_service_state_and_stale_sockets_passes() {
    let dir = build_valid_root();
    let bins = build_fake_bin_dir();
    let root = dir.path();

    // Populate stale sockets and runtime files left by a hard power-off.
    let radicle_sock = root.join("keys/radicle/node/outbe-control.sock");
    fs::create_dir_all(radicle_sock.parent().unwrap()).unwrap();
    let _radicle_listener = UnixListener::bind(&radicle_sock).unwrap();

    let reth_ipc = root.join("validator-0/data/reth.ipc");
    fs::create_dir_all(reth_ipc.parent().unwrap()).unwrap();
    let _reth_listener = UnixListener::bind(&reth_ipc).unwrap();

    let git_hook = root.join("keys/radicle/storage/repo/hooks/post-receive");
    write_file_mode(&git_hook, b"#!/bin/sh\n", 0o755);

    let reth_file = root.join("validator-0/data/db/mdbx.dat");
    write_file_mode(&reth_file, b"db", 0o755);

    let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");
    boot_root(root, &mountinfo, &test_options(bins.path())).unwrap();

    assert!(
        !radicle_sock.exists(),
        "stale radicle socket must be removed"
    );
    assert!(!reth_ipc.exists(), "stale reth.ipc socket must be removed");
    assert!(
        git_hook.is_file(),
        "radicle storage files must be preserved"
    );
    assert!(reth_file.is_file(), "reth datadir files must be preserved");
}

#[test]
fn rejects_missing_mount_or_wrong_mount_flags() {
    let dir = build_valid_root();
    let bins = build_fake_bin_dir();
    let root = dir.path();

    let err = boot_root(root, "", &test_options(bins.path())).unwrap_err();
    assert!(err.to_string().contains("is not a mount point"), "{err}");

    let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,relatime");
    let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
    assert!(err.to_string().contains("noexec"), "{err}");

    let mountinfo = mountinfo_for(root, "rw,noexec");
    let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
    assert!(err.to_string().contains("nosuid"), "{err}");
    assert!(err.to_string().contains("nodev"), "{err}");
}

#[test]
fn rejects_symlink_in_data_tree() {
    let dir = build_valid_root();
    let bins = build_fake_bin_dir();
    let root = dir.path();
    let link = root.join("evil-link");
    symlink("/etc/passwd", &link).unwrap();

    let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");
    let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
    assert!(err.to_string().contains("refusing symlink"), "{err}");
    assert!(
        err.to_string().contains(&link.display().to_string()),
        "{err}"
    );
}

#[test]
fn normalization_refuses_planted_symlinks_without_touching_outside_targets() {
    let dir = build_valid_root();
    let outside = tempfile::tempdir().unwrap();
    let root = dir.path();

    let outside_dir = outside.path().join("target-dir");
    fs::create_dir(&outside_dir).unwrap();
    fs::set_permissions(&outside_dir, fs::Permissions::from_mode(0o755)).unwrap();

    let outside_file = outside.path().join("target-file");
    write_file_mode(&outside_file, b"untouched", 0o644);

    // Plant a symlink where a state directory is expected to be normalized.
    fs::create_dir_all(root.join("state")).unwrap();
    symlink(&outside_dir, root.join("state/feeder")).unwrap();
    let err = prepare_directories(root, NodeRole::Founder, false).unwrap_err();
    assert!(
        err.to_string().contains("refusing symlink"),
        "expected symlink refusal, got: {err}"
    );
    assert_eq!(
        fs::metadata(&outside_dir).unwrap().mode() & 0o777,
        0o755,
        "outside directory mode must not be modified"
    );

    // Replace directory symlink with a real dir and plant a file symlink on a config file.
    fs::remove_file(root.join("state/feeder")).unwrap();
    fs::remove_file(root.join("feeder-public.toml")).unwrap();
    symlink(&outside_file, root.join("feeder-public.toml")).unwrap();
    let err = prepare_directories(root, NodeRole::Founder, false).unwrap_err();
    assert!(
        err.to_string().contains("refusing symlink"),
        "expected symlink refusal, got: {err}"
    );
    assert_eq!(
        fs::metadata(&outside_file).unwrap().mode() & 0o777,
        0o644,
        "outside file mode must not be modified"
    );
}

#[test]
fn rejects_executable_file_in_static_data_tree() {
    let dir = build_valid_root();
    let bins = build_fake_bin_dir();
    let root = dir.path();
    let exec_path = root.join("rogue-script");
    write_file_mode(&exec_path, b"#!/bin/sh\n", 0o755);

    let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");
    let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
    assert!(
        err.to_string().contains("refusing executable file"),
        "{err}"
    );
    assert!(
        err.to_string().contains(&exec_path.display().to_string()),
        "{err}"
    );
}

#[test]
fn rejects_unexpected_socket_in_static_data_tree() {
    let dir = build_valid_root();
    let bins = build_fake_bin_dir();
    let root = dir.path();
    let sock = root.join("stray.sock");
    let _listener = UnixListener::bind(&sock).unwrap();

    let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");
    let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
    assert!(err.to_string().contains("refusing socket"), "{err}");
    assert!(
        err.to_string().contains(&sock.display().to_string()),
        "{err}"
    );
}

#[test]
fn rejects_missing_key_and_loose_key_permissions() {
    let dir = build_valid_root();
    let bins = build_fake_bin_dir();
    let root = dir.path();
    let key_path = root.join("keys/evm-key.hex");
    fs::remove_file(&key_path).unwrap();

    let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");
    let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
    assert!(
        err.to_string().contains("required secret is missing"),
        "{err}"
    );
    assert!(
        err.to_string().contains(&key_path.display().to_string()),
        "{err}"
    );

    let key_hex = "a".repeat(64);
    write_file_mode(&key_path, key_hex.as_bytes(), 0o644);
    let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
    assert!(err.to_string().contains("owner-only permissions"), "{err}");
    assert!(
        err.to_string().contains(&key_path.display().to_string()),
        "{err}"
    );
}

#[test]
fn rejects_unknown_validator_env_keys() {
    let dir = build_valid_root();
    let bins = build_fake_bin_dir();
    let root = dir.path();
    let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");

    for bad_line in [
        "BB_CRS_PATH=\"/tmp/evil.dat\"\n",
        "LD_PRELOAD=\"/tmp/evil.so\"\n",
    ] {
        let mut env = valid_env();
        env.push_str(bad_line);
        write_file_mode(&root.join("validator.env"), env.as_bytes(), 0o640);
        let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
        assert!(
            err.to_string().contains("unknown validator.env key"),
            "{err}"
        );
    }
}

#[test]
fn rejects_invalid_signing_key_via_outbe_keygen() {
    let dir = build_valid_root();
    let bins = tempfile::tempdir().unwrap();
    write_file_mode(
        &bins.path().join("outbe-keygen"),
        concat!(
            "#!/bin/sh\n",
            "echo 'STDOUT_SECRET_MARKER_deadbeef'\n",
            "echo 'STDERR_SECRET_MARKER_cafebabe' >&2\n",
            "exit 1\n",
        )
        .as_bytes(),
        0o755,
    );
    let root = dir.path();
    let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");

    let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("signing-key.hex failed outbe-keygen verify"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("STDOUT_SECRET_MARKER_deadbeef"),
        "verify_signing_key must never leak stdout: {rendered}"
    );
    assert!(
        !rendered.contains("STDERR_SECRET_MARKER_cafebabe"),
        "verify_signing_key must never leak stderr: {rendered}"
    );
}

fn build_valid_full_node_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_file_mode(
        &root.join("validator.env"),
        concat!(
            "OUTBE_NODE_ROLE=\"full-node\"\n",
            "OUTBE_EXTERNAL_IP=\"10.0.0.2\"\n",
            "OUTBE_RPC_BIND_IP=\"10.77.0.20\"\n",
            "OUTBE_VM_IP=\"10.77.0.20\"\n",
            "OUTBE_VM_PREFIX=\"24\"\n",
            "OUTBE_VM_GATEWAY=\"10.77.0.1\"\n",
            "OUTBE_VM_DNS=\"1.1.1.1,8.8.8.8\"\n",
            "OUTBE_RPC_PORT=\"8545\"\n",
            "OUTBE_CONSENSUS_PORT=\"30400\"\n",
            "OUTBE_RETH_P2P_PORT=\"30303\"\n",
            "OUTBE_RETH_DISCOVERY_PORT=\"31303\"\n",
            "OUTBE_RADICLE_PORT=\"8776\"\n",
            "OUTBE_CERTIFIED_RPC=\"http://10.0.0.1:8545\"\n",
            "OUTBE_BOOTNODES=\"enode://abcd@10.0.0.1:30303\"\n",
            "OUTBE_CONSENSUS_EXTRA_ARGS=\"--consensus.use-local-defaults\"\n",
            "OUTBE_CHAIN_ID=\"707\"\n",
        )
        .as_bytes(),
        0o640,
    );
    write_file_mode(
        &root.join("genesis.json"),
        br#"{"config":{"chainId":707}}"#,
        0o644,
    );
    write_file_mode(&root.join(".bb-crs/bn254_g1.dat"), b"crs", 0o644);
    write_file_mode(
        &root.join("reth.toml"),
        b"[peers]\nincoming_ip_throttle_duration = \"30s\"\n",
        0o640,
    );
    dir
}

#[test]
fn role_marker_validates_joining_and_full_node_and_forbids_full_node_secrets() {
    let bins = build_fake_bin_dir();

    // 1. Joining validator via OUTBE_NODE_ROLE in validator.env passes full validator checks.
    let joining = build_valid_root();
    write_file_mode(
        &joining.path().join("validator.env"),
        valid_env_for_role("joining-validator").as_bytes(),
        0o640,
    );
    let mountinfo = mountinfo_for(joining.path(), "rw,nosuid,nodev,noexec");
    assert_eq!(
        read_role_from_tree(joining.path(), false).unwrap(),
        NodeRole::JoiningValidator
    );
    boot_root(joining.path(), &mountinfo, &test_options(bins.path())).unwrap();
    assert!(joining.path().join("state/hyperlane").is_dir());

    // 2. Full node passes with zero keys and creates only validator-0 + state/operator.
    let full = build_valid_full_node_root();
    let full_mount = mountinfo_for(full.path(), "rw,nosuid,nodev,noexec");
    verify_static_tree_for_role(full.path(), NodeRole::FullNode, None).unwrap();
    assert!(
        !full.path().join("validator-0").exists(),
        "static verification must not create runtime directories"
    );
    boot_root(full.path(), &full_mount, &test_options(bins.path())).unwrap();
    assert!(full.path().join("validator-0/data").is_dir());
    assert!(full.path().join("state/operator").is_dir());
    assert!(!full.path().join("state/feeder").exists());
    assert!(!full.path().join("state/hyperlane").exists());
    assert!(!full.path().join("keys").exists());

    // 3. Full node rejects validator keys, feeder/hyperlane configs, and validator env keys.
    let bad_keys = build_valid_full_node_root();
    write_file_mode(
        &bad_keys.path().join("keys/evm-key.hex"),
        "a".repeat(64).as_bytes(),
        0o600,
    );
    let err = verify_static_tree_for_role(bad_keys.path(), NodeRole::FullNode, None).unwrap_err();
    assert!(
        err.to_string()
            .contains("must not contain validator artifact"),
        "{err}"
    );

    let bad_feeder = build_valid_full_node_root();
    write_file_mode(
        &bad_feeder.path().join("feeder-public.toml"),
        b"[oracle]\n",
        0o640,
    );
    let err = verify_static_tree_for_role(bad_feeder.path(), NodeRole::FullNode, None).unwrap_err();
    assert!(
        err.to_string()
            .contains("must not contain validator artifact"),
        "{err}"
    );

    let bad_env = build_valid_full_node_root();
    let mut env = fs::read_to_string(bad_env.path().join("validator.env")).unwrap();
    env.push_str("OUTBE_VALIDATOR_ADDRESS=\"0x1111111111111111111111111111111111111111\"\n");
    write_file_mode(&bad_env.path().join("validator.env"), env.as_bytes(), 0o640);
    let err = verify_static_tree_for_role(bad_env.path(), NodeRole::FullNode, None).unwrap_err();
    assert!(
        err.to_string().contains("unknown validator.env key"),
        "{err}"
    );

    // 4. Role mismatch, missing/invalid OUTBE_NODE_ROLE, and disagreeing node-role file fail closed.
    let err = verify_static_tree_for_role(full.path(), NodeRole::Founder, None).unwrap_err();
    assert!(err.to_string().contains("role mismatch"), "{err}");

    let missing_role = build_valid_full_node_root();
    let stripped = fs::read_to_string(missing_role.path().join("validator.env"))
        .unwrap()
        .replace("OUTBE_NODE_ROLE=\"full-node\"\n", "");
    write_file_mode(
        &missing_role.path().join("validator.env"),
        stripped.as_bytes(),
        0o640,
    );
    let err =
        verify_static_tree_for_role(missing_role.path(), NodeRole::FullNode, None).unwrap_err();
    assert!(err.to_string().contains("missing OUTBE_NODE_ROLE"), "{err}");

    let bogus_role = build_valid_full_node_root();
    let bogus_env = fs::read_to_string(bogus_role.path().join("validator.env"))
        .unwrap()
        .replace("full-node", "bogus");
    write_file_mode(
        &bogus_role.path().join("validator.env"),
        bogus_env.as_bytes(),
        0o640,
    );
    assert!(verify_static_tree_for_role(bogus_role.path(), NodeRole::FullNode, None).is_err());

    // Separate `node-role` file present and agreeing passes; disagreeing or loose fails closed.
    let with_legacy_file = build_valid_full_node_root();
    write_file_mode(
        &with_legacy_file.path().join(NODE_ROLE_FILE),
        b"full-node\n",
        0o640,
    );
    verify_static_tree_for_role(with_legacy_file.path(), NodeRole::FullNode, None).unwrap();

    write_file_mode(
        &with_legacy_file.path().join(NODE_ROLE_FILE),
        b"founder\n",
        0o640,
    );
    let err =
        verify_static_tree_for_role(with_legacy_file.path(), NodeRole::FullNode, None).unwrap_err();
    assert!(
        err.to_string()
            .contains("role mismatch between validator.env OUTBE_NODE_ROLE"),
        "{err}"
    );

    write_file_mode(
        &with_legacy_file.path().join(NODE_ROLE_FILE),
        b"full-node\n",
        0o666,
    );
    assert!(
        verify_static_tree_for_role(with_legacy_file.path(), NodeRole::FullNode, None).is_err()
    );

    // 5. Loopback OUTBE_VM_IP is refused for all VM roles, while OUTBE_RPC_BIND_IP=127.0.0.1
    //    is accepted (distinct from OUTBE_VM_IP) and unrelated private OUTBE_RPC_BIND_IP is refused.
    let loopback_founder = build_valid_root();
    let env_lb = valid_env().replace("OUTBE_VM_IP=\"10.77.0.10\"", "OUTBE_VM_IP=\"127.0.0.1\"");
    write_file_mode(
        &loopback_founder.path().join("validator.env"),
        env_lb.as_bytes(),
        0o640,
    );
    let err =
        verify_static_tree_for_role(loopback_founder.path(), NodeRole::Founder, None).unwrap_err();
    assert!(
        err.to_string().contains("non-loopback private IPv4"),
        "{err}"
    );

    let loopback_rpc_founder = build_valid_root();
    let env_lb_rpc = valid_env().replace(
        "OUTBE_RPC_BIND_IP=\"10.77.0.10\"",
        "OUTBE_RPC_BIND_IP=\"127.0.0.1\"",
    );
    write_file_mode(
        &loopback_rpc_founder.path().join("validator.env"),
        env_lb_rpc.as_bytes(),
        0o640,
    );
    verify_static_tree_for_role(loopback_rpc_founder.path(), NodeRole::Founder, None).unwrap();

    let mismatch_rpc_founder = build_valid_root();
    let env_mismatch_rpc = valid_env().replace(
        "OUTBE_RPC_BIND_IP=\"10.77.0.10\"",
        "OUTBE_RPC_BIND_IP=\"10.77.0.99\"",
    );
    write_file_mode(
        &mismatch_rpc_founder.path().join("validator.env"),
        env_mismatch_rpc.as_bytes(),
        0o640,
    );
    let err = verify_static_tree_for_role(mismatch_rpc_founder.path(), NodeRole::Founder, None)
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("must be 127.0.0.1 or equal to OUTBE_VM_IP"),
        "{err}"
    );

    let loopback_full = build_valid_full_node_root();
    let full_env_lb = fs::read_to_string(loopback_full.path().join("validator.env"))
        .unwrap()
        .replace("OUTBE_VM_IP=\"10.77.0.20\"", "OUTBE_VM_IP=\"127.0.0.1\"");
    write_file_mode(
        &loopback_full.path().join("validator.env"),
        full_env_lb.as_bytes(),
        0o640,
    );
    let err =
        verify_static_tree_for_role(loopback_full.path(), NodeRole::FullNode, None).unwrap_err();
    assert!(
        err.to_string().contains("non-loopback private IPv4"),
        "{err}"
    );
}

#[test]
fn vm_role_services_match_exact_per_role_units() {
    assert_eq!(
        vm_role_services(NodeRole::Founder),
        &[
            "outbe-radicle.service",
            "outbe-validator.service",
            "outbe-feeder.service",
            "outbe-hyperlane-agent.service",
        ]
    );
    assert_eq!(
        vm_role_services(NodeRole::JoiningValidator),
        &[
            "outbe-radicle.service",
            "outbe-joining-validator.service",
            "outbe-feeder.service",
            "outbe-hyperlane-agent.service",
        ]
    );
    assert_eq!(
        vm_role_services(NodeRole::FullNode),
        &["outbe-full-node.service"]
    );
}
