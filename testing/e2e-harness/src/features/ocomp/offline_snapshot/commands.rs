use super::*;

#[cfg(test)]
mod command_tests {
    use super::*;

    #[test]
    fn snapshot_cli_evidence_retains_failure_and_both_output_streams() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new("sh");
        command.args([
            "-c",
            "printf actual-output; printf actual-error >&2; exit 7",
        ]);
        let evidence = run_snapshot_command(
            command,
            dir.path(),
            "failure",
            std::time::Duration::from_secs(3),
        )
        .unwrap();
        assert_eq!(evidence.exit_code, Some(7));
        assert_eq!(evidence.signal, None);
        assert_eq!(evidence.stdout, b"actual-output");
        assert_eq!(evidence.stderr, b"actual-error");
        assert!(successful_command(&evidence).is_err());
    }

    #[test]
    fn snapshot_cli_timeout_reaps_its_exact_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("pid");
        let mut command = std::process::Command::new("sh");
        command.args([
            "-c",
            "printf '%s' $$ > \"$1\"; exec sleep 60",
            "snapshot-timeout",
        ]);
        command.arg(&pid_path);
        let result = run_snapshot_command(
            command,
            dir.path(),
            "timeout",
            std::time::Duration::from_secs(1),
        );
        assert!(result.unwrap_err().to_string().contains("deadline"));
        let pid: u32 = std::fs::read_to_string(pid_path).unwrap().parse().unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
}

pub(super) fn snapshot_now_millis() -> eyre::Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

/// Invoke the actual CLI, retaining outputs even when validation exits nonzero.
/// Timeout kills/reaps this owned child. It cannot leave a CLI writer behind.
pub(crate) fn run_snapshot_command(
    mut command: std::process::Command,
    evidence_dir: &std::path::Path,
    phase: &str,
    timeout: std::time::Duration,
) -> eyre::Result<SnapshotCommandObservation> {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;
    std::fs::create_dir_all(evidence_dir)?;
    let stdout = evidence_dir.join(format!("{phase}.stdout"));
    let stderr = evidence_dir.join(format!("{phase}.stderr"));
    let argv = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    command.stdout(Stdio::from(std::fs::File::create(&stdout)?));
    command.stderr(Stdio::from(std::fs::File::create(&stderr)?));
    let started = snapshot_now_millis()?;
    let mut child = crate::internal::proc::ChildGuard::spawn(format!("snapshot-{phase}"), command)?;
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.exit_status()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let status = child.stop_and_reap()?;
            eyre::bail!(
                "snapshot {phase} exceeded deadline; reaped pid={} status={status}; stderr={}",
                child.pid(),
                stderr.display()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    Ok(SnapshotCommandObservation {
        argv,
        started,
        ended: snapshot_now_millis()?,
        exit_code: status.code(),
        signal: status.signal(),
        stdout: std::fs::read(stdout)?,
        stderr: std::fs::read(stderr)?,
    })
}

#[cfg(test)]
mod observation_tests {
    use super::*;

    #[test]
    fn recovery_fields_come_from_complete_existing_record_not_later_head() {
        let hash = "ab".repeat(32);
        let log = format!("unrelated height=900\nINFO certified follower startup recovery barrier completed marshal_processed=100 recovery_height=101 recovery_hash=0x{hash} ce_marker_height=101 last_execution_height=102\nsubsequent finalized=200\n");
        let (processed, anchor, ce, execution) = parse_recovery_record(&log).unwrap();
        assert_eq!(
            (processed, anchor.number, ce, execution),
            (100, 101, 101, 102)
        );
        assert_eq!(anchor.hash, hash);
        assert!(parse_recovery_record("subsequent finalized=200").is_err());
        assert!(parse_recovery_record(&log.replace("recovery_hash=", "missing_hash=")).is_err());
    }

    #[test]
    fn pre_request_inventory_records_actual_existing_paths_and_absent_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("results");
        let absent = observe_snapshot_directory(&root).unwrap();
        assert!(absent.directory_identity.is_none());
        assert!(absent.entries.is_empty());
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("old.ocb1"), b"old result").unwrap();
        let before = observe_snapshot_directory(&root).unwrap();
        std::fs::write(root.join("new.ocb1"), b"new result").unwrap();
        assert_eq!(before.entries, vec![std::path::PathBuf::from("old.ocb1")]);
        assert!(before.directory_identity.is_some());
        assert_eq!(observe_snapshot_directory(&root).unwrap().entries.len(), 2);
    }
}

