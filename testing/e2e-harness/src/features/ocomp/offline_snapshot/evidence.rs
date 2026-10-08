use super::*;

pub(crate) fn assert_snapshot_workflow(e: &OfflineSnapshotEvidence) -> eyre::Result<()> {
    let manifest = assert_created_manifest(e)?;
    assert_transfer_and_placement(e, &manifest)?;
    assert_first_start(e, &manifest)?;
    assert_new_job(e)?;
    assert_restart(e)
}

fn assert_created_manifest(
    e: &OfflineSnapshotEvidence,
) -> eyre::Result<SnapshotManifestObservation> {
    let create = e
        .create
        .as_ref()
        .ok_or_else(|| eyre!("missing create CLI phase"))?;
    snapshot_command(create, "create")?;
    successful_command(create)?;
    let raw_manifest: serde_json::Value = serde_json::from_slice(&e.manifest_bytes)?;
    let raw_progress = raw_manifest["progress"]
        .as_object()
        .ok_or_else(|| eyre!("missing native progress object"))?;
    for key in [
        "finalized",
        "execution",
        "execution_stage",
        "finish_stage",
        "partial_state_trie",
        "unwind",
        "storage_version",
        "ce",
        "projection",
        "ocomp_baseline",
        "ocomp_previous",
        "ocomp_current",
    ] {
        ensure!(
            raw_progress.contains_key(key),
            "unobserved native field {key}; retain explicit nulls"
        );
    }
    let manifest: SnapshotManifestObservation = serde_json::from_slice(&e.manifest_bytes)?;
    ensure!(
        manifest.version == 1 && manifest.chain_id == 54322345,
        "unexpected snapshot format/network"
    );
    native(&manifest.progress)?;
    ensure!(
        manifest.progress.finalized == e.cut_canonical,
        "wrong stopped H/hash"
    );
    let stdout = std::str::from_utf8(&create.stdout)?;
    // Match actual named stdout fields, including when the output path has spaces.
    for token in [
        format!("finalized_height={}", e.cut_canonical.number),
        format!("finalized_hash={}", e.cut_canonical.hash),
        "signature=verified".into(),
    ] {
        ensure!(
            stdout.split_whitespace().any(|field| field == token),
            "create output missing {token}"
        );
    }
    Ok(manifest)
}

fn assert_transfer_and_placement(
    e: &OfflineSnapshotEvidence,
    manifest: &SnapshotManifestObservation,
) -> eyre::Result<()> {
    let create = e
        .create
        .as_ref()
        .ok_or_else(|| eyre!("missing create CLI phase"))?;
    let transfer = e
        .transfer
        .as_ref()
        .ok_or_else(|| eyre!("missing transfer phase"))?;
    successful_command(transfer)?;
    ensure!(
        create.ended <= transfer.started,
        "transfer predates create completion"
    );
    ensure!(
        is_hash(&e.archive_sha256) && e.archive_sha256 == e.transferred_archive_sha256,
        "archive changed during transfer"
    );
    let placement = e
        .placement
        .as_ref()
        .ok_or_else(|| eyre!("missing native placement phase"))?;
    ensure!(
        transfer.ended <= placement.native.observed
            && placement.native.observed <= placement.completed,
        "placement/read interval invalid"
    );
    ensure!(
        !placement.native.sources.is_empty() && placement.native.progress == manifest.progress,
        "placed native state differs from signed cut"
    );
    ensure!(
        !e.identity_before.is_empty(),
        "missing protected path inventory"
    );
    for fingerprint in e.identity_before.values() {
        ensure!(is_hash(&fingerprint.sha256), "invalid identity fingerprint");
    }
    ensure!(
        e.identity_before == e.identity_placed
            && e.identity_before == e.identity_at_k
            && e.identity_before == e.identity_restarted,
        "protected identity changed"
    );
    Ok(())
}

