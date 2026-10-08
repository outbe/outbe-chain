use super::*;

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

pub(super) fn snapshot_command(
    command: &SnapshotCommandObservation,
    verb: &str,
) -> eyre::Result<()> {
    ensure!(
        command.argv.get(1).map(String::as_str) == Some("snapshot")
            && command.argv.get(2).map(String::as_str) == Some(verb),
        "missing actual snapshot {verb} invocation"
    );
    Ok(())
}

pub(super) fn native(progress: &SnapshotNativeProgress) -> eyre::Result<()> {
    for checkpoint in [
        &progress.finalized,
        &progress.execution,
        &progress.ce,
        &progress.projection,
        &progress.ocomp_baseline,
        &progress.ocomp_previous,
        &progress.ocomp_current,
    ] {
        ensure!(is_hash(&checkpoint.hash), "invalid native hash spelling");
    }
    ensure!(
        matches!(progress.storage_version, 1 | 2),
        "unknown native storage version"
    );
    // No H=E=Finish=Q=P=C assertion. A valid native tail/partial state is retained.
    Ok(())
}

pub(super) fn log_slice(log: &SnapshotLogSlice) -> eyre::Result<()> {
    ensure!(
        !log.path.as_os_str().is_empty() && log.inode != 0,
        "missing log source"
    );
    ensure!(
        log.end.checked_sub(log.start) == Some(log.bytes.len() as u64),
        "log interval/bytes mismatch"
    );
    Ok(())
}

pub(super) fn ordinary_launch(launch: &SnapshotLaunchObservation) -> eyre::Result<()> {
    ensure!(
        launch.pid != 0 && !launch.argv.is_empty(),
        "missing owned launch"
    );
    ensure!(
        !launch.before_launch.sources.is_empty(),
        "missing stopped native read"
    );
    ensure!(
        launch.before_launch.observed <= launch.started,
        "full native read must precede spawn"
    );
    native(&launch.before_launch.progress)?;
    let recovery = &launch.recovery;
    ensure!(
        recovery.pid == launch.pid && recovery.incarnation_started == launch.started,
        "recovery belongs to another incarnation"
    );
    ensure!(
        recovery.observed >= launch.started,
        "recovery record observed before spawn"
    );
    log_slice(&recovery.log)?;
    let line = std::str::from_utf8(&recovery.log.bytes)?;
    ensure!(
        line.contains("certified follower startup recovery barrier completed"),
        "missing actual startup recovery barrier record"
    );
    // Fields are parsed by the collector from this retained incarnation-bound record.
    // Full native fields above are never filled from this narrower startup log.
    ensure!(
        is_hash(&recovery.anchor.hash) && recovery.anchor == recovery.canonical,
        "wrong canonical recovery anchor"
    );
    ensure!(
        recovery.anchor.number >= launch.before_launch.progress.finalized.number,
        "genesis-like or regressed recovery anchor"
    );
    ensure!(
        recovery.marshal_processed <= recovery.anchor.number
            && recovery.anchor.number <= recovery.marshal_processed.saturating_add(1),
        "anchor outside ordinary processed frontier"
    );
    ensure!(
        recovery.last_execution_height >= recovery.anchor.number,
        "anchor exceeds observed execution tip"
    );
    // No anchor==H, last_execution_height==E, or reconciled CE==prelaunch Q claim.
    for arg in &launch.argv {
        let flag = arg.split('=').next().unwrap_or(arg);
        ensure!(
            ![
                "snapshot",
                "--manifest",
                "--signature",
                "--archive",
                "--report",
                "--validator",
                "--consensus.signing-key",
                "--validator.evm-key",
                "--upstream.nocertify"
            ]
            .contains(&flag),
            "nonordinary or validator authority argument: {flag}"
        );
    }
    Ok(())
}

pub(super) fn prior_started(prior: &SnapshotPriorFileObservation) -> u64 {
    let SnapshotPriorFileObservation::DirectoryListing(listing) = prior;
    listing.started
}

pub(super) fn newly_present<'a>(
    before: &SnapshotPriorFileObservation,
    after: &'a SnapshotFileRead,
    requested: u64,
    expected_root: &std::path::Path,
) -> eyre::Result<&'a [u8]> {
    let relative = after
        .path
        .strip_prefix(expected_root)
        .map_err(|_| eyre!("artifact outside owned root"))?;
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
        "invalid artifact relative path"
    );
    let SnapshotPriorFileObservation::DirectoryListing(listing) = before;
    ensure!(
        listing.root == expected_root
            && listing.started <= listing.completed
            && listing.completed <= requested,
        "wrong owned inventory root or observation interval"
    );
    ensure!(
        listing.directory_identity.is_some() || listing.entries.is_empty(),
        "absent directory cannot have entries"
    );
    let mut seen = std::collections::BTreeSet::new();
    for entry in &listing.entries {
        ensure!(
            !entry.as_os_str().is_empty()
                && entry
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
                && seen.insert(entry),
            "invalid or duplicate inventory path"
        );
    }
    ensure!(
        !listing.entries.iter().any(|entry| entry == relative),
        "later derived artifact already existed in pre-request inventory"
    );
    ensure!(
        requested <= after.observed,
        "artifact observed before request"
    );
    let bytes = after
        .bytes
        .as_deref()
        .ok_or_else(|| eyre!("missing new artifact"))?;
    ensure!(!bytes.is_empty(), "empty new artifact");
    Ok(bytes)
}

