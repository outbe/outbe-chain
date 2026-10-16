//! Native snapshot commands and conventional payload placement.
use crate::world::state::{SnapshotBlock, SnapshotCommandObservation};
use eyre::{ensure, eyre};

pub(super) fn snapshot_option(argv: &[String], flag: &str) -> eyre::Result<String> {
    for (index, arg) in argv.iter().enumerate() {
        if arg == flag {
            return argv
                .get(index + 1)
                .cloned()
                .ok_or_else(|| eyre!("missing value for {flag}"));
        }
        if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            return Ok(value.to_owned());
        }
    }
    Err(eyre!("ordinary node command has no {flag}"))
}

pub(super) fn canonical_snapshot_block(
    world: &crate::world::World,
    number: u64,
) -> eyre::Result<SnapshotBlock> {
    let hash = world
        .rpc
        .block_hash(world.validators.primary_port(), number)
        .ok_or_else(|| eyre!("missing upstream canonical block {number}"))?
        .parse::<alloy_primitives::B256>()?;
    Ok(SnapshotBlock {
        number,
        hash: hex::encode(hash),
    })
}

pub(super) fn snapshot_now_millis() -> eyre::Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

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

pub(crate) fn parse_recovery_record(log: &str) -> eyre::Result<(u64, SnapshotBlock, u64)> {
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
        field("last_execution_height")?.parse()?,
    ))
}

pub(crate) fn successful_command(command: &SnapshotCommandObservation) -> eyre::Result<()> {
    ensure!(!command.argv.is_empty(), "missing actual argv");
    ensure!(command.started <= command.ended, "invalid command interval");
    ensure!(
        command.exit_code == Some(0) && command.signal.is_none(),
        "command failed: {:?}",
        String::from_utf8_lossy(&command.stderr)
    );
    Ok(())
}

pub(crate) fn place_snapshot_payload(
    world: &crate::world::World,
    archive: &std::path::Path,
    evidence_dir: &std::path::Path,
) -> eyre::Result<(Vec<u8>, Vec<u8>)> {
    use std::process::Command;
    // Keep the unpacked MDBX files off the live validators' disk. A second
    // placement unpacks the same signed archive again after the first closes.
    let shm = std::path::Path::new("/dev/shm");
    let archive_size = u128::from(std::fs::metadata(archive)?.len());
    let scratch = if shm.is_dir()
        && rustix::fs::statvfs(shm).is_ok_and(|stat| {
            let free = u128::from(stat.f_bavail) * u128::from(stat.f_frsize);
            free > archive_size + 2 * 1024 * 1024 * 1024
        }) {
        tempfile::Builder::new()
            .prefix("outbe-snapshot-")
            .tempdir_in(shm)?
    } else {
        tempfile::tempdir()?
    };
    eprintln!("snapshot placement scratch: {}", scratch.path().display());
    let mut command = Command::new("tar");
    command
        .args(["--no-same-owner", "-xpf"])
        .arg(archive)
        .arg("-C")
        .arg(scratch.path());
    let extracted = run_snapshot_command(
        command,
        evidence_dir,
        "extract",
        std::time::Duration::from_secs(600),
    )?;
    successful_command(&extracted)?;
    let manifest = std::fs::read(scratch.path().join("manifest.json"))?;
    let signature = std::fs::read(scratch.path().join("signature.json"))?;
    let value: serde_json::Value = serde_json::from_slice(&manifest)?;
    let data = world.validators.data_dir(world.validators.joiner_index());
    for domain in value["domains"]
        .as_array()
        .ok_or_else(|| eyre!("missing domain inventory"))?
    {
        let id = domain["id"]
            .as_str()
            .ok_or_else(|| eyre!("missing domain id"))?;
        let entries = domain["entries"]
            .as_array()
            .ok_or_else(|| eyre!("missing entries"))?;
        if entries.is_empty() {
            continue;
        }
        let target = match domain["native_root"]
            .as_str()
            .ok_or_else(|| eyre!("missing target root"))?
        {
            "chain" => data.clone(),
            "consensus" => data.join("consensus"),
            "static-files" => data.join("static_files"),
            "execution-rocks-db" => data.join("rocksdb"),
            root => return Err(eyre!("unknown native root {root}")),
        };
        std::fs::create_dir_all(&target)?;
        let mut command = Command::new("cp");
        command
            .args([
                "-a",
                "--reflink=auto",
                "--sparse=always",
                "--no-preserve=ownership",
                "--",
            ])
            .arg(scratch.path().join("payload").join(id).join("."))
            .arg(&target);
        successful_command(&run_snapshot_command(
            command,
            evidence_dir,
            &format!("place-{id}"),
            std::time::Duration::from_secs(600),
        )?)?;
    }
    scratch.close()?;
    Ok((manifest, signature))
}

pub(crate) fn assert_local_committee_anchor(text: &str, finalized_height: u64) -> eyre::Result<()> {
    let record = text
        .lines()
        .find(|line| line.contains("follower restored committee from local finalized history"))
        .ok_or_else(|| eyre!("snapshot fullnode rebuilt committee history from genesis"))?;
    let field = |key: &str| -> eyre::Result<u64> {
        let prefix = format!("{key}=");
        record
            .split_whitespace()
            .find_map(|word| word.strip_prefix(&prefix))
            .ok_or_else(|| eyre!("missing {key} in local committee anchor"))?
            .parse()
            .map_err(Into::into)
    };
    let epoch = field("anchor_epoch")?;
    let height = field("anchor_height")?;
    ensure!(
        epoch > 0 && height > 1 && height <= finalized_height,
        "snapshot fullnode did not resume from a non-genesis finalized committee anchor"
    );
    Ok(())
}
