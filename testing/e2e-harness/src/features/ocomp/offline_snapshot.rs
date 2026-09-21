//! Real offline snapshot CLI and ordinary lifecycle acceptance evidence.
use crate::world::state::{
    OfflineSnapshotEvidence, SnapshotBlock, SnapshotCommandObservation, SnapshotFileRead,
    SnapshotLaunchObservation, SnapshotLogSlice, SnapshotManifestObservation,
    SnapshotNativeProgress, SnapshotNewJobObservation, SnapshotPriorFileObservation,
    SnapshotValidationObservation, SnapshotWorkerAttribution, SnapshotWorkerOwner,
};
use eyre::{ensure, eyre};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SnapshotCheckStatus {
    Passed,
    Failed,
    Incomplete,
    NotRequested,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SnapshotCheckReport {
    pub selected: bool,
    pub status: SnapshotCheckStatus,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SnapshotObservedFrontiers {
    pub h: Option<SnapshotBlock>,
    pub e: Option<SnapshotBlock>,
    pub q: Option<SnapshotBlock>,
    pub p: Option<SnapshotBlock>,
    pub c_baseline: Option<SnapshotBlock>,
    pub c_previous: Option<SnapshotBlock>,
    pub c_current: Option<SnapshotBlock>,
}

/// Subset of actual public JSON, with the remaining report retained as raw bytes.
#[derive(Debug, Deserialize)]
pub(crate) struct SnapshotValidationReport {
    pub checks: BTreeMap<String, SnapshotCheckReport>,
    pub observed: SnapshotObservedFrontiers,
}

pub(crate) fn parse_snapshot_validation_report(
    bytes: &[u8],
) -> eyre::Result<SnapshotValidationReport> {
    let report: SnapshotValidationReport = serde_json::from_slice(bytes)?;
    for name in [
        "files",
        "provenance",
        "headers",
        "evm",
        "ce",
        "bodies",
        "ocomp",
    ] {
        let check = report
            .checks
            .get(name)
            .ok_or_else(|| eyre!("missing check {name}"))?;
        ensure!(
            check.selected != (check.status == SnapshotCheckStatus::NotRequested),
            "selection/status mismatch for {name}"
        );
    }
    ensure!(report.checks.len() == 7, "unexpected check names");
    if report.checks["bodies"].status == SnapshotCheckStatus::Passed {
        let q = report
            .observed
            .q
            .as_ref()
            .ok_or_else(|| eyre!("passed bodies without Q"))?;
        let p = report
            .observed
            .p
            .as_ref()
            .ok_or_else(|| eyre!("passed bodies without P"))?;
        ensure!(q == p, "body equality claimed for different Q/P identities");
    }
    // Incomplete is intentionally preserved; parsing is not validation success.
    Ok(report)
}

fn successful_command(command: &SnapshotCommandObservation) -> eyre::Result<()> {
    ensure!(!command.argv.is_empty(), "missing actual argv");
    ensure!(command.started <= command.ended, "invalid command interval");
    ensure!(
        command.exit_code == Some(0) && command.signal.is_none(),
        "command failed: {:?}",
        String::from_utf8_lossy(&command.stderr)
    );
    Ok(())
}

fn snapshot_command(command: &SnapshotCommandObservation, verb: &str) -> eyre::Result<()> {
    ensure!(
        command.argv.get(1).map(String::as_str) == Some("snapshot")
            && command.argv.get(2).map(String::as_str) == Some(verb),
        "missing actual snapshot {verb} invocation"
    );
    Ok(())
}

fn native(progress: &SnapshotNativeProgress) -> eyre::Result<()> {
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

fn log_slice(log: &SnapshotLogSlice) -> eyre::Result<()> {
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

fn ordinary_launch(launch: &SnapshotLaunchObservation) -> eyre::Result<()> {
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

fn prior_started(prior: &SnapshotPriorFileObservation) -> u64 {
    let SnapshotPriorFileObservation::DirectoryListing(listing) = prior;
    listing.started
}

fn newly_present<'a>(
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

fn worker_counters(body: &[u8]) -> eyre::Result<(u64, u64)> {
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
    // Counters are created lazily; an absent pre-work series is an observed zero.
    Ok((started.unwrap_or(0), success.unwrap_or(0)))
}

fn owned_worker_at(owner: &SnapshotWorkerOwner, slot: u8, at: u64) -> eyre::Result<()> {
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

fn actual_worker(new: &SnapshotNewJobObservation, slot: u8) -> eyre::Result<()> {
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
            p.role == crate::world::ocomp::OcompProcessRole::Worker
                && p.validator_index == Some(slot)
                && owner.bundle_hash == worker.owner.bundle_hash
                && p.started_at_millis <= through.max(new.local_after.observed)
                && p.stopped_at_millis.is_none_or(|end| {
                    end >= from
                        .min(prior_started(&worker.artifact_before))
                        .min(prior_started(&new.local_before))
                })
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

fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// Narrow relational assertion for Task09 Tests-first 1, not full acceptance.
/// A passed assertion cannot replace signature/placement/role/public-action checks.
pub(crate) fn assert_snapshot_workflow(e: &OfflineSnapshotEvidence) -> eyre::Result<()> {
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
        // A nonpassing optional audit remains nonpassing; it is not a startup gate.
    }
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
// Unit fixtures only: these bytes/process records are NOT runtime acceptance evidence.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::state::*;
    use std::collections::BTreeMap;

    fn block(number: u64) -> SnapshotBlock {
        SnapshotBlock {
            number,
            hash: format!("{number:064x}"),
        }
    }
    fn native(h: u64) -> SnapshotNativeProgress {
        SnapshotNativeProgress {
            finalized: block(h),
            execution: block(h + 1),
            execution_stage: Some(h + 1),
            finish_stage: Some(h),
            partial_state_trie: Some(h),
            unwind: None,
            storage_version: 2,
            ce: block(h - 1),
            projection: block(h),
            ocomp_baseline: block(1),
            ocomp_previous: block(h - 1),
            ocomp_current: block(h),
        }
    }
    fn command(verb: &str, at: u64) -> SnapshotCommandObservation {
        SnapshotCommandObservation {
            argv: vec![
                "/release/outbe-chain".into(),
                "snapshot".into(),
                verb.into(),
                "--".into(),
                "--datadir".into(),
                "/fixture/receiver".into(),
            ],
            started: at,
            ended: at + 1,
            exit_code: Some(0),
            signal: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }
    fn metrics(started: u64, success: u64) -> Vec<u8> {
        format!("outbe_ocomp_worker_units_started_total {started}\noutbe_ocomp_worker_units_completed_total{{outcome=\"success\"}} {success}\n").into_bytes()
    }
    fn worker_fixture() -> SnapshotWorkerExecution {
        use alloy_primitives::B256;
        use outbe_ocomp_protocol::{
            common::BoundedBytes,
            profile::poc_schema_limits,
            unit::{
                BinaryReducerNode, UnitArtifactV1, UnitInterval, UnitPhase, UnitSpecV1,
                WorkOutputHeaderV1,
            },
        };
        let limits = poc_schema_limits();
        let spec = UnitSpecV1 {
            protocol_bundle_hash: B256::repeat_byte(0x77),
            job_id: B256::repeat_byte(0x55),
            attempt: 0,
            phase: UnitPhase::FixedReduce,
            interval: UnitInterval::BinaryReducerNode(BinaryReducerNode { level: 0, index: 0 }),
            canonical_ordered_inputs: vec![],
            lysis_program_semantics_hash: B256::repeat_byte(0x88),
            planner_spec_version: 1,
            reducer_spec_version: 1,
        };
        // Valid canonical unit fixture, not evidence of an executed production job.
        let artifact = UnitArtifactV1::from_canonical_output(
            &spec,
            WorkOutputHeaderV1 {
                source_coverage_root: B256::repeat_byte(0x81),
                output_coverage_root: B256::repeat_byte(0x82),
                source_coverage_count: 1,
                output_coverage_count: 1,
            },
            BoundedBytes(vec![1]),
            &limits,
        )
        .unwrap();
        let bytes = artifact.encode_canonical(&limits).unwrap();
        let path = std::path::PathBuf::from(format!(
            "/fixture/worker-inbox-v1/{}/artifacts/{}.ocb1",
            hex::encode(spec.protocol_bundle_hash),
            hex::encode(artifact.unit_id)
        ));
        let owner = SnapshotWorkerOwner {
            process: crate::world::ocomp::OcompProcessRecordV1 {
                validator_index: Some(4),
                role: crate::world::ocomp::OcompProcessRole::Worker,
                worker_ordinal: Some(0),
                pid: 2001,
                started_at_millis: 10,
                stopped_at_millis: None,
            },
            endpoint: "http://127.0.0.1:41000".into(),
            inbox_root: path.parent().unwrap().parent().unwrap().to_path_buf(),
            bundle_hash: spec.protocol_bundle_hash,
        };
        SnapshotWorkerExecution {
            before: SnapshotWorkerHttpObservation {
                owner: owner.clone(),
                observed: 11,
                body: metrics(0, 0),
            },
            after: SnapshotWorkerHttpObservation {
                owner: owner.clone(),
                observed: 17,
                body: metrics(1, 1),
            },
            attribution: SnapshotWorkerAttribution::SingleOwnedProducer {
                inventory_from: 7,
                inventory_through: 18,
                workers: vec![owner.clone()],
            },
            owner,
            artifact_before: SnapshotPriorFileObservation::DirectoryListing(
                SnapshotDirectoryListing {
                    root: path.parent().unwrap().to_path_buf(),
                    directory_identity: Some((1, 3)),
                    started: 7,
                    completed: 7,
                    entries: vec![],
                },
            ),
            admitted_artifact_len: bytes.len() as u64,
            admitted_artifact_keccak256: alloy_primitives::keccak256(&bytes),
            artifact_after: SnapshotFileRead {
                path,
                observed: 15,
                bytes: Some(bytes),
            },
            admission_catalog: "/fixture/new-job/admissions".into(),
            canonical_unit_spec: spec.encode_canonical(&limits).unwrap(),
            log: Some(SnapshotLogSlice {
                path: "/fixture/worker.log".into(),
                device: 1,
                inode: 2,
                start: 0,
                end: 0,
                bytes: vec![],
            }),
        }
    }
    fn local_result_fixture() -> (
        alloy_primitives::B256,
        outbe_ocomp_protocol::result::LysisResultV1,
        Vec<u8>,
    ) {
        use alloy_primitives::{B256, U256};
        use outbe_ocomp_protocol::{
            hash::hash_framed,
            intent::DayType,
            profile::poc_schema_limits,
            registry::HashDomain,
            result::{
                lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
                CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisArithmeticSummaryV1,
                LysisResultV1, MetadosisCompletionSummaryV1, ResultRootsV1,
            },
        };

        let limits = poc_schema_limits();
        let roots = ResultRootsV1 {
            nod_root: B256::repeat_byte(0x31),
            bucket_root: B256::repeat_byte(0x32),
            contributor_root: B256::repeat_byte(0x33),
            output_manifest_root: B256::repeat_byte(0x34),
        };
        let counts = ExactCountsV1 {
            tribute_count: 1,
            nod_count: 1,
            bucket_count: 0,
            contributor_count: 0,
            semantic_event_count: 0,
        };
        let conservation = ConservationTotalsV1 {
            tribute_nominal_total: U256::ZERO,
            eligible_nominal_total: U256::ZERO,
            day_limit: U256::ZERO,
            gratis_demand: U256::ZERO,
            day_gratis_limit_minor: U256::ZERO,
            lysis_limit_minor: U256::ZERO,
            desis_limit_minor: U256::ZERO,
            lysis_allocation_minor: U256::ZERO,
            unused_lysis_limit_minor: U256::ZERO,
            carry_over_credit: U256::ZERO,
            nod_cost_total: U256::ZERO,
        };
        let summary = LysisArithmeticSummaryV1 {
            input_manifest_hash: B256::repeat_byte(0x35),
            plan_hash: B256::repeat_byte(0x36),
            unit_artifact_root: B256::repeat_byte(0x37),
            fidelity_fraction_root: B256::repeat_byte(0x38),
            gratis_prefix_root: B256::repeat_byte(0x39),
            roots: roots.clone(),
            counts: counts.clone(),
            conservation: conservation.clone(),
            first_error_ordinal: None,
        };
        let job_id = B256::repeat_byte(0x55);
        let result = LysisResultV1 {
            protocol_bundle_hash: B256::repeat_byte(0x77),
            job_id,
            attempt: 0,
            input_manifest_hash: summary.input_manifest_hash,
            plan_hash: summary.plan_hash,
            unit_artifact_root: summary.unit_artifact_root,
            fidelity_fraction_root: summary.fidelity_fraction_root,
            gratis_prefix_root: summary.gratis_prefix_root,
            result_chunk_count: 1,
            result_chunk_list_root: B256::repeat_byte(0x3a),
            carry_over_credit: CarryOverCreditActionV1 {
                source_wwd: 1,
                reason: CarryOverReason::UnusedLysis,
                amount: U256::ZERO,
            },
            metadosis_completion_summary: MetadosisCompletionSummaryV1 {
                wwd: 1,
                pending_nonce: 0,
                day_type: DayType::Green,
                tribute_nominal_total: U256::ZERO,
                day_limit: U256::ZERO,
                gratis_demand: U256::ZERO,
                day_gratis_limit_minor: U256::ZERO,
                lysis_limit_minor: U256::ZERO,
                desis_limit_minor: U256::ZERO,
                lysis_allocation_minor: U256::ZERO,
                unused_lysis_limit_minor: U256::ZERO,
                carry_over_credit: U256::ZERO,
                status: CompletionStatus::Completed,
                logical_evaluation_height: 1,
                logical_evaluation_time: 1,
            },
            tribute_count: 1,
            tribute_nominal_total: U256::ZERO,
            unused_lysis_limit_minor: U256::ZERO,
            roots,
            counts,
            conservation,
            arithmetic_commitment: hash_framed(
                HashDomain::LysisArithmetic,
                &summary.encode_canonical(&limits).unwrap(),
            )
            .unwrap(),
            event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
        };
        let encoded = result
            .encode_canonical(&limits)
            .expect("fixture result encodes canonically");
        (job_id, result, encoded)
    }

    fn fixture() -> OfflineSnapshotEvidence {
        let (_, local, local_bytes) = local_result_fixture();
        let local_digest = hex::encode(
            local
                .result_digest(&outbe_ocomp_protocol::profile::poc_schema_limits())
                .unwrap(),
        );
        let progress = native(10);
        let manifest = serde_json::to_vec(
            &serde_json::json!({"version":1,"chain_id":54322345,"progress":progress}),
        )
        .unwrap();
        let archive = b"unit fixture archive, not a native snapshot";
        let mut create = command("create", 1);
        create.stdout = format!("snapshot=/fixture/cut.tar finalized_height=10 finalized_hash={} files=1 bytes=42 creator_public_key={} signature=verified\n", block(10).hash, "02".to_owned() + &"11".repeat(32)).into_bytes();
        let identity = BTreeMap::from([(
            "/fixture/receiver/own.key".into(),
            SnapshotFingerprint {
                sha256: "22".repeat(32),
                mode: 0o600,
            },
        )]);
        let launch = |at, pid, progress: SnapshotNativeProgress| {
            let anchor = block(progress.finalized.number + 1);
            let record = format!("certified follower startup recovery barrier completed marshal_processed={} recovery_height={} recovery_hash=0x{} ce_marker_height={} last_execution_height={}\n", anchor.number - 1, anchor.number, anchor.hash, anchor.number, anchor.number + 1).into_bytes();
            SnapshotLaunchObservation {
                slot: 4,
                started: at,
                pid,
                argv: vec![
                    "/release/outbe-chain".into(),
                    "node".into(),
                    "--datadir".into(),
                    "/fixture/receiver".into(),
                ],
                before_launch: SnapshotNativeObservation {
                    progress,
                    sources: vec!["/fixture/receiver/db".into()],
                    observed: at - 1,
                },
                recovery: SnapshotRecoveryObservation {
                    pid,
                    incarnation_started: at,
                    observed: at + 1,
                    log: SnapshotLogSlice {
                        path: "/fixture/receiver/node.log".into(),
                        device: 1,
                        inode: 1,
                        start: 0,
                        end: record.len() as u64,
                        bytes: record,
                    },
                    marshal_processed: anchor.number - 1,
                    ce_marker_height: anchor.number,
                    last_execution_height: anchor.number + 1,
                    canonical: anchor.clone(),
                    anchor,
                },
            }
        };
        OfflineSnapshotEvidence {
            create: Some(create),
            manifest_bytes: manifest,
            archive_sha256: sha256(archive),
            transferred_archive_sha256: sha256(archive),
            transfer: Some(SnapshotCommandObservation {
                argv: vec!["cp".into(), "cut.tar".into(), "receiver.tar".into()],
                ..command("unused", 3)
            }),
            placement: Some(SnapshotPlacementObservation {
                completed: 6,
                native: SnapshotNativeObservation {
                    progress: native(10),
                    sources: vec!["/fixture/receiver/db".into()],
                    observed: 5,
                },
            }),
            validation: SnapshotValidationObservation::NotRun,
            cut_canonical: block(10),
            identity_before: identity.clone(),
            identity_placed: identity.clone(),
            identity_at_k: identity.clone(),
            identity_restarted: identity,
            first_start: Some(launch(8, 1001, native(10))),
            copied_result: SnapshotResultObservation {
                job_id: "33".repeat(32),
                digest: "44".repeat(32),
            },
            new_job: Some(SnapshotNewJobObservation {
                requested: 12,
                request: block(12),
                job_id: "55".repeat(32),
                worker: worker_fixture(),
                local_before: SnapshotPriorFileObservation::DirectoryListing(
                    SnapshotDirectoryListing {
                        root: "/fixture/new-job".into(),
                        directory_identity: Some((1, 4)),
                        started: 7,
                        completed: 7,
                        entries: vec![],
                    },
                ),
                local_result_root: "/fixture/new-job".into(),
                local_after: SnapshotFileRead {
                    path: std::path::PathBuf::from("/fixture/new-job")
                        .join(format!("{}.lysis-result-v1.ocb1", "55".repeat(32))),
                    observed: 18,
                    bytes: Some(local_bytes),
                },
                local_result: SnapshotResultObservation {
                    job_id: "55".repeat(32),
                    digest: local_digest.clone(),
                },
                canonical_result: SnapshotResultObservation {
                    job_id: "55".repeat(32),
                    digest: local_digest.clone(),
                },
                canonical_result_at: block(14),
            }),
            before_restart: Some(SnapshotNativeObservation {
                progress: native(15),
                sources: vec!["/fixture/receiver/db".into()],
                observed: 21,
            }),
            k_canonical: Some(block(15)),
            first_exit: Some(SnapshotExitObservation {
                pid: 1001,
                reaped: 20,
                code: Some(0),
                signal: None,
            }),
            second_start: Some(launch(23, 1002, native(15))),
        }
    }
    #[test]
    fn accepts_distinct_native_frontiers_and_unvalidated_start_records() {
        let evidence = fixture();
        assert!(assert_snapshot_workflow(&evidence).is_ok());
        let decoded: SnapshotManifestObservation =
            serde_json::from_slice(&evidence.manifest_bytes).unwrap();
        assert_ne!(decoded.progress.execution, decoded.progress.finalized);
        assert_ne!(decoded.progress.ce, decoded.progress.projection);
        assert_eq!(decoded.progress.partial_state_trie, Some(10));
    }
    #[test]
    fn rejects_missing_actual_cli_and_nonzero_exit() {
        let mut e = fixture();
        e.create = None;
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.create.as_mut().unwrap().exit_code = Some(1);
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.transfer = None;
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.placement = None;
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn rejects_wrong_cut_height_hash_or_rpc_only_resume() {
        let mut e = fixture();
        e.cut_canonical.number += 1;
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.cut_canonical.hash = "ab".repeat(32);
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.first_start.as_mut().unwrap().before_launch.progress = native(1);
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn rejects_no_post_placement_progress_or_changed_current_k_hash() {
        let mut e = fixture();
        e.before_restart.as_mut().unwrap().progress = native(10);
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.k_canonical.as_mut().unwrap().hash = "ab".repeat(32);
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn rejects_changed_own_identity_at_every_boundary() {
        for boundary in 0..3 {
            let mut e = fixture();
            let fingerprints = match boundary {
                0 => &mut e.identity_placed,
                1 => &mut e.identity_at_k,
                _ => &mut e.identity_restarted,
            };
            fingerprints.values_mut().next().unwrap().sha256 = "ff".repeat(32);
            assert!(assert_snapshot_workflow(&e).is_err());
        }
    }
    #[test]
    fn rejects_copied_job_relabeling_and_pre_start_work() {
        let mut e = fixture();
        let old = e.copied_result.job_id.clone();
        let new = e.new_job.as_mut().unwrap();
        new.job_id = old.clone();
        new.local_result.job_id = old.clone();
        new.canonical_result.job_id = old;
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.new_job.as_mut().unwrap().requested = 7;
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.new_job.as_mut().unwrap().worker.after.body = metrics(0, 0);
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn rejects_missing_restart_reap_or_old_h_resume() {
        let mut e = fixture();
        e.second_start = None;
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.first_exit = None;
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.second_start.as_mut().unwrap().before_launch.progress = native(10);
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    fn report(q: SnapshotBlock, p: SnapshotBlock, bodies: &str) -> Vec<u8> {
        let mut checks = serde_json::Map::new();
        for name in [
            "files",
            "provenance",
            "headers",
            "evm",
            "ce",
            "bodies",
            "ocomp",
        ] {
            checks.insert(name.into(), serde_json::json!({"selected":name == "bodies", "status":if name == "bodies" { bodies } else { "not_requested" }, "diagnostic":null}));
        }
        serde_json::to_vec(&serde_json::json!({"checks":checks,"observed":{"q":q,"p":p},"provenance":{},"required_missing":[],"retained_ranges":[],"inventory_bounds":[],"active_ocomp":[]})).unwrap()
    }
    #[test]
    fn preserves_actual_incomplete_report_and_rejects_false_body_equality() {
        assert!(
            parse_snapshot_validation_report(&report(block(9), block(10), "incomplete")).is_ok()
        );
        assert!(parse_snapshot_validation_report(&report(block(9), block(10), "passed")).is_err());
        let mut fork = block(10);
        fork.hash = "ef".repeat(32);
        assert!(parse_snapshot_validation_report(&report(block(10), fork, "passed")).is_err());
        assert!(parse_snapshot_validation_report(&report(block(10), block(10), "passed")).is_ok());
    }
    #[test]
    fn rejects_unknown_status_and_missing_check_in_actual_json_shape() {
        assert!(
            parse_snapshot_validation_report(&report(block(10), block(10), "success")).is_err()
        );
        let mut json: serde_json::Value =
            serde_json::from_slice(&report(block(10), block(10), "passed")).unwrap();
        json["checks"].as_object_mut().unwrap().remove("ce");
        assert!(parse_snapshot_validation_report(&serde_json::to_vec(&json).unwrap()).is_err());
    }
    #[test]
    fn optional_audit_keeps_incomplete_exit_and_cannot_be_backfilled_after_start() {
        let mut e = fixture();
        let mut validation = command("validate", 6);
        validation.stdout = report(block(9), block(10), "incomplete");
        validation.exit_code = Some(1);
        e.validation = SnapshotValidationObservation::Run(validation.clone());
        assert!(assert_snapshot_workflow(&e).is_ok());
        validation.exit_code = Some(0);
        e.validation = SnapshotValidationObservation::Run(validation.clone());
        assert!(assert_snapshot_workflow(&e).is_err());
        validation.exit_code = Some(1);
        validation.started = 9;
        validation.ended = 10;
        e.validation = SnapshotValidationObservation::Run(validation);
        assert!(assert_snapshot_workflow(&e).is_err());
    }

    #[test]
    fn digest_inequality_does_not_replace_new_job_execution_evidence() {
        let mut e = fixture();
        // Keep the new job's canonical bytes and digest bound. The copied result
        // observation is not used as a substitute for actual worker evidence.
        e.copied_result.digest = e.new_job.as_ref().unwrap().local_result.digest.clone();
        assert!(assert_snapshot_workflow(&e).is_ok());
    }
    #[test]
    fn native_read_is_prelaunch_and_anchor_may_follow_a_legitimate_tail() {
        let e = fixture();
        let launch = e.first_start.as_ref().unwrap();
        assert!(launch.before_launch.observed < launch.started);
        assert_ne!(
            launch.recovery.anchor,
            launch.before_launch.progress.finalized
        );
        assert_ne!(
            launch.recovery.last_execution_height,
            launch.before_launch.progress.execution.number
        );
        assert!(assert_snapshot_workflow(&e).is_ok());
        let mut e = fixture();
        e.first_start.as_mut().unwrap().before_launch.observed = 9;
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn rejects_missing_wrong_or_old_incarnation_recovery_record() {
        let mut e = fixture();
        e.first_start.as_mut().unwrap().recovery.log.bytes.clear();
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.first_start.as_mut().unwrap().recovery.pid += 1;
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.first_start.as_mut().unwrap().recovery.incarnation_started -= 1;
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.first_start.as_mut().unwrap().recovery.canonical.hash = "ee".repeat(32);
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn empty_worker_log_is_valid_with_sampled_status_native_artifact_and_counters() {
        let e = fixture();
        assert!(e
            .new_job
            .as_ref()
            .unwrap()
            .worker
            .log
            .as_ref()
            .unwrap()
            .bytes
            .is_empty());
        assert!(assert_snapshot_workflow(&e).is_ok());
    }
    #[test]
    fn rejects_wrong_unit_path_tampered_artifact_or_no_success_counter() {
        let mut e = fixture();
        let worker = &mut e.new_job.as_mut().unwrap().worker;
        worker.artifact_after.path = worker.owner.inbox_root.join("artifacts/wrong-unit.ocb1");
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.new_job
            .as_mut()
            .unwrap()
            .worker
            .artifact_after
            .bytes
            .as_mut()
            .unwrap()
            .push(0);
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.new_job.as_mut().unwrap().worker.after.body = metrics(0, 0);
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn unique_owned_producer_requires_one_complete_matching_incarnation() {
        let mut e = fixture();
        assert!(assert_snapshot_workflow(&e).is_ok());
        let worker = &mut e.new_job.as_mut().unwrap().worker;
        let mut other = worker.owner.clone();
        other.process.pid += 1;
        other.process.worker_ordinal = Some(1);
        {
            let SnapshotWorkerAttribution::SingleOwnedProducer { workers, .. } =
                &mut worker.attribution;
            workers.push(other);
        }
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        let SnapshotWorkerAttribution::SingleOwnedProducer { workers, .. } =
            &mut e.new_job.as_mut().unwrap().worker.attribution;
        workers.clear();
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        let SnapshotWorkerAttribution::SingleOwnedProducer { workers, .. } =
            &mut e.new_job.as_mut().unwrap().worker.attribution;
        workers[0].process.pid += 1;
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn rejects_counter_from_restarted_worker_and_preexisting_new_job_artifact() {
        let mut e = fixture();
        e.new_job
            .as_mut()
            .unwrap()
            .worker
            .after
            .owner
            .process
            .started_at_millis += 1;
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        {
            let worker = &mut e.new_job.as_mut().unwrap().worker;
            let SnapshotPriorFileObservation::DirectoryListing(listing) =
                &mut worker.artifact_before;
            listing.entries.push(
                worker
                    .artifact_after
                    .path
                    .strip_prefix(&listing.root)
                    .unwrap()
                    .to_path_buf(),
            );
        }
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        {
            let new = e.new_job.as_mut().unwrap();
            let SnapshotPriorFileObservation::DirectoryListing(listing) = &mut new.local_before;
            listing.entries.push(
                new.local_after
                    .path
                    .strip_prefix(&listing.root)
                    .unwrap()
                    .to_path_buf(),
            );
        }
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn job_must_follow_barrier_observation_and_second_native_read_is_independent() {
        let mut e = fixture();
        e.new_job.as_mut().unwrap().requested = 9;
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.second_start.as_mut().unwrap().before_launch.progress.ce = block(1);
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn transfer_tampering_still_fails() {
        let mut e = fixture();
        e.transferred_archive_sha256 = "ff".repeat(32);
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn unknown_future_unit_path_can_be_proven_absent_by_prior_complete_listing() {
        let e = fixture();
        assert!(assert_snapshot_workflow(&e).is_ok());
        let mut e = fixture();
        let new = e.new_job.as_mut().unwrap();
        {
            let SnapshotPriorFileObservation::DirectoryListing(listing) =
                &mut new.worker.artifact_before;
            listing.entries.push(
                new.worker
                    .artifact_after
                    .path
                    .strip_prefix(&listing.root)
                    .unwrap()
                    .to_path_buf(),
            );
        }
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn rejects_listing_after_request_wrong_root_and_incomplete_producer_interval() {
        let mut e = fixture();
        {
            let SnapshotPriorFileObservation::DirectoryListing(listing) =
                &mut e.new_job.as_mut().unwrap().worker.artifact_before;
            listing.completed = 13;
        }
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        {
            let SnapshotPriorFileObservation::DirectoryListing(listing) =
                &mut e.new_job.as_mut().unwrap().worker.artifact_before;
            listing.root = "/unrelated".into();
        }
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        let worker = &mut e.new_job.as_mut().unwrap().worker;
        worker.attribution = SnapshotWorkerAttribution::SingleOwnedProducer {
            inventory_from: 10,
            inventory_through: 18,
            workers: vec![worker.owner.clone()],
        };
        assert!(assert_snapshot_workflow(&e).is_err());
    }
    #[test]
    fn serialized_observations_in_the_same_millisecond_are_valid() {
        let mut e = fixture();
        e.create.as_mut().unwrap().ended = 1;
        e.transfer.as_mut().unwrap().ended = 3;
        e.first_start.as_mut().unwrap().before_launch.observed = 8;
        let new = e.new_job.as_mut().unwrap();
        new.worker.before.observed = new.requested;
        {
            let SnapshotPriorFileObservation::DirectoryListing(listing) =
                &mut new.worker.artifact_before;
            listing.completed = new.requested;
        }
        {
            let SnapshotPriorFileObservation::DirectoryListing(listing) = &mut new.local_before;
            listing.completed = new.requested;
        }
        assert!(assert_snapshot_workflow(&e).is_ok());
    }
    #[test]
    fn pending_cut_accepts_lysis_at_execution_after_finalized_height() {
        let mut progress = fixture().placement.unwrap().native.progress;
        progress.finalized.number = 105;
        progress.execution.number = 106;
        let mut queried = Vec::new();
        let observed = snapshot_pending_cut(&progress, |at| {
            queried.push(at.number);
            ensure!(at.number == 106, "Lysis has not activated at H=105");
            Ok(serde_json::json!({"block_number":106,"queue_sequence":1}))
        })
        .expect("copied E contains the completed Lysis even when H precedes it");
        assert_eq!(queried, vec![106]);
        assert_eq!(observed["execution"]["block_number"], 106);
        assert_eq!(observed["finalized_block"]["number"], 105);
        assert!(snapshot_pending_cut(&progress, |_| Err(eyre!("no pending work at E"))).is_err());
    }
    #[test]
    fn local_result_requires_exact_job_filename_and_canonical_bytes() {
        let mut e = fixture();
        e.new_job.as_mut().unwrap().local_after.path = "/fixture/new-job/unrelated.ocb1".into();
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.new_job.as_mut().unwrap().local_after.bytes = Some(vec![1]);
        assert!(assert_snapshot_workflow(&e).is_err());
        let mut e = fixture();
        e.new_job.as_mut().unwrap().local_result.digest = "ff".repeat(32);
        e.new_job.as_mut().unwrap().canonical_result.digest = "ff".repeat(32);
        assert!(assert_snapshot_workflow(&e).is_err());
    }
}

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

fn snapshot_now_millis() -> eyre::Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

/// Invoke the actual CLI, retaining outputs even when validation exits nonzero.
/// Timeout kills/reaps this owned child; it cannot leave a CLI writer behind.
fn run_snapshot_command(
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

fn parse_recovery_record(log: &str) -> eyre::Result<(u64, SnapshotBlock, u64, u64)> {
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

fn observe_snapshot_directory(
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
fn completed_snapshot_workflow(world: &mut crate::world::World) {
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
fn observe_stopped_native(
    node_dir: &std::path::Path,
    projection_config_path: &std::path::Path,
    chain_id: u64,
    genesis_hash: alloy_primitives::B256,
) -> eyre::Result<crate::world::state::SnapshotNativeObservation> {
    use crate::world::state::{
        SnapshotBlock, SnapshotNativeObservation, SnapshotNativeProgress, SnapshotUnwind,
    };
    use alloy_consensus::Sealable;
    use outbe_compressed_entities::{
        CeMdbxReadOnly, CeTopologyV1, EnvironmentIdentity, ACTIVE_COMMITMENT_SCHEME,
        LOCAL_STORAGE_SCHEMA_VERSION,
    };
    use outbe_offchain_data::{read_projection_state, ProjectionConfig};
    use outbe_offchain_storage::{RocksDbReader, StorageBackend};
    use outbe_primitives::{projection::ProjectionCheckpoint, OutbeHeader, OutbePrimitives};
    use reth_ethereum::provider::db::{
        database::Database,
        mdbx::DatabaseArguments,
        models::PartialStateTrieUnwindMarker,
        open_db_read_only,
        tables::{self, ChainStateKey},
        transaction::DbTx,
    };
    use reth_provider::{
        providers::StaticFileProvider, BlockHashReader, HeaderProvider, StorageSettings,
    };
    use std::sync::Arc;

    // These paths come from the harness's ordinary node and projection config,
    // independently of the received manifest's donor paths and expected values.
    let node = node_dir.canonicalize()?;
    let chain = node.join("data");
    let static_path = chain.join("static_files");
    let closure_path = node.join("ocomp/domain-v1/exporter-v1/discovery/closure-checkpoint-v1");
    let projection = outbe_offchain_storage::StorageConfig::load(projection_config_path)?;
    let StorageBackend::RocksDb(rocks) = &projection.backend else {
        eyre::bail!("snapshot E2E requires its configured RocksDB projection");
    };
    eyre::ensure!(
        projection.start_block == 1
            && rocks.path == node.join("data/offchain")
            && rocks.secondary_path == node.join("ocomp/rocksdb-secondary"),
        "recipient projection config differs from its ordinary storage identity"
    );
    eyre::ensure!(static_path.is_dir(), "missing placed static files");
    let db = open_db_read_only(chain.join("db"), DatabaseArguments::default())?;
    let files = StaticFileProvider::<OutbePrimitives>::read_only(&static_path)?;
    let tx = db.tx()?;
    let identity = |number| -> eyre::Result<SnapshotBlock> {
        let header = match tx.get::<tables::Headers<OutbeHeader>>(number)? {
            Some(value) => Some(value),
            None => files.header_by_number(number)?,
        }
        .ok_or_else(|| eyre::eyre!("missing native header {number}"))?;
        let hash = match tx.get::<tables::CanonicalHeaders>(number)? {
            Some(value) => Some(value),
            None => files.block_hash(number)?,
        }
        .ok_or_else(|| eyre::eyre!("missing native canonical identity {number}"))?;
        eyre::ensure!(
            header.inner.number == number && header.hash_slow() == hash,
            "native header identity mismatch at {number}"
        );
        Ok(SnapshotBlock {
            number,
            hash: hex::encode(hash),
        })
    };
    eyre::ensure!(
        identity(0)?.hash == hex::encode(genesis_hash),
        "wrong placed genesis"
    );
    let finalized = identity(
        tx.get::<tables::ChainState>(ChainStateKey::LastFinalizedBlock)?
            .ok_or_else(|| eyre::eyre!("missing native LastFinalizedBlock"))?,
    )?;
    let execution = tx
        .get::<tables::StageCheckpoints>("Execution".into())?
        .ok_or_else(|| eyre::eyre!("missing native Execution checkpoint"))?;
    let execution_identity = identity(execution.block_number)?;
    let finish = tx.get::<tables::StageCheckpoints>("Finish".into())?;
    let storage_version = match tx.get::<tables::Metadata>("storage_settings".into())? {
        Some(raw) if serde_json::from_slice::<StorageSettings>(&raw)?.is_v2() => 2,
        _ => 1,
    };
    let unwind = tx
        .get::<tables::Metadata>("partial_state_trie_unwind".into())?
        .map(|raw| serde_json::from_slice::<PartialStateTrieUnwindMarker>(&raw))
        .transpose()?
        .map(|value| SnapshotUnwind {
            finish_block_number: value.finish_block_number,
            partial_state_trie: value.partial_state_trie,
        });
    tx.commit()?;
    drop(files);
    drop(db);

    let ce = CeMdbxReadOnly::open(
        &chain,
        EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id,
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
        },
    )?
    .marker()?;
    // Never share the live exporter's secondary directory or write deployment data.
    let secondary = tempfile::tempdir()?;
    let scratch = secondary.path().canonicalize()?;
    eyre::ensure!(
        !scratch.starts_with(&node) && !node.starts_with(&scratch),
        "native inspection scratch overlaps the node directory"
    );
    let projection_marker = read_projection_state(
        ProjectionConfig {
            chain_id,
            genesis_hash,
            start_block: projection.start_block,
        },
        Arc::new(RocksDbReader::open(&rocks.path, secondary.path())?),
    )?
    .and_then(|state| state.checkpoint)
    .ok_or_else(|| eyre::eyre!("missing placed projection checkpoint"))?;
    let closure = outbe_ocomp::discovery_spool::inspect_closure_checkpoint(
        &closure_path,
        ProjectionCheckpoint {
            block_number: 0,
            block_hash: genesis_hash,
        },
    )?;
    let block = |value: ProjectionCheckpoint| SnapshotBlock {
        number: value.block_number,
        hash: hex::encode(value.block_hash),
    };
    let progress = SnapshotNativeProgress {
        finalized,
        execution: execution_identity,
        execution_stage: Some(execution.block_number),
        finish_stage: finish.as_ref().map(|value| value.block_number),
        partial_state_trie: finish
            .as_ref()
            .and_then(|value| value.finish_stage_checkpoint())
            .and_then(|value| value.partial_state_trie),
        unwind,
        storage_version,
        ce: SnapshotBlock {
            number: ce.height,
            hash: hex::encode(ce.block_hash),
        },
        projection: block(projection_marker),
        ocomp_baseline: block(closure.baseline),
        ocomp_previous: block(closure.previous),
        ocomp_current: block(closure.current),
    };
    Ok(SnapshotNativeObservation {
        progress,
        sources: vec![
            chain.join("db"),
            static_path,
            chain.join("compressed_entities/smt"),
            rocks.path.clone(),
            closure_path,
        ],
        observed: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
            .try_into()?,
    })
}

fn snapshot_option(argv: &[String], flag: &str) -> eyre::Result<String> {
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

fn canonical_snapshot_block(
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

fn snapshot_file_sha256(path: &std::path::Path) -> eyre::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut input = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut bytes)?;
        if read == 0 {
            break;
        }
        hash.update(&bytes[..read]);
    }
    Ok(hex::encode(hash.finalize()))
}

#[cucumber::when("a stopped post-Lysis donor creates the signed native snapshot")]
fn create_stopped_snapshot(world: &mut crate::world::World) {
    create_stopped_snapshot_result(world)
        .expect("create signed snapshot from stopped native files");
}

fn create_stopped_snapshot_result(world: &mut crate::world::World) -> eyre::Result<()> {
    use crate::world::state::*;
    use std::process::Command;
    let index = 3;
    let node = world
        .validators
        .data_dir(index)
        .parent()
        .unwrap()
        .to_path_buf();
    let evidence_dir = world.localnet.scenario_dir().join("offline-snapshot");
    std::fs::create_dir_all(&evidence_dir)?;
    let launch = world.localnet.validator_launch_observation(index)?;
    ensure!(
        launch.argv.first().map(String::as_str) == Some("node"),
        "unexpected ordinary command"
    );
    let projection = snapshot_option(&launch.argv, "--projection.storage-config")?;
    let genesis = canonical_snapshot_block(world, 0)?
        .hash
        .parse::<alloy_primitives::B256>()?;
    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .ok_or_else(|| eyre!("chain id unavailable"))?;
    ensure!(
        chain_id == 54322345,
        "snapshot E2E requires the existing SGX testnet profile"
    );
    let price_publication = crate::features::price_oracle::stop_before_clock_restart(world);
    let clients = world
        .ocomp
        .stop_node_facing_roles_for_snapshot(index as u8)?;
    ensure!(clients.index() == index as u8, "wrong selected clients");
    let client_exits: Vec<_> = clients.stops().iter().map(|stop| serde_json::json!({
        "index":stop.index, "role":stop.role, "worker_ordinal":stop.worker_ordinal,
        "pid":stop.pid, "requested":stop.stop_requested_at_millis, "reaped":stop.reaped_at_millis,
        "code":stop.code,"signal":stop.signal,
    })).collect();
    let stopped = world
        .localnet
        .stop_validator_for_snapshot(index, launch.pid)?;
    let native =
        observe_stopped_native(&node, std::path::Path::new(&projection), chain_id, genesis)?;
    let generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .ok_or_else(|| eyre!("missing actual certified generation before stopped cut"))?;
    let pending = snapshot_pending_cut(&native.progress, |at| {
        snapshot_pending_materialization_at(world, at, generation)
    })?;
    std::fs::write(
        evidence_dir.join("pending-native-cut.json"),
        serde_json::to_vec_pretty(&pending)?,
    )?;
    let cut = canonical_snapshot_block(world, native.progress.finalized.number)?;
    ensure!(
        native.progress.finalized == cut,
        "stopped donor has noncanonical H"
    );
    let archive = evidence_dir.join("created.tar");
    let mut command = Command::new(&launch.program);
    command
        .args(["snapshot", "create", "--output"])
        .arg(&archive)
        .arg("--signing-key")
        .arg(world.validators.get(index).evm_key_path())
        .args([
            "--creator",
            "E2E donor validator 3",
            "--source",
            "stopped localnet donor",
            "--",
        ])
        .args(&launch.argv[1..]);
    let create = run_snapshot_command(
        command,
        &evidence_dir,
        "create",
        std::time::Duration::from_secs(600),
    )?;
    successful_command(&create)?;
    let archive_sha256 = snapshot_file_sha256(&archive)?;
    let mut command = Command::new("tar");
    command.arg("-xOf").arg(&archive).arg("manifest.json");
    let manifest = run_snapshot_command(
        command,
        &evidence_dir,
        "read-manifest",
        std::time::Duration::from_secs(60),
    )?;
    successful_command(&manifest)?;
    let recorded: SnapshotManifestObservation = serde_json::from_slice(&manifest.stdout)?;
    ensure!(
        recorded.progress == native.progress,
        "creation changed or misreported stopped native progress"
    );
    let copied_job = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .ok_or_else(|| eyre!("missing actual Lysis generation"))?
        .job_id;
    let copied_bytes = std::fs::read(super::local_result_path(world, index, copied_job))?;
    let copied_result = outbe_ocomp_protocol::result::LysisResultV1::decode_canonical(
        &copied_bytes,
        &outbe_ocomp_protocol::profile::poc_schema_limits(),
    )?;
    ensure!(
        copied_result.job_id == copied_job,
        "copied result belongs to another job"
    );
    let copied_result = SnapshotResultObservation {
        job_id: hex::encode(copied_job),
        digest: hex::encode(
            copied_result.result_digest(&outbe_ocomp_protocol::profile::poc_schema_limits())?,
        ),
    };
    std::fs::write(
        evidence_dir.join("donor-stop.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "node_pid": stopped.observation.launch.pid,
            "node_stop_requested": stopped.observation.stop_requested_at_millis,
            "node_reaped": stopped.observation.reaped_at_millis,
            "node_code": stopped.observation.code,"node_signal": stopped.observation.signal,
            "clients": client_exits,"native": native.progress,
        }))?,
    )?;
    world.state.offline_snapshot = Some(OfflineSnapshotEvidence {
        create: Some(create),
        manifest_bytes: manifest.stdout,
        archive_sha256,
        transferred_archive_sha256: String::new(),
        transfer: None,
        placement: None,
        validation: SnapshotValidationObservation::NotRun,
        cut_canonical: cut,
        identity_before: BTreeMap::new(),
        identity_placed: BTreeMap::new(),
        identity_at_k: BTreeMap::new(),
        identity_restarted: BTreeMap::new(),
        first_start: None,
        copied_result,
        new_job: None,
        before_restart: None,
        k_canonical: None,
        first_exit: None,
        second_start: None,
    });
    world.localnet.resume_snapshot_node(stopped)?;
    ensure!(
        world.rpc.wait_finalized_at_least(
            world.validators.http_port(index),
            native.progress.finalized.number + 1,
            180
        ),
        "resumed donor did not become ready"
    );
    world.ocomp.resume_snapshot_node_facing_roles(clients)?;
    // Bind a new Oracle cohort only after the restarted donor shares fresh finality.
    let ports = world.validators.committee_ports();
    let target = world.rpc.fresh_finality_target(&ports)?;
    world.rpc.wait_finalized_checkpoint(&ports, target, 60)?;
    if let Some(pending) =
        crate::features::price_oracle::resume_after_clock_restart(world, price_publication)
    {
        while !crate::features::price_oracle::observe_pending_publication(world, &pending) {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    Ok(())
}

#[cucumber::when("a fresh snapshot recipient provisions its own identity without syncing")]
fn provision_snapshot_recipient(world: &mut crate::world::World) {
    let index = world.validators.joiner_index();
    let data = world.validators.data_dir(index);
    for relative in [
        "db",
        "static_files",
        "compressed_entities",
        "offchain",
        "consensus",
    ] {
        assert!(
            !data.join(relative).exists(),
            "recipient already has native chain history"
        );
    }
    let node = data.parent().expect("recipient node root");
    assert!(
        !node.join("ocomp/domain-v1/node-v1").exists(),
        "recipient already has OCOMP history"
    );
    world
        .localnet
        .prepare_snapshot_full_node(index)
        .expect("provision own node-host and normal configuration");
    world
        .ocomp
        .stage_keyless_full_node_domain(index.try_into().unwrap())
        .expect("stage public FullNode deployment files");
    for key in ["evm-key.hex", "signing-key.hex", "reth-p2p-secret.hex"] {
        let donor = world.validators.data_dir(3).parent().unwrap().join(key);
        assert_ne!(
            snapshot_file_sha256(&node.join(key)).unwrap(),
            snapshot_file_sha256(&donor).unwrap(),
            "recipient must own a distinct {key}"
        );
    }
}

fn recipient_identity(
    world: &crate::world::World,
) -> eyre::Result<BTreeMap<std::path::PathBuf, crate::world::state::SnapshotFingerprint>> {
    use std::os::unix::fs::PermissionsExt;
    let data = world.validators.data_dir(world.validators.joiner_index());
    let node = data.parent().unwrap();
    let mut values = BTreeMap::new();
    for relative in [
        "evm-key.hex",
        "signing-key.hex",
        "reth-p2p-secret.hex",
        "offchain-storage.toml",
        "data/tee-node-host-v1/noise-initiator.key",
        "data/tee-node-host-v1/initialization-manifest.bin",
    ] {
        let path = node.join(relative);
        let metadata = std::fs::metadata(&path)?;
        values.insert(
            path.clone(),
            crate::world::state::SnapshotFingerprint {
                sha256: snapshot_file_sha256(&path)?,
                mode: metadata.permissions().mode() & 0o7777,
            },
        );
    }
    for relative in [
        "ocomp-key-v1.hex",
        "ocomp-evm-key.hex",
        "supervisor-v1/sign-once",
        "supervisor-v1/vote-submissions",
        "supervisor-v1/materialization-submissions",
        "supervisor-v1/payout-submissions",
    ] {
        ensure!(
            !node.join("ocomp/domain-v1").join(relative).exists(),
            "FullNode imported donor signing authority: {relative}"
        );
    }
    Ok(values)
}

fn place_snapshot_payload(
    world: &crate::world::World,
    archive: &std::path::Path,
    evidence_dir: &std::path::Path,
) -> eyre::Result<(Vec<u8>, Vec<u8>)> {
    use std::process::Command;
    let scratch = tempfile::tempdir()?;
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
    let node = data.parent().unwrap();
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
            "ocomp" => node.join("ocomp/domain-v1"),
            "offchain" => data.join("offchain"),
            "static-files" => data.join("static_files"),
            "execution-rocks-db" => data.join("rocksdb"),
            root => return Err(eyre!("unknown native root {root}")),
        };
        std::fs::create_dir_all(&target)?;
        let mut command = Command::new("cp");
        command
            .args(["-a", "--no-preserve=ownership", "--"])
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

fn observe_snapshot_launch(
    world: &mut crate::world::World,
    launch: crate::world::localnet::NodeLaunchObservation,
    before_launch: crate::world::state::SnapshotNativeObservation,
) -> eyre::Result<crate::world::state::SnapshotLaunchObservation> {
    use crate::world::state::*;
    use std::os::unix::fs::MetadataExt;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let (text, fields) = loop {
        let text = world.localnet.node_launch_log(launch.index, launch.pid)?;
        if text.contains("certified follower startup recovery barrier completed") {
            let fields = parse_recovery_record(&text)?;
            break (text, fields);
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "recipient did not finish native startup; see {}",
            launch.log_path.display()
        );
        world
            .localnet
            .follower_launch_observation("snapshot-recipient", launch.index)?;
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let (marshal_processed, anchor, ce_marker_height, last_execution_height) = fields;
    let canonical = canonical_snapshot_block(world, anchor.number)?;
    let metadata = std::fs::metadata(&launch.log_path)?;
    let log = SnapshotLogSlice {
        path: launch.log_path,
        device: metadata.dev(),
        inode: metadata.ino(),
        start: launch.log_start,
        end: launch.log_start + text.len() as u64,
        bytes: text.into_bytes(),
    };
    let mut argv = vec![launch.program.display().to_string()];
    argv.extend(launch.argv);
    let value = SnapshotLaunchObservation {
        slot: launch.index.try_into()?,
        started: launch.started_at_millis,
        pid: launch.pid,
        argv,
        before_launch,
        recovery: SnapshotRecoveryObservation {
            pid: launch.pid,
            incarnation_started: launch.started_at_millis,
            observed: snapshot_now_millis()?,
            log,
            marshal_processed,
            anchor,
            ce_marker_height,
            last_execution_height,
            canonical,
        },
    };
    ordinary_launch(&value)?;
    Ok(value)
}

fn start_snapshot_recipient(
    world: &mut crate::world::World,
    chain_id: u64,
    genesis: alloy_primitives::B256,
) -> eyre::Result<crate::world::state::SnapshotLaunchObservation> {
    let slot = world.validators.joiner_index();
    let data = world.validators.data_dir(slot);
    let node = data.parent().unwrap();
    let before =
        observe_stopped_native(node, &node.join("offchain-storage.toml"), chain_id, genesis)?;
    world
        .localnet
        .launch_dcap_full_node("snapshot-recipient", slot, 0)?;
    let launched = world
        .localnet
        .follower_launch_observation("snapshot-recipient", slot)?;
    let observation = observe_snapshot_launch(world, launched, before)?;
    let target = observation.recovery.anchor.number + 1;
    ensure!(
        world
            .rpc
            .wait_finalized_at_least(world.validators.http_port(slot), target, 180),
        "recipient did not advance beyond startup anchor"
    );
    world
        .ocomp
        .start_keyless_full_node_roles(slot.try_into()?)?;
    Ok(observation)
}

#[cucumber::when(
    "the signed files start fresh FullNode placements with and without offline validation"
)]
fn place_and_start_snapshot_recipient(world: &mut crate::world::World) {
    place_and_start_snapshot_recipient_result(world)
        .expect("ordinary signed-file placement and startup");
}

fn place_and_start_snapshot_recipient_result(world: &mut crate::world::World) -> eyre::Result<()> {
    use crate::world::state::*;
    use std::{process::Command, time::Duration};
    let slot = world.validators.joiner_index();
    let data = world.validators.data_dir(slot);
    let node = data.parent().unwrap().to_path_buf();
    let root = world.localnet.scenario_dir().join("offline-snapshot");
    let archive = root.join("created.tar");
    let retained_archive = root.join("next-placement.tar");
    let received = root.join("received.tar");
    let manifest_path = root.join("manifest.json");
    let signature_path = root.join("signature.json");
    let donor_launch = world.localnet.validator_launch_observation(3)?;
    let chain = snapshot_option(&donor_launch.argv, "--chain")?;
    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .ok_or_else(|| eyre!("chain id"))?;
    let genesis = canonical_snapshot_block(world, 0)?.hash.parse()?;
    let identity = recipient_identity(world)?;
    let initial_data_members = std::fs::read_dir(&data)?
        .map(|entry| entry.map(|v| v.file_name()))
        .collect::<std::io::Result<std::collections::BTreeSet<_>>>()?;
    let tee_root = data.join("tee-node-host-v1");
    let tee_before = fingerprint_snapshot_tree(&tee_root)?;
    // The source paths will be absent during startup; this is filesystem-path
    // independence, not OS isolation between processes using the same test UID.
    let price_publication = crate::features::price_oracle::stop_before_clock_restart(world);
    let clients = world.ocomp.stop_node_facing_roles_for_snapshot(3)?;
    let stopped = world
        .localnet
        .stop_validator_for_snapshot(3, donor_launch.pid)?;
    let donor_node = world.validators.data_dir(3).parent().unwrap().to_path_buf();
    let hidden = [
        (donor_node.join("data"), donor_node.join("data.offline-e2e")),
        (
            donor_node.join("ocomp/domain-v1"),
            donor_node.join("ocomp/domain-v1.offline-e2e"),
        ),
    ];
    hide_snapshot_sources(&hidden)?;
    let result = (|| -> eyre::Result<()> {
        for validate in [true, false] {
            let phase_dir = root.join(if validate {
                "validated-placement"
            } else {
                "unvalidated-placement"
            });
            std::fs::create_dir_all(&phase_dir)?;
            let source = if validate {
                &archive
            } else {
                &retained_archive
            };
            let mut command = Command::new("cp");
            command.arg("--").arg(source).arg(&received);
            let transfer =
                run_snapshot_command(command, &phase_dir, "transfer", Duration::from_secs(600))?;
            successful_command(&transfer)?;
            let transferred_archive_sha256 = snapshot_file_sha256(&received)?;
            let (manifest_bytes, signature_bytes) =
                place_snapshot_payload(world, &received, &phase_dir)?;
            let original = world
                .state
                .offline_snapshot
                .as_ref()
                .ok_or_else(|| eyre!("missing created artifact"))?;
            ensure!(
                manifest_bytes == original.manifest_bytes
                    && transferred_archive_sha256 == original.archive_sha256,
                "placement used a different artifact"
            );
            ensure!(
                recipient_identity(world)? == identity,
                "placement changed own identity"
            );
            if validate {
                ensure!(
                    fingerprint_snapshot_tree(&tee_root)? == tee_before,
                    "placement changed recipient NodeHost records"
                );
            }
            std::fs::write(&manifest_path, &manifest_bytes)?;
            std::fs::write(&signature_path, signature_bytes)?;
            let placed = observe_stopped_native(
                &node,
                &node.join("offchain-storage.toml"),
                chain_id,
                genesis,
            )?;
            let placement = SnapshotPlacementObservation {
                completed: snapshot_now_millis()?,
                native: placed,
            };
            let validation = if validate {
                let creator = std::str::from_utf8(&original.create.as_ref().unwrap().stdout)?
                    .split_whitespace()
                    .find_map(|field| field.strip_prefix("creator_public_key="))
                    .ok_or_else(|| eyre!("create omitted signer public key"))?;
                let mut command = Command::new(&donor_launch.program);
                command
                    .args(["snapshot", "validate", "--checks", "all", "--manifest"])
                    .arg(&manifest_path)
                    .arg("--signature")
                    .arg(&signature_path)
                    .arg("--expected-signer")
                    .arg(creator)
                    .arg("--report")
                    .arg(phase_dir.join("validation.json"))
                    .args(["--", "--chain"])
                    .arg(&chain)
                    .arg("--datadir")
                    .arg(&data)
                    .arg("--consensus.storage-dir")
                    .arg(data.join("consensus"))
                    .arg("--projection.storage-config")
                    .arg(node.join("offchain-storage.toml"));
                let observation = run_snapshot_command(
                    command,
                    &phase_dir,
                    "validate",
                    Duration::from_secs(900),
                )?;
                let report = parse_snapshot_validation_report(&observation.stdout)?;
                ensure!(
                    report.checks["files"].status == SnapshotCheckStatus::Passed
                        && report.checks["provenance"].status == SnapshotCheckStatus::Passed,
                    "original signed file checks did not pass"
                );
                ensure!(
                    !report
                        .checks
                        .values()
                        .any(|check| check.status == SnapshotCheckStatus::Failed),
                    "semantic validation failed; inspect retained actual report"
                );
                // Incomplete remains a nonzero audit, never relabeled complete.
                let complete = report
                    .checks
                    .values()
                    .all(|check| check.status == SnapshotCheckStatus::Passed);
                ensure!(
                    (complete && observation.exit_code == Some(0))
                        || (!complete && observation.exit_code.is_some_and(|code| code != 0)),
                    "validation report/exit disagree"
                );
                snapshot_rejects_damaged_artifact(
                    &donor_launch.program,
                    &received,
                    &manifest_path,
                    &signature_path,
                    &phase_dir,
                    creator,
                    &chain,
                    &node,
                )?;
                SnapshotValidationObservation::Run(observation)
            } else {
                SnapshotValidationObservation::NotRun
            };
            std::fs::remove_file(&received)?;
            std::fs::remove_file(&manifest_path)?;
            std::fs::remove_file(&signature_path)?;
            if validate {
                std::fs::rename(&archive, &retained_archive)?;
            } else {
                std::fs::remove_file(&retained_archive)?;
            }
            ensure!(
                !archive.exists()
                    && !received.exists()
                    && !manifest_path.exists()
                    && !signature_path.exists(),
                "startup still has original artifact paths"
            );
            ensure!(
                hidden.iter().all(|(original, _)| !original.exists()),
                "donor source paths remain available"
            );
            if !validate {
                let bundle = world
                    .ocomp
                    .canonical_fork_install()?
                    .request_profile
                    .protocol_bundle_hash;
                let port = world.ocomp.snapshot_worker_port(slot);
                world.state.offline_snapshot_worker_inventory = Some(snapshot_worker_inventory(
                    &world.ocomp,
                    &node.join("ocomp/domain-v1"),
                    port,
                    bundle,
                )?);
            }
            let started = start_snapshot_recipient(world, chain_id, genesis)?;
            ensure!(
                started.before_launch.progress == placement.native.progress,
                "startup did not open placed native state"
            );
            if validate {
                let follower_clients = world
                    .ocomp
                    .stop_node_facing_roles_for_snapshot(slot.try_into()?)?;
                let exited = world.localnet.stop_follower_for_snapshot(
                    "snapshot-recipient",
                    slot,
                    started.pid,
                )?;
                std::fs::write(
                    phase_dir.join("ordinary-start.json"),
                    serde_json::to_vec_pretty(&serde_json::json!({
                        "placement":placement, "validation":validation, "launch":started,
                        "stop_pid":exited.observation.launch.pid,"stop_code":exited.observation.code,
                        "stop_signal":exited.observation.signal,"stop_at":exited.observation.reaped_at_millis,
                        "protected_identity":recipient_identity(world)?,
                    }))?,
                )?;
                drop(follower_clients);
                // A separate fresh placement, with the same recipient-owned identity.
                // Only this stopped, scenario-owned temporary node's copied data is discarded.
                for entry in std::fs::read_dir(&data)? {
                    let entry = entry?;
                    if initial_data_members.contains(&entry.file_name()) {
                        continue;
                    }
                    if entry.file_type()?.is_dir() {
                        std::fs::remove_dir_all(entry.path())?;
                    } else {
                        std::fs::remove_file(entry.path())?;
                    }
                }
                std::fs::remove_dir_all(node.join("ocomp/domain-v1"))?;
                std::fs::create_dir_all(node.join("ocomp/domain-v1"))?;
            } else {
                let e = world.state.offline_snapshot.as_mut().unwrap();
                e.transfer = Some(transfer);
                e.transferred_archive_sha256 = transferred_archive_sha256;
                e.placement = Some(placement);
                e.validation = validation;
                e.identity_before = identity.clone();
                e.identity_placed = identity.clone();
                e.first_start = Some(started);
            }
        }
        Ok(())
    })();
    // Restore the donor's paths regardless of the recipient assertion result.
    restore_snapshot_sources(&hidden)?;
    world.localnet.resume_snapshot_node(stopped)?;
    ensure!(
        world
            .rpc
            .wait_finalized_at_least(world.validators.http_port(3), 1, 180),
        "donor restart readiness"
    );
    world.ocomp.resume_snapshot_node_facing_roles(clients)?;
    // Bind a new Oracle cohort only after the restarted donor shares fresh finality.
    let ports = world.validators.committee_ports();
    let target = world.rpc.fresh_finality_target(&ports)?;
    world.rpc.wait_finalized_checkpoint(&ports, target, 60)?;
    if let Some(pending) =
        crate::features::price_oracle::resume_after_clock_restart(world, price_publication)
    {
        while !crate::features::price_oracle::observe_pending_publication(world, &pending) {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    result
}

fn fingerprint_snapshot_tree(
    root: &std::path::Path,
) -> eyre::Result<BTreeMap<std::path::PathBuf, crate::world::state::SnapshotFingerprint>> {
    use std::os::unix::fs::PermissionsExt;
    let mut result = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if entry.file_type().is_file() {
            result.insert(
                entry.path().strip_prefix(root)?.to_owned(),
                crate::world::state::SnapshotFingerprint {
                    sha256: snapshot_file_sha256(entry.path())?,
                    mode: entry.metadata()?.permissions().mode() & 0o7777,
                },
            );
        }
    }
    Ok(result)
}

// Read pending work at the copied execution frontier E. Finalized H may precede
// the Lysis activation and is recorded independently, without a pending-head gate.
fn snapshot_pending_materialization_at(
    world: &crate::world::World,
    at: &crate::world::state::SnapshotBlock,
    expected: &crate::world::rpc::OcompCertifiedGenerationV1,
) -> eyre::Result<serde_json::Value> {
    use crate::internal::{addresses, eth};
    use outbe_ocomp_protocol::{
        nod_materialization::NodMaterializationHeadV1, profile::poc_schema_limits,
    };
    eyre::ensure!(
        canonical_snapshot_block(world, at.number)? == *at,
        "native height differs from canonical RPC identity"
    );
    let returned = eth::read_call_at_result(
        &world.rpc.url(world.validators.primary_port()),
        addresses::NOD_FACTORY_ADDR,
        &eth::INodFactory::materializationHeadCall {},
        at.number,
    )
    .map_err(|error| eyre::eyre!(error))?;
    eyre::ensure!(
        returned.exists,
        "no pending materialization head at native height {}",
        at.number
    );
    let raw = returned.canonicalHead;
    let head = NodMaterializationHeadV1::decode_canonical(raw.as_ref(), &poc_schema_limits())?;
    eyre::ensure!(
        head.job_id == expected.job_id
            && head.program_semantics_hash == expected.program_semantics_hash
            && head.worldwide_day == expected.worldwide_day
            && head.generation == expected.generation
            && head.nod_root == expected.nod_root
            && head.nod_count == expected.nod_count,
        "native-height materialization head is not the actual certified generation"
    );
    eyre::ensure!(
        head.next_nod_ordinal < head.nod_count,
        "actual certified generation already completed at native height {}",
        at.number
    );
    eyre::ensure!(
        canonical_snapshot_block(world, at.number)? == *at,
        "canonical identity changed during historical materialization read"
    );
    Ok(serde_json::json!({
        "observed": snapshot_now_millis()?,
        "block_number": at.number,
        "block_hash": at.hash,
        "canonical_head": hex::encode(raw),
        "queue_sequence": head.queue_sequence,
        "job_id": hex::encode(head.job_id),
        "program_semantics_hash": hex::encode(head.program_semantics_hash),
        "worldwide_day": head.worldwide_day,
        "generation": head.generation,
        "nod_root": hex::encode(head.nod_root),
        "nod_count": head.nod_count,
        "next_nod_ordinal": head.next_nod_ordinal,
        "last_progress_height": head.last_progress_height,
    }))
}

#[cfg(test)]
mod worker_collector_tests {
    use super::*;
    use crate::world::ocomp::{OcompProcessRecordV1, OcompProcessRole};

    fn record(pid: u32, start: u64) -> OcompProcessRecordV1 {
        OcompProcessRecordV1 {
            validator_index: Some(4),
            role: OcompProcessRole::Worker,
            worker_ordinal: Some(0),
            pid,
            started_at_millis: start,
            stopped_at_millis: None,
        }
    }

    #[test]
    fn worker_history_cannot_drop_prior_incarnations_or_hide_another_ordinal() {
        let mut old = record(10, 1);
        old.stopped_at_millis = Some(2);
        let live = record(11, 10);
        assert!(snapshot_require_history(&[old.clone()], &[old.clone(), live.clone()]).is_ok());
        assert!(
            snapshot_require_history(std::slice::from_ref(&old), std::slice::from_ref(&live))
                .is_err()
        );
        let mut other = live.clone();
        other.worker_ordinal = Some(1);
        assert!(snapshot_v1_worker_records(&[live.clone(), other]).is_err());
        assert!(snapshot_unique_worker(&[live.clone(), record(12, 11)]).is_err());
    }

    #[test]
    fn bounded_file_observation_retains_not_found_and_rejects_oversize() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("artifact");
        assert!(snapshot_read_file(&path, 3).unwrap().bytes.is_none());
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(snapshot_read_file(&path, 3).unwrap().bytes.unwrap(), b"abc");
        std::fs::write(&path, b"abcd").unwrap();
        assert!(snapshot_read_file(&path, 3).is_err());
    }

    #[test]
    fn raw_http_observation_preserves_body_and_rejects_status_and_truncation() {
        let body = b"unit_counter 7\n";
        let raw = [
            b"HTTP/1.0 200 OK\r\nContent-Length: 15\r\n\r\n".as_slice(),
            body,
        ]
        .concat();
        // The exact body is 15 bytes; changing a byte must not change framing.
        assert_eq!(snapshot_http_body(&raw).unwrap(), body);
        assert!(snapshot_http_body(b"HTTP/1.0 503 unavailable\r\n\r\nno").is_err());
        assert!(snapshot_http_body(b"HTTP/1.0 200 OK\r\nContent-Length: 4\r\n\r\na").is_err());
    }
    #[test]
    fn worker_http_reads_actual_bounded_socket_response() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            assert!(request.starts_with(b"GET /metrics HTTP/1.0\r\n"));
            socket
                .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 3\r\n\r\nraw")
                .unwrap();
        });
        let observed = snapshot_http_get(address, "/metrics");
        server.join().unwrap();
        assert_eq!(observed.unwrap(), b"raw");
    }
}

// Append inside features/ocomp/offline_snapshot.rs after the existing helpers.
// This fragment owns no launches, requests, mutations or background polling.
// Scope: recipient slot 4, worker ordinal 0, one installed V1 bundle lane.
use crate::world::state::{
    SnapshotDirectoryListing, SnapshotResultObservation, SnapshotWorkerExecution,
    SnapshotWorkerHttpObservation,
};

const SNAPSHOT_RECIPIENT_SLOT: u8 = 4;
const SNAPSHOT_OBJECT_BYTES: u64 = 1_048_576; // Current runtime CAS object ceiling.

#[derive(Clone, Debug)]
pub(crate) struct SnapshotWorkerBeforeRequest {
    domain_root: std::path::PathBuf,
    address: std::net::SocketAddr,
    bundle: outbe_ocomp::bundle::PinnedProtocolBundle,
    artifact_inventory: SnapshotDirectoryListing,
    local_inventory: SnapshotDirectoryListing,
    history: Vec<crate::world::ocomp::OcompProcessRecordV1>,
    history_from: u64,
}

#[derive(Clone)]
struct SnapshotWorkerRunning {
    before_request: SnapshotWorkerBeforeRequest,
    before: SnapshotWorkerHttpObservation,
    before_status: SnapshotWorkerHttpObservation,
}

struct SnapshotWorkerLiveResult {
    running: SnapshotWorkerRunning,
    after: SnapshotWorkerHttpObservation,
    after_status: SnapshotWorkerHttpObservation,
    artifact: SnapshotFileRead,
    local: SnapshotFileRead,
    local_result: SnapshotResultObservation,
    job_id: alloy_primitives::B256,
    history: Vec<crate::world::ocomp::OcompProcessRecordV1>,
    history_through: u64,
}

// Retain these alongside the final existing evidence type, for raw diagnostics.
struct SnapshotCollectedWorker {
    new_job: SnapshotNewJobObservation,
    before_status: SnapshotWorkerHttpObservation,
    after_status: SnapshotWorkerHttpObservation,
    admission_record: SnapshotFileRead,
}

/// Call with the actual recipient domain root while root-owned writers are stopped.
/// No future job or unit ID is required. Port is Config::ocomp_worker_port(4, 0).
fn snapshot_worker_inventory(
    topology: &crate::world::ocomp::OcompTopology,
    domain_root: &std::path::Path,
    worker_port: u16,
    expected_bundle: alloy_primitives::B256,
) -> eyre::Result<SnapshotWorkerBeforeRequest> {
    let history_from = snapshot_now_millis()?;
    let history = snapshot_v1_worker_records(topology.process_records())?;
    ensure!(
        history.iter().all(|p| p.stopped_at_millis.is_some()),
        "recipient worker inventory requires stopped writers"
    );
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let raw = snapshot_read_file(
        &domain_root
            .join("protocol-bundles-v1")
            .join(format!("{}.ocb1", hex::encode(expected_bundle))),
        SNAPSHOT_OBJECT_BYTES,
    )?;
    let bundle = outbe_ocomp::bundle::PinnedProtocolBundle::decode(
        raw.bytes
            .as_deref()
            .ok_or_else(|| eyre!("missing installed V1 bundle"))?,
        expected_bundle,
        &limits,
    )?;
    let inbox = domain_root
        .join("worker-inbox-v1")
        .join(hex::encode(bundle.hash()));
    let artifact_inventory = observe_snapshot_directory(&inbox.join("artifacts"))?;
    let local_inventory = observe_snapshot_directory(&domain_root.join("node-v1/local-results"))?;
    Ok(SnapshotWorkerBeforeRequest {
        domain_root: domain_root.to_path_buf(),
        address: ([127, 0, 0, 1], worker_port).into(),
        bundle,
        artifact_inventory,
        local_inventory,
        history,
        history_from,
    })
}

/// Call after ordinary worker launch and before root requests the next-day job.
fn snapshot_worker_before(
    topology: &mut crate::world::ocomp::OcompTopology,
    inventory: SnapshotWorkerBeforeRequest,
) -> eyre::Result<SnapshotWorkerRunning> {
    let history = snapshot_v1_worker_records(topology.process_records())?;
    snapshot_require_history(&inventory.history, &history)?;
    let process = snapshot_unique_worker(&history)?;
    let owner = snapshot_owner(&inventory, process);
    let before_status = snapshot_worker_http(topology, &owner, inventory.address, "/status")?;
    let _: outbe_ocomp::worker_observability::WorkerStatusV1 =
        serde_json::from_slice(&before_status.body)?;
    let before = snapshot_worker_http(topology, &owner, inventory.address, "/metrics")?;
    worker_counters(&before.body)?;
    Ok(SnapshotWorkerRunning {
        before_request: inventory,
        before,
        before_status,
    })
}

/// Call after the existing root-owned public/local completion wait, before stop.
/// Select a new inbox artifact by its raw job identity; the later independent
/// admission/plan/CAS check authenticates it. The inbox is never the spec source.
fn snapshot_worker_after(
    topology: &mut crate::world::ocomp::OcompTopology,
    running: &SnapshotWorkerRunning,
    job_id: alloy_primitives::B256,
) -> eyre::Result<SnapshotWorkerLiveResult> {
    let owner = &running.before.owner;
    let inventory = &running.before_request;
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let current = observe_snapshot_directory(&inventory.artifact_inventory.root)?;
    let mut selected = None;
    for entry in &current.entries {
        if inventory.artifact_inventory.entries.contains(entry)
            || entry.extension().is_none_or(|ext| ext != "ocb1")
        {
            continue;
        }
        let observation = snapshot_read_file(&current.root.join(entry), SNAPSHOT_OBJECT_BYTES)?;
        let raw = observation
            .bytes
            .as_deref()
            .ok_or_else(|| eyre!("new inbox artifact vanished"))?;
        let artifact = outbe_ocomp_protocol::unit::UnitArtifactV1::decode_canonical(raw, &limits)?;
        if artifact.job_id == job_id && artifact.protocol_bundle_hash == inventory.bundle.hash() {
            ensure!(
                entry
                    == &std::path::PathBuf::from(format!("{}.ocb1", hex::encode(artifact.unit_id))),
                "inbox filename/unit mismatch"
            );
            selected = Some(observation);
            break;
        }
    }
    let artifact = selected
        .ok_or_else(|| eyre!("no newly present recipient artifact for finalized job {job_id}"))?;
    let local = snapshot_read_file(
        &inventory
            .local_inventory
            .root
            .join(format!("{}.lysis-result-v1.ocb1", hex::encode(job_id))),
        SNAPSHOT_OBJECT_BYTES,
    )?;
    let raw = local
        .bytes
        .as_deref()
        .ok_or_else(|| eyre!("new local result not yet committed"))?;
    let decoded = outbe_ocomp_protocol::result::LysisResultV1::decode_canonical(raw, &limits)?;
    ensure!(
        decoded.job_id == job_id
            && decoded.protocol_bundle_hash == inventory.bundle.hash()
            && decoded.encode_canonical(&limits)? == raw,
        "local result job/bundle/canonical encoding mismatch"
    );
    let local_result = SnapshotResultObservation {
        job_id: hex::encode(job_id),
        digest: hex::encode(decoded.result_digest(&limits)?),
    };
    let after_status = snapshot_worker_http(topology, owner, inventory.address, "/status")?;
    let _: outbe_ocomp::worker_observability::WorkerStatusV1 =
        serde_json::from_slice(&after_status.body)?;
    let after = snapshot_worker_http(topology, owner, inventory.address, "/metrics")?;
    let history = snapshot_v1_worker_records(topology.process_records())?;
    snapshot_require_history(&inventory.history, &history)?;
    ensure!(
        snapshot_unique_worker(&history)? == owner.process,
        "worker incarnation changed during observation"
    );
    let history_through = snapshot_now_millis()?;
    Ok(SnapshotWorkerLiveResult {
        running: running.clone(),
        after,
        after_status,
        artifact,
        local,
        local_result,
        job_id,
        history,
        history_through,
    })
}

/// Read immutable job authorities after the writer releases its lock (normally
/// root's existing ordinary stop before K). All native readers drop on return.
fn snapshot_worker_bind_admission(
    live: SnapshotWorkerLiveResult,
    requested: u64,
    request: SnapshotBlock,
    canonical_result: SnapshotResultObservation,
    canonical_result_at: SnapshotBlock,
) -> eyre::Result<SnapshotCollectedWorker> {
    use outbe_ocomp::{
        admission_catalog::AdmissionCatalogReader,
        cas::{CasLimits, FilesystemCasReader},
        input_artifacts::poc_input_list_limits,
        input_ref_catalog::VerifiedInputChunkRefCatalog,
        lysis_plan_audit::LocalLysisPlanAuditV1,
    };
    let inventory = &live.running.before_request;
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let reader = FilesystemCasReader::open(
        inventory.domain_root.join("cas-v1"),
        CasLimits {
            max_object_bytes: SNAPSHOT_OBJECT_BYTES,
            max_total_bytes: u64::MAX,
        },
    )?;
    let job = hex::encode(live.job_id);
    let admission_path = inventory
        .domain_root
        .join("supervisor-v1/jobs")
        .join(&job)
        .join("admissions");
    let admissions = AdmissionCatalogReader::open_existing(&admission_path, &reader, limits)?;
    let input_refs = VerifiedInputChunkRefCatalog::reopen(
        inventory
            .domain_root
            .join("exporter-v1/input-refs")
            .join(&job),
        &reader,
        limits,
        poc_input_list_limits(),
    )?;
    let audit = LocalLysisPlanAuditV1::open_read_only(
        &admissions,
        &input_refs,
        &reader,
        &inventory.bundle,
        &limits,
    )?;
    ensure!(
        audit.plan().job_id == live.job_id
            && audit.plan().protocol_bundle_hash == inventory.bundle.hash(),
        "independent plan is not the requested job/bundle"
    );
    let raw = live
        .artifact
        .bytes
        .as_deref()
        .ok_or_else(|| eyre!("missing captured artifact"))?;
    let artifact = outbe_ocomp_protocol::unit::UnitArtifactV1::decode_canonical(raw, &limits)?;
    let mut matched = None;
    for entry in admissions.exact_plan_cursor()? {
        let entry = entry?;
        if entry.unit_id == artifact.unit_id {
            ensure!(matched.is_none(), "duplicate unit admission");
            matched = Some(entry);
        }
    }
    let admitted = matched.ok_or_else(|| eyre!("captured unit has no independent admission"))?;
    let spec = audit.candidate_spec_at(admitted.plan_ordinal)?;
    ensure!(
        admitted.job_id == live.job_id
            && admitted.protocol_bundle_hash == inventory.bundle.hash()
            && admitted.plan_hash == audit.plan().plan_hash(&limits)?
            && admitted.unit_id == spec.unit_id(&limits)?,
        "admission does not bind the canonical job plan"
    );
    artifact.validate_against(&spec, &limits)?;
    let cas_object = reader.read_verified(&admitted.artifact_ref)?;
    ensure!(
        cas_object.bytes() == raw,
        "inbox bytes differ from independently admitted CAS object"
    );
    let admission_record = snapshot_read_file(
        &admission_path.join(format!("{:010}.admission", admitted.plan_ordinal)),
        SNAPSHOT_OBJECT_BYTES,
    )?;
    ensure!(
        admission_record.bytes.is_some(),
        "verified admission file vanished"
    );
    let workers = live
        .history
        .iter()
        .cloned()
        .map(|p| snapshot_owner(inventory, p))
        .collect();
    let worker = SnapshotWorkerExecution {
        owner: live.running.before.owner.clone(),
        before: live.running.before.clone(),
        after: live.after,
        attribution: SnapshotWorkerAttribution::SingleOwnedProducer {
            inventory_from: inventory.history_from,
            inventory_through: live.history_through,
            workers,
        },
        artifact_before: SnapshotPriorFileObservation::DirectoryListing(
            inventory.artifact_inventory.clone(),
        ),
        artifact_after: live.artifact,
        admission_catalog: admission_path,
        canonical_unit_spec: spec.encode_canonical(&limits)?,
        admitted_artifact_len: admitted.artifact_ref.encoded_bytes,
        admitted_artifact_keccak256: admitted.artifact_ref.transport_digest,
        log: None,
    };
    let new_job = SnapshotNewJobObservation {
        requested,
        request,
        job_id: job,
        worker,
        local_before: SnapshotPriorFileObservation::DirectoryListing(
            inventory.local_inventory.clone(),
        ),
        local_after: live.local,
        local_result_root: inventory.local_inventory.root.clone(),
        local_result: live.local_result,
        canonical_result,
        canonical_result_at,
    };
    actual_worker(&new_job, SNAPSHOT_RECIPIENT_SLOT)?;
    ensure!(
        new_job.local_result == new_job.canonical_result,
        "local result differs from independently observed public result"
    );
    newly_present(
        &new_job.local_before,
        &new_job.local_after,
        requested,
        &new_job.local_result_root,
    )?;
    Ok(SnapshotCollectedWorker {
        new_job,
        before_status: live.running.before_status,
        after_status: live.after_status,
        admission_record,
    })
}

fn snapshot_v1_worker_records(
    records: &[crate::world::ocomp::OcompProcessRecordV1],
) -> eyre::Result<Vec<crate::world::ocomp::OcompProcessRecordV1>> {
    let records: Vec<_> = records
        .iter()
        .filter(|p| {
            p.validator_index == Some(SNAPSHOT_RECIPIENT_SLOT)
                && p.role == crate::world::ocomp::OcompProcessRole::Worker
        })
        .cloned()
        .collect();
    ensure!(
        records.iter().all(|p| p.worker_ordinal == Some(0)),
        "collector is scoped to V1 worker0; another lane/ordinal is present"
    );
    Ok(records)
}

fn snapshot_require_history(
    before: &[crate::world::ocomp::OcompProcessRecordV1],
    after: &[crate::world::ocomp::OcompProcessRecordV1],
) -> eyre::Result<()> {
    ensure!(
        before.iter().all(|old| after.iter().any(|new| old == new)),
        "retained process history dropped or rewrote a prior stopped incarnation"
    );
    Ok(())
}

fn snapshot_unique_worker(
    records: &[crate::world::ocomp::OcompProcessRecordV1],
) -> eyre::Result<crate::world::ocomp::OcompProcessRecordV1> {
    let mut live = records.iter().filter(|p| p.stopped_at_millis.is_none());
    let process = live
        .next()
        .ok_or_else(|| eyre!("no live owned recipient worker"))?;
    ensure!(live.next().is_none(), "multiple live recipient workers");
    Ok(process.clone())
}

fn snapshot_owner(
    inventory: &SnapshotWorkerBeforeRequest,
    process: crate::world::ocomp::OcompProcessRecordV1,
) -> SnapshotWorkerOwner {
    SnapshotWorkerOwner {
        process,
        endpoint: format!("http://{}", inventory.address),
        inbox_root: inventory
            .domain_root
            .join("worker-inbox-v1")
            .join(hex::encode(inventory.bundle.hash())),
        bundle_hash: inventory.bundle.hash(),
    }
}

fn snapshot_worker_http(
    topology: &mut crate::world::ocomp::OcompTopology,
    owner: &SnapshotWorkerOwner,
    address: std::net::SocketAddr,
    path: &str,
) -> eyre::Result<SnapshotWorkerHttpObservation> {
    topology.ensure_worker_alive(SNAPSHOT_RECIPIENT_SLOT, 0)?;
    ensure!(
        snapshot_unique_worker(&snapshot_v1_worker_records(topology.process_records())?)?
            == owner.process,
        "wrong owned worker before HTTP observation"
    );
    let body = snapshot_http_get(address, path)?;
    topology.ensure_worker_alive(SNAPSHOT_RECIPIENT_SLOT, 0)?;
    ensure!(
        snapshot_unique_worker(&snapshot_v1_worker_records(topology.process_records())?)?
            == owner.process,
        "wrong owned worker after HTTP observation"
    );
    Ok(SnapshotWorkerHttpObservation {
        owner: owner.clone(),
        observed: snapshot_now_millis()?,
        body,
    })
}

fn snapshot_http_get(address: std::net::SocketAddr, path: &str) -> eyre::Result<Vec<u8>> {
    use std::io::{Read as _, Write as _};
    let timeout = std::time::Duration::from_secs(2);
    let mut stream = std::net::TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    // HTTP/1.0 requests close-delimited/non-chunked responses from this local server.
    stream.write_all(
        format!("GET {path} HTTP/1.0\r\nHost: {address}\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut raw = Vec::new();
    stream.take(65_537).read_to_end(&mut raw)?;
    ensure!(raw.len() <= 65_536, "worker HTTP response exceeds bound");
    snapshot_http_body(&raw)
}

fn snapshot_http_body(raw: &[u8]) -> eyre::Result<Vec<u8>> {
    let offset = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| eyre!("malformed worker HTTP headers"))?;
    let headers = std::str::from_utf8(&raw[..offset])?;
    ensure!(
        headers
            .lines()
            .next()
            .and_then(|s| s.split_whitespace().nth(1))
            == Some("200"),
        "worker HTTP request failed"
    );
    let body = &raw[offset + 4..];
    for line in headers.lines().skip(1) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| eyre!("malformed worker HTTP header"))?;
        ensure!(
            !name.eq_ignore_ascii_case("transfer-encoding"),
            "unexpected transfer encoding on HTTP/1.0 response"
        );
        if name.eq_ignore_ascii_case("content-length") {
            ensure!(
                value.trim().parse::<usize>()? == body.len(),
                "truncated or overlong worker HTTP body"
            );
        }
    }
    Ok(body.to_vec())
}

fn snapshot_read_file(path: &std::path::Path, max_bytes: u64) -> eyre::Result<SnapshotFileRead> {
    use std::io::Read as _;
    let bytes = match std::fs::File::open(path) {
        Ok(file) => {
            ensure!(file.metadata()?.is_file(), "expected regular evidence file");
            let mut bytes = Vec::new();
            file.take(
                max_bytes
                    .checked_add(1)
                    .ok_or_else(|| eyre!("invalid file byte bound"))?,
            )
            .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 <= max_bytes,
                "evidence file exceeds byte bound"
            );
            Some(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    Ok(SnapshotFileRead {
        path: path.to_path_buf(),
        observed: snapshot_now_millis()?,
        bytes,
    })
}

#[cucumber::when(
    "the snapshot FullNode executes a fresh next-day OCOMP job and restarts at current progress"
)]
fn new_snapshot_work_and_restart(world: &mut crate::world::World) {
    new_snapshot_work_and_restart_result(world)
        .expect("new real OCOMP work and ordinary current-K restart");
}

fn new_snapshot_work_and_restart_result(world: &mut crate::world::World) -> eyre::Result<()> {
    use crate::features::ocomp::{
        first_protocol_cycle_at_or_after, quorum_applies_lysis_and_creates_nod_for_request,
        restart_committee_at_logical_time, PublicVoteSetExpectation,
    };
    use crate::world::state::*;
    use outbe_primitives::time::WorldwideDay;
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    let inventory = world
        .state
        .offline_snapshot_worker_inventory
        .take()
        .ok_or_else(|| eyre!("missing stopped pre-request inventories"))?;
    let running = snapshot_worker_before(&mut world.ocomp, inventory)?;
    let original = world
        .state
        .ocomp_job_request
        .clone()
        .ok_or_else(|| eyre!("original completed request"))?;
    let day = WorldwideDay::from_timestamp(
        WorldwideDay::new(original.worldwide_day)
            .start_timestamp()
            .checked_add(86_400)
            .ok_or_else(|| eyre!("day overflow"))?,
    )
    .value();
    let primary = world.validators.primary_port();
    let mut schedule = world.rpc.metadosis_wwd_state_on(primary, day);
    if schedule.is_none() {
        // The native ProtocolCycle creates a day only after its forming start.
        // Read phase boundaries from that created state, never fabricate them.
        let creation =
            first_protocol_cycle_at_or_after(world, WorldwideDay::new(day).start_timestamp());
        let height = world
            .rpc
            .head(primary)
            .ok_or_else(|| eyre!("head before next WWD"))?;
        let timestamp = world
            .rpc
            .block_timestamp(primary, height)
            .ok_or_else(|| eyre!("timestamp before next WWD"))?;
        let mut publication = if timestamp < creation {
            restart_committee_at_logical_time(world, creation).3
        } else {
            None
        };
        let deadline = Instant::now() + Duration::from_secs(180);
        while schedule.is_none() || publication.is_some() {
            ensure!(
                Instant::now() < deadline,
                "next WWD creation and Oracle publication did not complete"
            );
            if let Some(pending) = publication.as_ref() {
                if crate::features::price_oracle::observe_pending_publication(world, pending) {
                    publication = None;
                }
            }
            schedule = world.rpc.metadosis_wwd_state_on(primary, day);
            ensure!(
                schedule.as_ref().is_none_or(|state| state.status <= 2),
                "next WWD passed offering while awaiting creation and Oracle publication"
            );
            if schedule.is_none() || publication.is_some() {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    let schedule = schedule.ok_or_else(|| eyre!("next WWD schedule unavailable"))?;
    ensure!(schedule.status <= 2, "next-day offering already passed");
    let mut publication = if schedule.status < 2 {
        restart_committee_at_logical_time(world, schedule.lookback_end + 1).3
    } else {
        None
    };
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let state = world
            .rpc
            .metadosis_wwd_state_on(primary, day)
            .ok_or_else(|| eyre!("next WWD schedule unavailable"))?;
        ensure!(
            state.status <= 2 && Instant::now() < deadline,
            "next WWD offering and Oracle publication did not remain available"
        );
        if let Some(pending) = publication.as_ref() {
            if crate::features::price_oracle::observe_pending_publication(world, pending) {
                publication = None;
                // Re-read current offering after the publication observation.
                continue;
            }
        }
        if state.status == 2 && publication.is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let cut = world
        .state
        .offline_snapshot
        .as_ref()
        .unwrap()
        .cut_canonical
        .number;
    ensure!(
        world
            .rpc
            .finalized_ocomp_job_request_for_worldwide_day_result_on(primary, cut, day)?
            .is_absent(),
        "follow-up job already existed before requested work"
    );
    let operator = world.validators.get(0).evm_key()?;
    let requested = snapshot_now_millis()?;
    let tx = world
        .rpc
        .tribute_offer(&operator, &day.to_string())
        .ok_or_else(|| eyre!("submit real next-day Tribute"))?;
    ensure!(
        world.rpc.wait_successful_receipt(&tx, 240),
        "new Tribute failed"
    );
    world.projection.wait_for_tribute_projection(&tx, 240)?;
    let processing = first_protocol_cycle_at_or_after(world, schedule.scheduled_process_time);
    let mut publication = restart_committee_at_logical_time(world, processing).3;
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut finalized_request = None;
    let request = loop {
        ensure!(
            Instant::now() < deadline,
            "new JobIntent and Oracle publication did not finalize"
        );
        if finalized_request.is_none() {
            finalized_request = world
                .rpc
                .finalized_ocomp_job_request_for_worldwide_day_result_on(primary, cut + 1, day)?
                .into_bound_request()?;
        }
        if let Some(pending) = publication.as_ref() {
            if crate::features::price_oracle::observe_pending_publication(world, pending) {
                publication = None;
            }
        }
        if publication.is_none() {
            if let Some(request) = finalized_request.take() {
                break request;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    ensure!(
        request.intent_id != original.intent_id && request.request_height > cut,
        "new job did not follow snapshot cut"
    );
    let request_block = canonical_snapshot_block(world, request.request_height)?;
    quorum_applies_lysis_and_creates_nod_for_request(
        world,
        request,
        PublicVoteSetExpectation::Exact(&[0, 1, 2, 3]),
    );
    let activation = world
        .state
        .ocomp_activation
        .clone()
        .ok_or_else(|| eyre!("new canonical Lysis result"))?;
    let canonical_result = SnapshotResultObservation {
        job_id: hex::encode(activation.job_id),
        digest: hex::encode(activation.result_digest),
    };
    let canonical_result_at = canonical_snapshot_block(world, activation.block_number)?;
    let slot = world.validators.joiner_index();
    ensure!(
        world.rpc.wait_finalized_at_least(
            world.validators.http_port(slot),
            activation.block_number + 2,
            300
        ),
        "recipient did not finalize new result"
    );
    let deadline = Instant::now() + Duration::from_secs(180);
    let live = loop {
        match snapshot_worker_after(&mut world.ocomp, &running, activation.job_id) {
            Ok(value) => break value,
            Err(error) => {
                ensure!(
                    Instant::now() < deadline,
                    "recipient new compute observation failed: {error:#}"
                );
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    };
    let launched = world
        .localnet
        .follower_launch_observation("snapshot-recipient", slot)?;
    let clients = world
        .ocomp
        .stop_node_facing_roles_for_snapshot(slot.try_into()?)?;
    let stopped =
        world
            .localnet
            .stop_follower_for_snapshot("snapshot-recipient", slot, launched.pid)?;
    let collected = snapshot_worker_bind_admission(
        live,
        requested,
        request_block,
        canonical_result,
        canonical_result_at,
    )?;
    let root = world.localnet.scenario_dir().join("offline-snapshot");
    std::fs::write(
        root.join("worker-native-evidence.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "before_status":collected.before_status,"after_status":collected.after_status,"admission_record":collected.admission_record,
        }))?,
    )?;
    let data = world.validators.data_dir(slot);
    let node = data.parent().unwrap();
    let chain_id = world
        .rpc
        .chain_id(primary)
        .ok_or_else(|| eyre!("chain id"))?;
    let genesis = canonical_snapshot_block(world, 0)?.hash.parse()?;
    let at_k =
        observe_stopped_native(node, &node.join("offchain-storage.toml"), chain_id, genesis)?;
    let k_canonical = canonical_snapshot_block(world, at_k.progress.finalized.number)?;
    let identity_at_k = recipient_identity(world)?;
    // Native-only audit is separate from the subsequent ordinary launch.
    let chain = snapshot_option(&launched.argv, "--chain")?;
    let mut command = Command::new(&launched.program);
    command
        .args([
            "snapshot",
            "validate",
            "--checks",
            "headers,evm,ce,bodies,ocomp",
            "--report",
        ])
        .arg(root.join("current-k-validation.json"))
        .args(["--", "--chain"])
        .arg(chain)
        .arg("--datadir")
        .arg(&data)
        .arg("--consensus.storage-dir")
        .arg(data.join("consensus"))
        .arg("--projection.storage-config")
        .arg(node.join("offchain-storage.toml"));
    let audit = run_snapshot_command(
        command,
        &root,
        "validate-current-k",
        Duration::from_secs(900),
    )?;
    let report = parse_snapshot_validation_report(&audit.stdout)?;
    ensure!(
        report.checks["files"].status == SnapshotCheckStatus::NotRequested
            && report.checks["provenance"].status == SnapshotCheckStatus::NotRequested,
        "current K audit unexpectedly requires old sidecars"
    );
    ensure!(
        !report
            .checks
            .values()
            .any(|check| check.status == SnapshotCheckStatus::Failed),
        "current K native validation failed"
    );
    let complete = report
        .checks
        .values()
        .filter(|check| check.selected)
        .all(|check| check.status == SnapshotCheckStatus::Passed);
    ensure!(
        (complete && audit.exit_code == Some(0))
            || (!complete && audit.exit_code.is_some_and(|code| code != 0)),
        "current K validation report/exit disagree"
    );
    let before_launch =
        observe_stopped_native(node, &node.join("offchain-storage.toml"), chain_id, genesis)?;
    let first_exit = SnapshotExitObservation {
        pid: stopped.observation.launch.pid,
        reaped: stopped.observation.reaped_at_millis,
        code: stopped.observation.code,
        signal: stopped.observation.signal,
    };
    let launch = world.localnet.resume_snapshot_node(stopped)?;
    let restarted = observe_snapshot_launch(world, launch, before_launch)?;
    ensure!(
        world.rpc.wait_finalized_at_least(
            world.validators.http_port(slot),
            at_k.progress.finalized.number + 1,
            180
        ),
        "restarted current K node did not continue"
    );
    world.ocomp.resume_snapshot_node_facing_roles(clients)?;
    let identity_restarted = recipient_identity(world)?;
    let fresh = std::fs::read(super::local_result_path(world, slot, activation.job_id))?;
    let result = outbe_ocomp_protocol::result::LysisResultV1::decode_canonical(
        &fresh,
        &outbe_ocomp_protocol::profile::poc_schema_limits(),
    )?;
    ensure!(
        result.result_digest(&outbe_ocomp_protocol::profile::poc_schema_limits())?
            == activation.result_digest,
        "current K restart changed new result"
    );
    let evidence = world.state.offline_snapshot.as_mut().unwrap();
    evidence.new_job = Some(collected.new_job);
    evidence.before_restart = Some(at_k);
    evidence.k_canonical = Some(k_canonical);
    evidence.first_exit = Some(first_exit);
    evidence.second_start = Some(restarted);
    evidence.identity_at_k = identity_at_k;
    evidence.identity_restarted = identity_restarted;
    Ok(())
}

#[cfg(test)]
mod source_cleanup_tests {
    use super::*;
    #[test]
    fn failed_second_source_move_restores_the_first_source() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("data");
        std::fs::create_dir(&first).unwrap();
        std::fs::write(first.join("native"), b"unchanged").unwrap();
        let pairs = [
            (first.clone(), root.path().join("hidden-data")),
            (root.path().join("absent"), root.path().join("hidden-ocomp")),
        ];
        assert!(hide_snapshot_sources(&pairs).is_err());
        assert_eq!(std::fs::read(first.join("native")).unwrap(), b"unchanged");
        assert!(!pairs[0].1.exists());
    }
    #[test]
    fn source_restore_attempts_remaining_paths_after_one_failure() {
        let root = tempfile::tempdir().unwrap();
        let pairs = [
            (root.path().join("data"), root.path().join("missing-backup")),
            (root.path().join("ocomp"), root.path().join("hidden-ocomp")),
        ];
        std::fs::create_dir(&pairs[1].1).unwrap();
        std::fs::write(pairs[1].1.join("result"), b"retained").unwrap();
        assert!(restore_snapshot_sources(&pairs).is_err());
        assert_eq!(
            std::fs::read(pairs[1].0.join("result")).unwrap(),
            b"retained"
        );
    }
}

fn restore_snapshot_sources(
    pairs: &[(std::path::PathBuf, std::path::PathBuf)],
) -> eyre::Result<()> {
    let mut failures = Vec::new();
    for (original, hidden) in pairs.iter().rev() {
        if let Err(error) = std::fs::rename(hidden, original) {
            failures.push(format!(
                "{} -> {}: {error}",
                hidden.display(),
                original.display()
            ));
        }
    }
    ensure!(
        failures.is_empty(),
        "restore stopped test donor paths: {}",
        failures.join("; ")
    );
    Ok(())
}

fn hide_snapshot_sources(pairs: &[(std::path::PathBuf, std::path::PathBuf)]) -> eyre::Result<()> {
    for (position, (original, hidden)) in pairs.iter().enumerate() {
        if let Err(error) = std::fs::rename(original, hidden) {
            let restored = restore_snapshot_sources(&pairs[..position]);
            return Err(eyre!(
                "move stopped test donor {} -> {}: {error}; restore: {restored:?}",
                original.display(),
                hidden.display()
            ));
        }
    }
    Ok(())
}

#[cucumber::then("the snapshot FullNode preserves the copied Lysis result and canonical state")]
fn snapshot_fullnode_preserves_copied_result(world: &mut crate::world::World) {
    snapshot_fullnode_preserves_copied_result_checked(world).expect("copied chain and OCOMP data");
}

fn snapshot_fullnode_preserves_copied_result_checked(
    world: &crate::world::World,
) -> eyre::Result<()> {
    let evidence = world
        .state
        .offline_snapshot
        .as_ref()
        .ok_or_else(|| eyre!("snapshot evidence"))?;
    let slot = world.validators.joiner_index();
    let port = world.validators.http_port(slot);
    let cut = &evidence.cut_canonical;
    ensure!(
        world.rpc.wait_finalized_at_least(port, cut.number + 1, 180),
        "recipient did not catch advancing chain"
    );
    ensure!(
        world
            .rpc
            .block_hash(port, cut.number)
            .ok_or_else(|| eyre!("recipient cut block"))?
            .parse::<alloy_primitives::B256>()?
            == cut.hash.parse::<alloy_primitives::B256>()?,
        "recipient canonical cut changed"
    );
    let primary_root = world
        .rpc
        .state_root(world.validators.primary_port(), cut.number)
        .ok_or_else(|| eyre!("primary cut state root"))?;
    ensure!(
        world.rpc.state_root(port, cut.number) == Some(primary_root),
        "recipient cut EVM state differs"
    );
    let job: alloy_primitives::B256 = evidence.copied_result.job_id.parse()?;
    let bytes = std::fs::read(super::local_result_path(world, slot, job))?;
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let result = outbe_ocomp_protocol::result::LysisResultV1::decode_canonical(&bytes, &limits)?;
    ensure!(
        result.job_id == job
            && hex::encode(result.result_digest(&limits)?) == evidence.copied_result.digest,
        "copied completed result changed"
    );
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .ok_or_else(|| eyre!("actual Lysis activation"))?;
    ensure!(
        world
            .rpc
            .finalized_ocomp_certified_generation_on(port, activation)
            == world.state.ocomp_certified_generation,
        "recipient certified generation differs"
    );
    let local_actions = super::result_nod_actions_on(world, slot, job);
    ensure!(
        local_actions == super::result_nod_actions_on(world, 0, job),
        "recipient saved NOD bodies differ"
    );
    ensure!(
        recipient_identity(world)? == evidence.identity_placed,
        "ordinary launch changed resident identity"
    );
    Ok(())
}

// Harness-only fragment, called after existing capacity completion and
// contributors_are_paid, BEFORE the next-day request changes current generation.
// Pass the copied generation/payout evidence and queue_sequence captured at E.
// Existing armProceedsForTest fixture remains part of this scenario's disclosure.
alloy_sol_types::sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface ISnapshotPayoutRead {
        struct ContributorLeaf { address owner; uint256 sourceTributeId; uint256 nominal; }
        struct ContributorRound { uint256 amount; uint32 contributorCount; uint256 paidSoFar; uint32 paidLeafCount; }
        function contributorPayoutRound(uint32 worldwideDay) external view returns (ContributorRound memory);
        function payContributorBatch(uint32 worldwideDay, uint32 startIndex, ContributorLeaf[] leaves, bytes32[] proof) external;
    }
}

struct SnapshotOwnerSample<T> {
    before: u64,
    observation: Result<Option<T>, String>,
    after: u64,
}

// Recognize only the adjacent, unchanged-root transition observed in E2E6.
// The RPC diagnostic is not a stable wire format: unknown spellings fail closed.
fn snapshot_forward_ce_mismatch(error: &str, before: u64, after: u64) -> bool {
    fn fields<'a>(text: &'a str, names: &[&str]) -> Option<Vec<&'a str>> {
        let values: Vec<_> = text.split(", ").collect();
        if values.len() != names.len() {
            return None;
        }
        values
            .into_iter()
            .zip(names)
            .map(|(value, name)| value.strip_prefix(*name)?.strip_prefix(": "))
            .collect()
    }
    let parsed = (|| -> Option<()> {
        let detail = error.strip_prefix(
            "eth_call failed: server returned an error response: error code -32603: Revm error: fatal: compressed-entity tree unavailable: exact parent mismatch: required ExactParentIdentity { ",
        )?;
        let (required, marker) = detail.split_once(" }, marker FinalizedMarker { ")?;
        let required = fields(
            required,
            &[
                "commitment_scheme_version",
                "block_number",
                "block_hash",
                "root",
            ],
        )?;
        let marker = fields(
            marker.strip_suffix(" }")?,
            &[
                "commitment_scheme_version",
                "height",
                "block_hash",
                "parent_block_hash",
                "parent_root",
                "new_root",
            ],
        )?;
        let required_scheme = required[0].parse::<u32>().ok()?;
        let marker_scheme = marker[0].parse::<u32>().ok()?;
        let required_height = required[1].parse::<u64>().ok()?;
        let marker_height = marker[1].parse::<u64>().ok()?;
        let required_hash = required[2].parse::<alloy_primitives::B256>().ok()?;
        let required_root = required[3].parse::<alloy_primitives::B256>().ok()?;
        let marker_hash = marker[2].parse::<alloy_primitives::B256>().ok()?;
        let parent_hash = marker[3].parse::<alloy_primitives::B256>().ok()?;
        let parent_root = marker[4].parse::<alloy_primitives::B256>().ok()?;
        let new_root = marker[5].parse::<alloy_primitives::B256>().ok()?;
        (before <= required_height
            && marker_height <= after
            && required_scheme == 1
            && required_scheme == marker_scheme
            && required_height.checked_add(1) == Some(marker_height)
            && parent_hash == required_hash
            && marker_hash != required_hash
            && parent_root == required_root
            && new_root == required_root)
            .then_some(())
    })();
    parsed.is_some()
}

// None requests a fresh complete observation; it never means an absent owner.
fn snapshot_owner_decision<T>(sample: SnapshotOwnerSample<T>) -> Result<Option<T>, String> {
    match sample.observation {
        Err(error) => {
            if sample.after > sample.before
                && snapshot_forward_ce_mismatch(&error, sample.before, sample.after)
            {
                Ok(None)
            } else {
                Err(error)
            }
        }
        Ok(None) => Err("missing materialized owner".to_owned()),
        Ok(Some(value)) => {
            if sample.after < sample.before {
                Err("owner observation head regressed".to_owned())
            } else if sample.after > sample.before {
                Ok(None)
            } else {
                Ok(Some(value))
            }
        }
    }
}

fn snapshot_observe_owner<T>(
    deadline: std::time::Instant,
    mut sample: impl FnMut() -> eyre::Result<SnapshotOwnerSample<T>>,
    mut now: impl FnMut() -> std::time::Instant,
    mut wait: impl FnMut(),
) -> eyre::Result<T> {
    let mut last = "no owner observation".to_owned();
    loop {
        ensure!(
            now() < deadline,
            "owner observation deadline exhausted: {last}"
        );
        let observed = sample()?;
        let outcome = match &observed.observation {
            Ok(Some(_)) => "present",
            Ok(None) => "missing",
            Err(error) => error.as_str(),
        };
        last = format!(
            "before={} after={} result={outcome}",
            observed.before, observed.after,
        );
        ensure!(
            now() < deadline,
            "owner observation deadline exhausted: {last}"
        );
        if let Some(value) =
            snapshot_owner_decision(observed).map_err(|error| eyre!("{error}; {last}"))?
        {
            return Ok(value);
        }
        eprintln!("snapshot_owner_observation_retry {last}");
        wait();
    }
}

fn snapshot_materialized_owner(
    world: &crate::world::World,
    port: u16,
    owner: alloy_primitives::Address,
    deadline: std::time::Instant,
) -> eyre::Result<(Vec<u8>, crate::internal::eth::INod::NodData)> {
    snapshot_observe_owner(
        deadline,
        || {
            let before = world
                .rpc
                .head(port)
                .ok_or_else(|| eyre!("head before owner read"))?;
            let observation = world.rpc.materialized_nod_for_owner(port, owner);
            // Preserve the whole Result until head movement has been observed.
            let after = world.rpc.head(port).ok_or_else(|| match &observation {
                Err(error) => eyre!("head after owner read unavailable; owner read error: {error}"),
                _ => eyre!("head after owner read unavailable"),
            })?;
            Ok(SnapshotOwnerSample {
                before,
                observation,
                after,
            })
        },
        std::time::Instant::now,
        || {
            std::thread::sleep(
                std::time::Duration::from_millis(250)
                    .min(deadline.saturating_duration_since(std::time::Instant::now())),
            );
        },
    )
    .map_err(|error| eyre!("materialized owner port={port} owner={owner:#x}: {error}"))
}

#[cfg(test)]
mod snapshot_owner_observation_tests {
    use super::*;
    use std::{
        cell::Cell,
        collections::VecDeque,
        time::{Duration, Instant},
    };

    const CE_RACE: &str = "eth_call failed: server returned an error response: error code -32603: Revm error: fatal: compressed-entity tree unavailable: exact parent mismatch: required ExactParentIdentity { commitment_scheme_version: 1, block_number: 521, block_hash: 0x4fd296af75fedd29d86ad983617c3614614f91b8bdb82001d00057e7cc1aacd7, root: 0x29f48a2e5bae541721b10af1233671747e7e955e794641a604aa9fc380a239e0 }, marker FinalizedMarker { commitment_scheme_version: 1, height: 522, block_hash: 0xb0d63f8c96229446dec5685a47eb1bd04a1299e86ca4ff11abc192fc7b445bb9, parent_block_hash: 0x4fd296af75fedd29d86ad983617c3614614f91b8bdb82001d00057e7cc1aacd7, parent_root: 0x29f48a2e5bae541721b10af1233671747e7e955e794641a604aa9fc380a239e0, new_root: 0x29f48a2e5bae541721b10af1233671747e7e955e794641a604aa9fc380a239e0 }";

    fn sample(
        before: u64,
        observation: Result<Option<u64>, String>,
        after: u64,
    ) -> SnapshotOwnerSample<u64> {
        SnapshotOwnerSample {
            before,
            observation,
            after,
        }
    }

    fn observe(samples: Vec<SnapshotOwnerSample<u64>>, seconds: u64) -> (eyre::Result<u64>, usize) {
        let started = Instant::now();
        let clock = Cell::new(started);
        let mut samples = VecDeque::from(samples);
        let mut calls = 0;
        let result = snapshot_observe_owner(
            started + Duration::from_secs(seconds),
            || {
                calls += 1;
                Ok(samples.pop_front().expect("unexpected observation retry"))
            },
            || clock.get(),
            || clock.set(clock.get() + Duration::from_secs(1)),
        );
        (result, calls)
    }

    #[test]
    fn crossed_forward_ce_race_restarts_the_whole_owner_observation() {
        let (result, calls) = observe(
            vec![
                sample(521, Err(CE_RACE.to_owned()), 522),
                sample(522, Ok(Some(7)), 522),
            ],
            5,
        );
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls, 2);
    }

    #[test]
    fn crossed_forward_success_is_discarded_before_accepting_a_stable_tuple() {
        let (result, calls) = observe(
            vec![
                sample(521, Ok(Some(99)), 522),
                sample(522, Ok(Some(7)), 522),
            ],
            5,
        );
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls, 2);
    }

    #[test]
    fn same_head_or_regressing_head_ce_mismatch_is_an_error() {
        for (before, after) in [(521, 521), (522, 521)] {
            let (result, calls) = observe(vec![sample(before, Err(CE_RACE.to_owned()), after)], 5);
            assert!(result.unwrap_err().to_string().contains(CE_RACE));
            assert_eq!(calls, 1);
        }
        let (result, calls) = observe(vec![sample(522, Ok(Some(7)), 521)], 5);
        assert!(result.unwrap_err().to_string().contains("regressed"));
        assert_eq!(calls, 1);
    }

    #[test]
    fn unrelated_head_movement_does_not_authorize_retry_of_an_old_ce_error() {
        let (result, calls) = observe(vec![sample(600, Err(CE_RACE.to_owned()), 601)], 5);
        assert!(result.unwrap_err().to_string().contains(CE_RACE));
        assert_eq!(calls, 1);
    }

    #[test]
    fn generic_rpc_decode_uniqueness_and_missing_owner_fail_even_during_progress() {
        for error in [
            "eth_call failed: connection reset",
            "ABI decode failed: invalid body",
            "balanceOf returned 2, expected exactly one",
            "owner has more than one materialized NOD",
            "execution reverted: index out of bounds",
            "compressed-entity tree unavailable: exact parent mismatch",
        ] {
            let (result, calls) = observe(vec![sample(521, Err(error.to_owned()), 522)], 5);
            assert!(result.unwrap_err().to_string().contains(error));
            assert_eq!(calls, 1);
        }
        let (result, calls) = observe(vec![sample(521, Ok(None), 522)], 5);
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing materialized owner"));
        assert_eq!(calls, 1);
    }

    #[test]
    fn unrelated_or_malformed_ce_identities_are_not_retried() {
        let bad_hash = format!("0x{}", "11".repeat(32));
        let errors = [
            CE_RACE.replace("commitment_scheme_version: 1", "commitment_scheme_version: 2"),
            CE_RACE.replace("height: 522", "height: 521"),
            CE_RACE.replace("height: 522", "height: 523"),
            CE_RACE.replace("height: 522", "height: 520"),
            CE_RACE.replace("FinalizedMarker { commitment_scheme_version: 1", "FinalizedMarker { commitment_scheme_version: 2"),
            CE_RACE.replace("parent_block_hash: 0x4fd296af75fedd29d86ad983617c3614614f91b8bdb82001d00057e7cc1aacd7", &format!("parent_block_hash: {bad_hash}")),
            CE_RACE.replace("parent_root: 0x29f48a2e5bae541721b10af1233671747e7e955e794641a604aa9fc380a239e0", &format!("parent_root: {bad_hash}")),
            CE_RACE.replace("new_root: 0x29f48a2e5bae541721b10af1233671747e7e955e794641a604aa9fc380a239e0", &format!("new_root: {bad_hash}")),
            CE_RACE.replace("height: 522", "height: broken"),
        ];
        for error in errors {
            let (result, calls) = observe(vec![sample(521, Err(error.clone()), 522)], 5);
            assert!(result.unwrap_err().to_string().contains(&error));
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn repeated_forward_errors_exhaust_one_deadline_with_last_heads_and_error() {
        let (result, calls) = observe(
            vec![
                sample(521, Err(CE_RACE.to_owned()), 522),
                sample(521, Err(CE_RACE.to_owned()), 522),
            ],
            2,
        );
        let error = result.unwrap_err().to_string();
        assert!(error.contains("deadline exhausted"));
        assert!(error.contains("before=521 after=522"));
        assert!(error.contains(CE_RACE));
        assert_eq!(calls, 2);
    }
}

fn snapshot_public_effects(
    world: &crate::world::World,
    cut_height: u64,
    copied_queue_sequence: u64,
    generation: &crate::world::rpc::OcompCertifiedGenerationV1,
    payout: &crate::world::state::ContributorPayoutEvidenceV1,
) -> eyre::Result<serde_json::Value> {
    use crate::internal::{addresses, eth};
    use alloy_primitives::{address, Address, U256};
    use alloy_sol_types::{SolCall, SolValue};
    use eyre::{ensure, eyre};
    use outbe_ocomp_protocol::{
        abi::{decode_materialize_certified_nods_calldata, MATERIALIZE_CERTIFIED_NODS_SELECTOR},
        list::streaming_ordered_list_membership_proof,
        profile::poc_schema_limits,
        result::{ActiveNodSetV1, NodMembershipProofV1},
        ListKind,
    };
    let primary = world.validators.primary_port();
    let slot = world.validators.joiner_index();
    let recipient = world.validators.http_port(slot);
    let url = world.rpc.url(primary);
    let recipient_url = world.rpc.url(recipient);
    let after = payout
        .after
        .as_ref()
        .ok_or_else(|| eyre!("contributors_are_paid has not completed"))?;
    ensure!(
        payout.worldwide_day == generation.worldwide_day,
        "wrong copied payout day"
    );
    let completed = world
        .rpc
        .completed_nod_materialization(primary, generation)
        .ok_or_else(|| eyre!("copied generation is not fully materialized"))?;
    let through = after.height.max(completed.completion_block_number);
    ensure!(
        world.rpc.wait_finalized_at_least(recipient, through, 180),
        "FullNode has not reached public completion"
    );
    ensure!(
        world
            .rpc
            .completed_nod_materialization(recipient, generation)
            == Some(completed.clone()),
        "FullNode materialization completion differs"
    );
    ensure!(
        world.rpc.state_root(recipient, after.height) == Some(format!("{:#x}", after.state_root)),
        "FullNode payout state root differs"
    );
    ensure!(
        world.rpc.block_hash(recipient, after.height) == Some(format!("{:#x}", after.block_hash)),
        "FullNode payout checkpoint differs"
    );

    let actions = super::result_nod_actions_on(world, slot, generation.job_id);
    ensure!(
        actions == super::result_nod_actions_on(world, 0, generation.job_id),
        "copied local Nod actions differ"
    );
    ensure!(
        actions.len() == generation.nod_count as usize,
        "copied Nod population differs"
    );
    ensure!(
        crate::internal::nod_reference::nod_root(&actions) == generation.nod_root,
        "local actions do not bind canonical Nod root"
    );
    let limits = poc_schema_limits();
    let records = actions
        .iter()
        .map(|a| a.encode_canonical_record(&limits))
        .collect::<Result<Vec<_>, _>>()?;
    let authority = ActiveNodSetV1 {
        job_id: generation.job_id,
        program_semantics_hash: generation.program_semantics_hash,
        worldwide_day: generation.worldwide_day,
        generation: generation.generation,
        nod_root: generation.nod_root,
        nod_count: generation.nod_count,
    };
    let mut proofs = Vec::new();
    // One observer budget across all owners and both nodes; retries never extend it.
    let owner_deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    for (ordinal, action) in actions.iter().enumerate() {
        let proof = NodMembershipProofV1 {
            job_id: generation.job_id,
            program_semantics_hash: generation.program_semantics_hash,
            worldwide_day: generation.worldwide_day,
            generation: generation.generation,
            nod_ordinal: ordinal.try_into()?,
            action: action.clone(),
            membership_siblings: streaming_ordered_list_membership_proof(
                ListKind::NodActions,
                generation.nod_count,
                ordinal.try_into()?,
                &records,
                limits.max_bounded_bytes,
            )?,
        };
        proof.verify_against(&authority, &limits)?;
        let local = snapshot_materialized_owner(world, recipient, action.owner, owner_deadline)?;
        let canonical = snapshot_materialized_owner(world, primary, action.owner, owner_deadline)?;
        ensure!(
            local.0 == action.nod_id.as_slice()
                && local.0 == canonical.0
                && local.1.abi_encode() == canonical.1.abi_encode(),
            "FullNode Nod body differs"
        );
        proofs.push(serde_json::json!({"nod_id":hex::encode(action.nod_id),"proof":hex::encode(proof.encode_canonical_record(&limits)?),"body":hex::encode(local.1.abi_encode())}));
    }
    let factory = address!("0000000000000000000000000000000000001015");
    let round_call = ISnapshotPayoutRead::contributorPayoutRoundCall {
        worldwideDay: payout.worldwide_day,
    };
    let round = eth::read_call_at_result(&recipient_url, factory, &round_call, after.height)
        .map_err(|e| eyre!(e))?;
    let primary_round =
        eth::read_call_at_result(&url, factory, &round_call, after.height).map_err(|e| eyre!(e))?;
    ensure!(
        round.abi_encode() == primary_round.abi_encode()
            && round.amount == payout.amount
            && round.paidSoFar == payout.expected_paid
            && round.contributorCount as usize == payout.contributors.len()
            && round.paidLeafCount == round.contributorCount,
        "FullNode payout round differs or remains pending"
    );
    let balance_at = |account: Address| -> eyre::Result<U256> {
        let raw = eth::raw_json_with_params(
            &recipient_url,
            "eth_getBalance",
            serde_json::json!([format!("{account:#x}"), format!("0x{:x}", after.height)]),
        )
        .ok_or_else(|| eyre!("FullNode checkpoint balance unavailable"))?;
        Ok(U256::from_str_radix(
            raw.as_str()
                .ok_or_else(|| eyre!("balance encoding"))?
                .trim_start_matches("0x"),
            16,
        )?)
    };
    let balances = payout
        .contributors
        .iter()
        .map(|c| balance_at(c.owner))
        .collect::<eyre::Result<Vec<_>>>()?;
    ensure!(
        balances == after.owner_balances && balance_at(factory)? == after.factory_balance,
        "FullNode public payout balances differ"
    );

    let recipient_signer = eth::address_of(&world.validators.get(slot).evm_key()?)
        .ok_or_else(|| eyre!("recipient EVM address"))?;
    let mut delegate_owners = std::collections::BTreeMap::new();
    for index in 0..world.validators.size() {
        let validator = eth::address_of(&world.validators.get(index).evm_key()?)
            .ok_or_else(|| eyre!("validator EVM address"))?;
        delegate_owners.insert(
            world.ocomp.ocomp_delegate_address(index.try_into()?)?,
            validator,
        );
    }
    let mut transactions = Vec::new();
    let mut materialization_count = 0;
    let mut payout_count = 0;
    let mut first = cut_height
        .checked_add(1)
        .ok_or_else(|| eyre!("cut overflow"))?;
    while first <= through {
        let last = first.saturating_add(63).min(through);
        let blocks = eth::blocks_with_transactions(&url, first, last, 64)
            .ok_or_else(|| eyre!("public transaction scan failed"))?;
        for (height, block) in (first..=last).zip(blocks) {
            for tx in block["transactions"]
                .as_array()
                .ok_or_else(|| eyre!("missing public transactions"))?
            {
                let Some(to) = tx["to"].as_str().and_then(|s| s.parse::<Address>().ok()) else {
                    continue;
                };
                if to != addresses::NOD_FACTORY_ADDR && to != factory {
                    continue;
                }
                let input = hex::decode(
                    tx["input"]
                        .as_str()
                        .ok_or_else(|| eyre!("missing calldata"))?
                        .trim_start_matches("0x"),
                )?;
                let materialization = to == addresses::NOD_FACTORY_ADDR
                    && input.get(..4) == Some(MATERIALIZE_CERTIFIED_NODS_SELECTOR.as_slice());
                let paying = to == factory
                    && input.get(..4)
                        == Some(ISnapshotPayoutRead::payContributorBatchCall::SELECTOR.as_slice());
                if !materialization && !paying {
                    continue;
                }
                if materialization
                    && decode_materialize_certified_nods_calldata(&input, &limits)?.queue_sequence
                        != copied_queue_sequence
                {
                    continue;
                }
                if paying
                    && ISnapshotPayoutRead::payContributorBatchCall::abi_decode(&input)?
                        .worldwideDay
                        != payout.worldwide_day
                {
                    continue;
                }
                let signer: Address = tx["from"]
                    .as_str()
                    .ok_or_else(|| eyre!("missing sender"))?
                    .parse()?;
                ensure!(
                    signer != recipient_signer,
                    "recipient submitted copied public effects"
                );
                let hash = tx["hash"]
                    .as_str()
                    .ok_or_else(|| eyre!("missing transaction hash"))?;
                let receipt = eth::receipt_json(&url, hash)
                    .ok_or_else(|| eyre!("missing public effect receipt"))?;
                if receipt["status"].as_str() != Some("0x1") {
                    continue;
                }
                ensure!(
                    receipt["blockHash"] == block["hash"]
                        && receipt["transactionHash"] == tx["hash"],
                    "receipt is not this canonical transaction"
                );
                let validator = *delegate_owners
                    .get(&signer)
                    .ok_or_else(|| eyre!("successful sender is not an existing OCOMP delegate"))?;
                // Same existing OCOMP role value as verify_ocomp_delegate_bindings.
                let parent = height
                    .checked_sub(1)
                    .ok_or_else(|| eyre!("genesis transaction"))?;
                let active = eth::read_call_at_result(
                    &url,
                    addresses::VS_ADDR,
                    &eth::IValidatorSet::getActiveValidatorsCall {},
                    parent,
                )
                .map_err(|e| eyre!(e))?;
                let declared = eth::read_call_at_result(
                    &url,
                    addresses::VS_ADDR,
                    &eth::IValidatorSet::getDelegateCall { validator, role: 2 },
                    parent,
                )
                .map_err(|e| eyre!(e))?;
                let resolved = eth::read_call_at_result(
                    &url,
                    addresses::VS_ADDR,
                    &eth::IValidatorSet::resolveValidatorCall { role: 2, signer },
                    parent,
                )
                .map_err(|e| eyre!(e))?;
                ensure!(
                    active.contains(&validator) && declared == signer && resolved == validator,
                    "sender lacks existing active-validator delegate binding at transaction parent"
                );
                if materialization {
                    materialization_count += 1;
                } else {
                    payout_count += 1;
                }
                transactions.push(serde_json::json!({"block_number":height,"transaction":tx,"receipt":receipt,"validator":validator,"delegate":signer}));
            }
        }
        first = last.checked_add(1).ok_or_else(|| eyre!("scan overflow"))?;
    }
    ensure!(
        materialization_count > 0 && payout_count > 0,
        "missing actual post-cut materialization or payout transactions"
    );
    Ok(
        serde_json::json!({"generation":generation,"completion":completed,"payout_checkpoint":after,
        "payout_round":hex::encode(round.abi_encode()),"recipient_balances":balances,
        "nod_proofs":proofs,"post_cut_transactions":transactions,
        "fixture":"Existing armProceedsForTest/distribute fixture; public payout execution and balances are observed, not injected."}),
    )
}

#[cucumber::then(
    "the snapshot FullNode observes the same completed public actions without submitting them"
)]
fn snapshot_observes_public_actions(world: &mut crate::world::World) {
    snapshot_observes_public_actions_checked(world).expect("copied public effects on FullNode");
}

fn snapshot_observes_public_actions_checked(world: &crate::world::World) -> eyre::Result<()> {
    let root = world.localnet.scenario_dir().join("offline-snapshot");
    let pending: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("pending-native-cut.json"))?)?;
    let queue = pending["execution"]["queue_sequence"]
        .as_u64()
        .ok_or_else(|| eyre!("native cut queue sequence"))?;
    let snapshot = world
        .state
        .offline_snapshot
        .as_ref()
        .ok_or_else(|| eyre!("snapshot evidence"))?;
    let generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .ok_or_else(|| eyre!("certified generation"))?;
    let payout = world
        .state
        .ocomp_contributor_payout
        .as_ref()
        .ok_or_else(|| eyre!("completed payout"))?;
    let observed = snapshot_public_effects(
        world,
        snapshot.cut_canonical.number,
        queue,
        generation,
        payout,
    )?;
    std::fs::write(
        root.join("public-effects.json"),
        serde_json::to_vec_pretty(&observed)?,
    )?;
    Ok(())
}

/// Exercise public CLI failures without modifying the ready recipient databases.
#[allow(clippy::too_many_arguments)]
fn snapshot_rejects_damaged_artifact(
    program: &std::path::Path,
    archive: &std::path::Path,
    manifest: &std::path::Path,
    signature: &std::path::Path,
    evidence_dir: &std::path::Path,
    creator: &str,
    chain: &str,
    node: &std::path::Path,
) -> eyre::Result<()> {
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
    // No payload bytes have arrived; valid metadata must not imply file success.
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

fn snapshot_pending_cut(
    progress: &SnapshotNativeProgress,
    mut read: impl FnMut(&SnapshotBlock) -> eyre::Result<serde_json::Value>,
) -> eyre::Result<serde_json::Value> {
    let execution = read(&progress.execution)?;
    Ok(serde_json::json!({"finalized_block":progress.finalized,"execution":execution}))
}
