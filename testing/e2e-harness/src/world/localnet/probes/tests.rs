use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::env::Environment;
use crate::internal::{config::Config, proc::ChildGuard};

use super::{
    has_required_signing_share, parse_ce_startup_replay, read_required_node_log,
    validator_slot_node_log_path, CeStartupReplayObservationV1, RethLogTail,
};

#[test]
fn canonical_header_capture_preserves_both_streams_and_nonzero_exit() {
    for code in [0, 23] {
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "printf header; printf diagnostic >&2; exit \"$1\"",
                "reader",
            ])
            .arg(code.to_string());
        let output =
            super::capture_header_command_until(command, Instant::now() + Duration::from_secs(5))
                .unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert_eq!(output.stdout, b"header");
        assert_eq!(output.stderr, b"diagnostic");
    }
}

#[test]
fn canonical_header_capture_deadline_never_becomes_success() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("invoked");
    let mut command = Command::new("touch");
    command.arg(&marker);
    assert!(super::capture_header_command_until(command, Instant::now()).is_err());
    assert!(!marker.exists());

    let mut survivor = Command::new("sleep");
    survivor.arg("60");
    let mut survivor = ChildGuard::spawn("header-timeout-unrelated", survivor).unwrap();
    let mut reader = Command::new("sleep");
    reader.arg("60");
    let error =
        super::capture_header_command_until(reader, Instant::now() + Duration::from_millis(100))
            .unwrap_err();
    assert!(error.to_string().contains("exceeded its deadline"));
    let timeout = error.downcast_ref::<super::HeaderCaptureTimeout>().unwrap();
    assert!(!timeout.status.success());
    assert!(timeout.stdout.is_file() && timeout.stderr.is_file());
    std::fs::remove_file(&timeout.stdout).unwrap();
    std::fs::remove_file(&timeout.stderr).unwrap();
    assert!(survivor.exit_status().unwrap().is_none());
    survivor.fault_and_reap().unwrap();
}

#[test]
fn expired_capture_collection_preserves_complete_diagnostic_files() {
    let mut stdout = tempfile::NamedTempFile::new().unwrap();
    let stderr = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(stdout.path(), b"header diagnostic").unwrap();
    std::fs::write(stderr.path(), b"storage diagnostic").unwrap();
    assert!(
        super::header_stream_before(stdout.as_file_mut(), Instant::now())
            .unwrap()
            .is_none()
    );
    let status = Command::new("true").status().unwrap();
    let error = super::header_capture_timeout(status, stdout, stderr).unwrap_err();
    let timeout = error.downcast_ref::<super::HeaderCaptureTimeout>().unwrap();
    assert_eq!(timeout.status, status);
    assert_eq!(
        std::fs::read(&timeout.stdout).unwrap(),
        b"header diagnostic"
    );
    assert_eq!(
        std::fs::read(&timeout.stderr).unwrap(),
        b"storage diagnostic"
    );
    std::fs::remove_file(&timeout.stdout).unwrap();
    std::fs::remove_file(&timeout.stderr).unwrap();
}

#[test]
fn stopped_header_command_requires_existing_storage_and_exact_readonly_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("data");
    let genesis = directory.path().join("genesis.json");
    let binary = Path::new("/configured/release/outbe-chain");
    for relative in ["db", "static_files", "rocksdb"] {
        std::fs::create_dir_all(data.join(relative)).unwrap();
    }
    std::fs::write(&genesis, "{}").unwrap();
    assert!(super::stopped_header_command(binary, &data, &genesis, 42).is_err());
    assert!(!data.join("rocksdb/CURRENT").exists());
    std::fs::write(data.join("rocksdb/CURRENT"), "MANIFEST-000001\n").unwrap();
    let command = super::stopped_header_command(binary, &data, &genesis, 42).unwrap();
    assert_eq!(command.get_program(), "sh");
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        args,
        vec![
            "-c".to_owned(),
            "ulimit -Sn 131072 && exec \"$@\"".into(),
            "outbe-header-reader".into(),
            binary.display().to_string(),
            "db".to_owned(),
            "--datadir".into(),
            data.display().to_string(),
            "--chain".into(),
            genesis.display().to_string(),
            "--color".into(),
            "never".into(),
            "--log.file.max-files".into(),
            "0".into(),
            "get".into(),
            "static-file".into(),
            "headers".into(),
            "42".into()
        ]
    );
    std::fs::remove_file(data.join("rocksdb/CURRENT")).unwrap();
    std::os::unix::fs::symlink(&genesis, data.join("rocksdb/CURRENT")).unwrap();
    assert!(super::stopped_header_command(binary, &data, &genesis, 42).is_err());
}