pub(super) fn worker_counters(body: &[u8]) -> eyre::Result<(u64, u64)> {
    let text = std::str::from_utf8(body)?;
    let mut started = None;
    let mut success = None;
    for line in text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
    {
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let target = if name == "outbe_ocomp_worker_units_started_total" {
            &mut started
        } else if name == "outbe_ocomp_worker_units_completed_total{outcome=\"success\"}" {
            &mut success
        } else {
            continue;
        };
        ensure!(target.is_none(), "duplicate worker counter series");
        *target = Some(
            parts
                .next()
                .ok_or_else(|| eyre!("counter without value"))?
                .parse::<u64>()?,
        );
    }
    // Counters are created lazily. An absent pre-work series is an observed zero.
    Ok((started.unwrap_or(0), success.unwrap_or(0)))
}

pub(super) fn owned_worker_at(owner: &SnapshotWorkerOwner, slot: u8, at: u64) -> eyre::Result<()> {
    let p = &owner.process;
    ensure!(
        p.role == crate::world::ocomp::OcompProcessRole::Worker
            && p.validator_index == Some(slot)
            && p.worker_ordinal.is_some(),
        "wrong owned recipient worker"
    );
    ensure!(
        p.pid != 0 && p.started_at_millis <= at && p.stopped_at_millis.is_none_or(|end| at <= end),
        "worker incarnation not alive at observation"
    );
    ensure!(!owner.endpoint.is_empty(), "missing owned worker endpoint");
    Ok(())
}

pub(super) fn actual_worker(new: &SnapshotNewJobObservation, slot: u8) -> eyre::Result<()> {
    use outbe_ocomp_protocol::{
        profile::poc_schema_limits,
        unit::{UnitArtifactV1, UnitSpecV1},
    };
    let worker = &new.worker;
    let from = worker.before.observed;
    let through = worker.after.observed;
    ensure!(
        from <= new.requested && new.requested <= through,
        "metrics do not bracket new job request"
    );
    for observation in [&worker.before, &worker.after] {
        ensure!(
            observation.owner == worker.owner,
            "counter belongs to a different worker/incarnation/endpoint"
        );
        owned_worker_at(&observation.owner, slot, observation.observed)?;
    }
    let before = worker_counters(&worker.before.body)?;
    let after = worker_counters(&worker.after.body)?;
    ensure!(
        before.1 <= before.0 && after.1 <= after.0,
        "worker success count exceeds started count"
    );
    ensure!(
        after.0 > before.0 && after.1 > before.1,
        "no actual worker start/success counter advancement"
    );
    let raw = newly_present(
        &worker.artifact_before,
        &worker.artifact_after,
        new.requested,
        &worker.owner.inbox_root.join("artifacts"),
    )?;
    ensure!(
        worker.artifact_after.observed <= through,
        "artifact observations outside worker interval"
    );
    ensure!(
        raw.len() as u64 == worker.admitted_artifact_len
            && alloy_primitives::keccak256(raw) == worker.admitted_artifact_keccak256,
        "artifact differs from admitted CAS length/hash"
    );
    ensure!(
        !worker.admission_catalog.as_os_str().is_empty(),
        "missing independent admission source"
    );
    let limits = poc_schema_limits();
    let spec = UnitSpecV1::decode_canonical(&worker.canonical_unit_spec, &limits)?;
    let artifact = UnitArtifactV1::decode_canonical(raw, &limits)?;
    artifact.validate_against(&spec, &limits)?;
    ensure!(
        hex::encode(artifact.job_id) == new.job_id
            && artifact.protocol_bundle_hash == worker.owner.bundle_hash,
        "wrong new-job/bundle artifact"
    );
    let filename = format!("{}.ocb1", hex::encode(artifact.unit_id));
    ensure!(
        worker.artifact_after.path == worker.owner.inbox_root.join("artifacts").join(&filename),
        "artifact is outside the owned bundle inbox"
    );
    ensure!(
        worker
            .artifact_after
            .path
            .file_name()
            .and_then(|v| v.to_str())
            == Some(filename.as_str()),
        "artifact filename is not the actual unit ID"
    );
    let SnapshotWorkerAttribution::SingleOwnedProducer {
        inventory_from,
        inventory_through,
        workers,
    } = &worker.attribution;
    ensure!(
        *inventory_from
            <= from
                .min(prior_started(&worker.artifact_before))
                .min(prior_started(&new.local_before))
            && *inventory_through >= through.max(new.local_after.observed),
        "owned worker inventory does not cover observation interval"
    );
    let overlapping: Vec<_> = workers
        .iter()
        .filter(|owner| {
            let p = &owner.process;
            let same_identity = p.role == crate::world::ocomp::OcompProcessRole::Worker
                && p.validator_index == Some(slot)
                && owner.bundle_hash == worker.owner.bundle_hash;
            let started_before = p.started_at_millis <= through.max(new.local_after.observed);
            let ended_after = p.stopped_at_millis.is_none_or(|end| {
                end >= from
                    .min(prior_started(&worker.artifact_before))
                    .min(prior_started(&new.local_before))
            });
            same_identity && started_before && ended_after
        })
        .collect();
    ensure!(
        overlapping.len() == 1 && overlapping[0] == &worker.owner,
        "shared inbox has multiple, restarted or missing owned producers"
    );
    if let Some(log) = &worker.log {
        log_slice(log)?;
    }
    Ok(())
}