fn assert_first_start(
    e: &OfflineSnapshotEvidence,
    manifest: &SnapshotManifestObservation,
) -> eyre::Result<()> {
    let placement = e
        .placement
        .as_ref()
        .ok_or_else(|| eyre!("missing native placement phase"))?;
    let first = e
        .first_start
        .as_ref()
        .ok_or_else(|| eyre!("missing first ordinary start"))?;
    ordinary_launch(first)?;
    ensure!(
        placement.completed <= first.before_launch.observed
            && first.before_launch.progress == placement.native.progress,
        "first start did not resume placed native state"
    );
    if let SnapshotValidationObservation::Run(command) = &e.validation {
        snapshot_command(command, "validate")?;
        ensure!(
            placement.completed <= command.started
                && command.started <= command.ended
                && command.ended <= first.before_launch.observed,
            "validation was not before first native writes"
        );
        let report = parse_snapshot_validation_report(&command.stdout)?;
        for (observed, actual) in [
            (&report.observed.h, &manifest.progress.finalized),
            (&report.observed.e, &manifest.progress.execution),
            (&report.observed.q, &manifest.progress.ce),
            (&report.observed.p, &manifest.progress.projection),
            (
                &report.observed.c_baseline,
                &manifest.progress.ocomp_baseline,
            ),
            (
                &report.observed.c_previous,
                &manifest.progress.ocomp_previous,
            ),
            (&report.observed.c_current, &manifest.progress.ocomp_current),
        ] {
            if let Some(observed) = observed {
                ensure!(observed == actual, "validation frontier differs from cut");
            }
        }
        ensure!(
            report.checks.values().any(|check| check.selected),
            "empty validation selection"
        );
        let passed = report
            .checks
            .values()
            .filter(|check| check.selected)
            .all(|check| check.status == SnapshotCheckStatus::Passed);
        if passed {
            successful_command(command)?;
        } else {
            ensure!(
                command.exit_code.is_some_and(|code| code != 0) && command.signal.is_none(),
                "nonpassing report disguised as CLI success"
            );
        }
        // A nonpassing optional audit remains nonpassing. It is not a startup gate.
    }
    Ok(())
}

fn assert_new_job(e: &OfflineSnapshotEvidence) -> eyre::Result<()> {
    let first = e
        .first_start
        .as_ref()
        .ok_or_else(|| eyre!("missing first ordinary start"))?;
    let new = e.new_job.as_ref().ok_or_else(|| eyre!("missing new job"))?;
    ensure!(
        new.job_id != e.copied_result.job_id && is_hash(&new.job_id),
        "copied old result mislabeled as new compute"
    );
    ensure!(
        new.request.number > e.cut_canonical.number && new.requested >= first.recovery.observed,
        "job was not requested after placement/start/H"
    );
    actual_worker(new, first.slot)?;
    let local_bytes = newly_present(
        &new.local_before,
        &new.local_after,
        new.requested,
        &new.local_result_root,
    )?;
    ensure!(
        new.local_after.path
            == new
                .local_result_root
                .join(format!("{}.lysis-result-v1.ocb1", new.job_id)),
        "local result filename does not bind the new JobId"
    );
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let decoded =
        outbe_ocomp_protocol::result::LysisResultV1::decode_canonical(local_bytes, &limits)?;
    ensure!(
        decoded.encode_canonical(&limits)? == local_bytes
            && hex::encode(decoded.job_id) == new.job_id
            && decoded.protocol_bundle_hash == new.worker.owner.bundle_hash
            && hex::encode(decoded.result_digest(&limits)?) == new.local_result.digest,
        "raw local result differs from new job/bundle/digest"
    );
    ensure!(
        new.local_after.observed >= new.worker.artifact_after.observed,
        "local result predates observed unit artifact"
    );
    ensure!(
        new.local_result.job_id == new.job_id
            && new.local_result == new.canonical_result
            && is_hash(&new.local_result.digest),
        "new local/canonical result binding differs"
    );
    // Fresh execution is established by the job, worker and artifact observations.
    ensure!(
        new.canonical_result_at.number >= new.request.number,
        "result predates request block"
    );
    Ok(())
}

fn assert_restart(e: &OfflineSnapshotEvidence) -> eyre::Result<()> {
    let first = e
        .first_start
        .as_ref()
        .ok_or_else(|| eyre!("missing first ordinary start"))?;
    let new = e.new_job.as_ref().ok_or_else(|| eyre!("missing new job"))?;
    let before = e
        .before_restart
        .as_ref()
        .ok_or_else(|| eyre!("missing current K native read"))?;
    native(&before.progress)?;
    let k = e
        .k_canonical
        .as_ref()
        .ok_or_else(|| eyre!("missing canonical K"))?;
    ensure!(
        !before.sources.is_empty()
            && before.progress.finalized == *k
            && k.number > e.cut_canonical.number
            && k.number >= new.canonical_result_at.number,
        "no catchup/current K identity"
    );
    let exit = e
        .first_exit
        .as_ref()
        .ok_or_else(|| eyre!("missing first incarnation reap"))?;
    ensure!(
        exit.pid == first.pid
            && exit.code == Some(0)
            && exit.signal.is_none()
            && exit.reaped >= new.worker.after.observed
            && exit.reaped >= new.local_after.observed,
        "first incarnation was not normally reaped"
    );
    let second = e
        .second_start
        .as_ref()
        .ok_or_else(|| eyre!("missing second ordinary restart"))?;
    ordinary_launch(second)?;
    ensure!(
        exit.reaped <= before.observed
            && before.observed <= second.before_launch.observed
            && second.slot == first.slot,
        "current K must be read in stopped gap"
    );
    ensure!(
        second.before_launch.progress == before.progress,
        "second restart did not resume current K"
    );
    Ok(())
}