#[test]
fn stopped_header_reader_sets_only_its_own_soft_descriptor_limit() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("data");
    let genesis = directory.path().join("genesis.json");
    let binary = directory.path().join("reader with spaces");
    for relative in ["db", "static_files", "rocksdb"] {
        std::fs::create_dir_all(data.join(relative)).unwrap();
    }
    std::fs::write(data.join("rocksdb/CURRENT"), "MANIFEST-000001\n").unwrap();
    std::fs::write(&genesis, "{}").unwrap();
    std::fs::write(&binary, "#!/bin/sh\nulimit -Sn\nulimit -Hn\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let parent_limits = || {
        Command::new("sh")
            .args(["-c", "ulimit -Sn; ulimit -Hn"])
            .output()
            .unwrap()
            .stdout
    };
    let before = parent_limits();
    let output = super::stopped_header_command(&binary, &data, &genesis, 42)
        .unwrap()
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        parent_limits(),
        before,
        "reader changed the parent's limits"
    );
    let before = String::from_utf8(before).unwrap();
    let after = String::from_utf8(output.stdout).unwrap();
    let before = before.lines().collect::<Vec<_>>();
    let after = after.lines().collect::<Vec<_>>();
    assert_eq!(after.len(), 2);
    assert_eq!(after[1], before[1], "reader changed the hard limit");
    assert_eq!(after[0].parse::<u64>().unwrap(), 131_072);
}

#[test]
fn offline_header_read_requires_exact_owned_exited_node_before_invocation() {
    let directory = tempfile::tempdir().unwrap();
    let env = Environment {
        validators: 1,
        data_dir: directory.path().to_path_buf(),
        ..Environment::default()
    };
    env.ports.start_scenario(1).unwrap();
    let mut net = super::Localnet::new(Config::resolve(&env));
    let data = net.cfg.validator_dir(0).join("data");
    let deadline = Instant::now() + Duration::from_secs(5);
    assert!(net.owned_validator_process(0).is_err());
    assert!(net
        .stopped_validator_header_output(0, 0, 42, deadline)
        .is_err());
    let mut command = Command::new("sh");
    command
        .args(["-c", "exec sleep 60", "node", "--datadir"])
        .arg(&data);
    let child = ChildGuard::spawn("header-owned-node", command).unwrap();
    let pid = child.pid();
    net.validators.insert(0, child);
    assert_eq!(net.owned_validator_process(0).unwrap(), (pid, None));
    assert!(net
        .stopped_validator_header_output(0, pid + 1, 42, deadline)
        .unwrap_err()
        .to_string()
        .contains("PID/datadir"));
    assert!(net
        .stopped_validator_header_output(0, pid, 42, deadline)
        .unwrap_err()
        .to_string()
        .contains("still running"));
    assert_eq!(net.owned_validator_process(0).unwrap(), (pid, None));
    net.validators
        .get_mut(&0)
        .unwrap()
        .fault_and_reap()
        .unwrap();
    let (observed_pid, status) = net.owned_validator_process(0).unwrap();
    assert_eq!(observed_pid, pid);
    assert!(status.is_some());
    assert!(net
        .stopped_validator_header_output(0, pid, 42, deadline)
        .is_err());
    assert!(
        !data.exists(),
        "evidence collection must not initialize missing storage"
    );
}

