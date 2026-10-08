use super::*;

/// Exercise public CLI failures without modifying the ready recipient databases.
pub(super) struct SnapshotArtifactCheck<'a> {
    pub(super) program: &'a std::path::Path,
    pub(super) archive: &'a std::path::Path,
    pub(super) manifest: &'a std::path::Path,
    pub(super) signature: &'a std::path::Path,
    pub(super) evidence_dir: &'a std::path::Path,
    pub(super) creator: &'a str,
    pub(super) chain: &'a str,
    pub(super) node: &'a std::path::Path,
}

pub(super) fn snapshot_rejects_damaged_artifact(
    check: SnapshotArtifactCheck<'_>,
) -> eyre::Result<()> {
    let SnapshotArtifactCheck {
        program,
        archive,
        manifest,
        signature,
        evidence_dir,
        creator,
        chain,
        node,
    } = check;
    use std::io::Read;
    use std::process::Command;
    use std::time::Duration;
    let original = std::fs::read(manifest)?;
    let changed = evidence_dir.join("changed-manifest.json");
    let mut bytes = original.clone();
    // Still valid JSON, but no longer the exact bytes signed by the producer.
    bytes.push(b'\n');
    std::fs::write(&changed, bytes)?;
    let mut command = Command::new(program);
    command
        .args([
            "snapshot",
            "validate",
            "--checks",
            "provenance",
            "--manifest",
        ])
        .arg(&changed)
        .arg("--signature")
        .arg(signature)
        .args(["--expected-signer", creator]);
    let rejected = run_snapshot_command(
        command,
        evidence_dir,
        "changed-manifest",
        Duration::from_secs(60),
    )?;
    let report = parse_snapshot_validation_report(&rejected.stdout)?;
    ensure!(
        rejected.exit_code.is_some_and(|code| code != 0)
            && report.checks["provenance"].status == SnapshotCheckStatus::Failed,
        "changed signed manifest was accepted"
    );
    std::fs::remove_file(changed)?;

    // Retain only the two complete metadata members of the actual archive.
    // No payload bytes have arrived. Valid metadata must not imply file success.
    let signature_len = std::fs::metadata(signature)?.len();
    let prefix_len =
        1024 + (original.len() as u64).div_ceil(512) * 512 + signature_len.div_ceil(512) * 512;
    ensure!(
        std::fs::metadata(archive)?.len() > prefix_len,
        "no archive payload"
    );
    let partial = evidence_dir.join("partial-transfer.tar");
    std::io::copy(
        &mut std::fs::File::open(archive)?.take(prefix_len),
        &mut std::fs::File::create(&partial)?,
    )?;
    let mut command = Command::new(program);
    command
        .args(["snapshot", "validate", "--checks", "files", "--archive"])
        .arg(&partial)
        .args(["--expected-signer", creator, "--", "--chain", chain])
        .arg("--datadir")
        .arg(node.join("data"))
        .arg("--consensus.storage-dir")
        .arg(node.join("data/consensus"))
        .arg("--projection.storage-config")
        .arg(node.join("offchain-storage.toml"));
    let rejected = run_snapshot_command(
        command,
        evidence_dir,
        "partial-transfer",
        Duration::from_secs(60),
    )?;
    let report = parse_snapshot_validation_report(&rejected.stdout)?;
    ensure!(
        rejected.exit_code.is_some_and(|code| code != 0)
            && report.checks["provenance"].status == SnapshotCheckStatus::Passed
            && matches!(
                report.checks["files"].status,
                SnapshotCheckStatus::Failed | SnapshotCheckStatus::Incomplete
            ),
        "partial artifact was accepted or failed for an unrelated provenance reason"
    );
    std::fs::remove_file(partial)?;
    Ok(())
}

pub(super) fn snapshot_pending_cut(
    progress: &SnapshotNativeProgress,
    mut read: impl FnMut(&SnapshotBlock) -> eyre::Result<serde_json::Value>,
) -> eyre::Result<serde_json::Value> {
    let execution = read(&progress.execution)?;
    Ok(serde_json::json!({"finalized_block":progress.finalized,"execution":execution}))
}