pub(crate) fn parse_recovery_record(log: &str) -> eyre::Result<(u64, SnapshotBlock, u64, u64)> {
    let mut records = log
        .lines()
        .filter(|line| line.contains("certified follower startup recovery barrier completed"));
    let record = records
        .next()
        .ok_or_else(|| eyre!("missing startup recovery record"))?;
    ensure!(
        records.next().is_none(),
        "multiple startup barriers in one incarnation"
    );
    let field = |key: &str| -> eyre::Result<&str> {
        let prefix = format!("{key}=");
        record
            .split_whitespace()
            .find_map(|word| word.strip_prefix(&prefix))
            .ok_or_else(|| eyre!("missing {key} in startup record"))
    };
    let hash: alloy_primitives::B256 = field("recovery_hash")?.parse()?;
    Ok((
        field("marshal_processed")?.parse()?,
        SnapshotBlock {
            number: field("recovery_height")?.parse()?,
            hash: hex::encode(hash),
        },
        field("ce_marker_height")?.parse()?,
        field("last_execution_height")?.parse()?,
    ))
}

pub(super) fn observe_snapshot_directory(
    root: &std::path::Path,
) -> eyre::Result<crate::world::state::SnapshotDirectoryListing> {
    use std::os::unix::fs::MetadataExt;
    let started = snapshot_now_millis()?;
    let identity = match std::fs::metadata(root) {
        Ok(meta) => {
            ensure!(
                meta.is_dir(),
                "expected evidence directory {}",
                root.display()
            );
            Some((meta.dev(), meta.ino()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut entries = Vec::new();
    if identity.is_some() {
        for item in std::fs::read_dir(root)? {
            entries.push(std::path::PathBuf::from(item?.file_name()));
        }
    }
    // Stable report order only: correctness does not require the native files to be sorted.
    entries.sort();
    Ok(crate::world::state::SnapshotDirectoryListing {
        root: root.to_path_buf(),
        directory_identity: identity,
        started,
        completed: snapshot_now_millis()?,
        entries,
    })
}

#[cucumber::then("the offline snapshot workflow evidence is complete")]
pub(super) fn completed_snapshot_workflow(world: &mut crate::world::World) {
    let evidence = world
        .state
        .offline_snapshot
        .as_ref()
        .expect("snapshot workflow evidence");
    assert_snapshot_workflow(evidence).expect("complete ordinary snapshot workflow");
    let path = world
        .localnet
        .scenario_dir()
        .join("offline-snapshot-evidence.json");
    std::fs::write(
        path,
        serde_json::to_vec_pretty(evidence).expect("encode actual snapshot evidence"),
    )
    .expect("retain actual snapshot evidence");
}

// Task09 harness-only fragment for features/ocomp/offline_snapshot.rs.
// The scenario establishes stopped writer ownership before calling this reader.
// All handles and temporary secondary files are dropped before ordinary launch.

pub(super) fn resume_snapshot_prices(
    world: &mut crate::world::World,
    previous: Option<u64>,
) -> eyre::Result<()> {
    let ports = world.validators.committee_ports();
    let target = world.rpc.fresh_finality_target(&ports)?;
    world.rpc.wait_finalized_checkpoint(&ports, target, 60)?;
    if let Some(pending) =
        crate::features::price_oracle::resume_after_clock_restart(world, previous)
    {
        while !crate::features::price_oracle::observe_pending_publication(world, &pending) {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    Ok(())
}