#[test]
fn absent_signing_share_is_not_a_malformed_share() {
    let root = tempfile::tempdir().unwrap();
    assert!(!has_required_signing_share(root.path()).unwrap());
    for relative in ["keys/dkg_share.hex", "data/keys/dkg_share.hex"] {
        let path = root.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not a share").unwrap();
        assert!(has_required_signing_share(root.path()).is_err());
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn dangling_or_non_file_share_is_an_observation_error() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("keys/dkg_share.hex");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(root.path().join("absent"), &path).unwrap();
    assert!(has_required_signing_share(root.path()).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(has_required_signing_share(root.path()).is_err());
    std::fs::remove_dir(&path).unwrap();
    assert!(std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .unwrap()
        .success());
    assert!(has_required_signing_share(root.path())
        .unwrap_err()
        .to_string()
        .contains("not a regular file"));
}

#[test]
fn follower_trace_path_uses_its_owned_validator_slot() {
    let root = Path::new("/run/scenario-1");
    assert_eq!(
        validator_slot_node_log_path(root, 14),
        root.join("validator-14/node.log")
    );
    assert_ne!(
        validator_slot_node_log_path(root, 14),
        root.join("follower/node.log")
    );
}

#[test]
fn missing_owned_follower_trace_log_is_an_error() {
    let root = tempfile::tempdir().unwrap();
    let path = validator_slot_node_log_path(root.path(), 14);
    let error = read_required_node_log(&path, "follower").unwrap_err();
    assert!(error
        .to_string()
        .contains("read OCOMP runtime trace for follower from owned log"));
}

#[test]
fn completed_ce_startup_replay_record_decodes_the_exact_span() {
    let target_hash = "0x2a6edf48ac3c8fb19ff4e9daeca617c06c61ced8ec8889a59fafbc00093d4bb7"
        .parse()
        .unwrap();
    let record = "2026-07-27T10:44:12.172399Z INFO outbe_engine::ce_recovery: \
        compressed-entity startup replay completed first_missing=1 target_height=166 \
        target_hash=0x2a6edf48ac3c8fb19ff4e9daeca617c06c61ced8ec8889a59fafbc00093d4bb7 \
        replayed_blocks=166 elapsed_micros=912345";

    assert_eq!(
        parse_ce_startup_replay(record, 0).unwrap(),
        Some(CeStartupReplayObservationV1 {
            validator_index: 0,
            first_missing_block_number: 1,
            target_block_number: 166,
            target_block_hash: target_hash,
            replayed_block_count: 166,
            elapsed_micros: 912_345,
        })
    );
}

#[test]
fn reth_log_tail_is_incremental_and_keeps_rotated_files_open() {
    use std::io::Write as _;

    let root = tempfile::tempdir().unwrap();
    let log_dir = root.path().join("chain");
    std::fs::create_dir_all(&log_dir).unwrap();
    let current = log_dir.join("reth.log");
    std::fs::write(&current, "old record\n").unwrap();
    let mut tail = RethLogTail::at_end(root.path()).unwrap();

    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&current)
        .unwrap();
    writeln!(writer, "replay record one").unwrap();
    writer.flush().unwrap();
    assert_eq!(
        tail.read_new_complete_lines().unwrap(),
        "replay record one\n"
    );
    assert!(tail.read_new_complete_lines().unwrap().is_empty());

    std::fs::rename(&current, log_dir.join("reth.log.1")).unwrap();
    std::fs::write(&current, "replay record two\n").unwrap();
    assert_eq!(
        tail.read_new_complete_lines().unwrap(),
        "replay record two\n"
    );
    assert!(tail.read_new_complete_lines().unwrap().is_empty());

    std::fs::write(
        &current,
        "replay record after copy-truncate and rapid refill\n",
    )
    .unwrap();
    assert_eq!(
        tail.read_new_complete_lines().unwrap(),
        "replay record after copy-truncate and rapid refill\n"
    );

    std::fs::write(&current, "partial").unwrap();
    assert!(tail.read_new_complete_lines().unwrap().is_empty());
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&current)
        .unwrap();
    writeln!(writer, " record").unwrap();
    writer.flush().unwrap();
    assert_eq!(tail.read_new_complete_lines().unwrap(), "partial record\n");

    let discovered = super::reth_log_paths(root.path()).unwrap();
    assert!(
        discovered
            .iter()
            .any(|path| path.file_name().unwrap() == "reth.log.1"),
        "already-rotated Reth logs must remain discoverable"
    );
}
