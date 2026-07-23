//! Independent, fail-closed verification of OCOMP PoC evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use eyre::{bail, ensure, Result, WrapErr};
use walkdir::WalkDir;

use super::discovery::{discover, validate_discovery};
use super::io::hash_file;
use super::ledger::PlanningLedger;
use super::schema::{
    AssertionRecordV1, AssertionStatus, ClosureReportV1, EvidenceMode, MemberDigestV1,
    RunManifestV1, TestDiscoveryV1, RUNTIME_SCHEMA_VERSION,
};

/// Verify one published manifest without trusting scenario-declared coverage.
pub fn verify_manifest(
    repo: &Path,
    ledger: &PlanningLedger,
    manifest_path: &Path,
) -> Result<ClosureReportV1> {
    ledger.validate()?;
    let bytes = std::fs::read(manifest_path)
        .wrap_err_with(|| format!("read run manifest {}", manifest_path.display()))?;
    let manifest: RunManifestV1 =
        serde_json::from_slice(&bytes).wrap_err("decode run-manifest.json")?;
    validate_manifest_shape(&manifest)?;
    for section in &ledger.runtime_evidence.required_sections {
        ensure!(
            manifest.sections.get(section).is_some_and(substantive_json),
            "run manifest is missing required section {section}"
        );
    }
    let forbidden = ledger
        .runtime_evidence
        .forbidden_content
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    for (section, value) in &manifest.sections {
        ensure!(
            !contains_forbidden_key(value, &forbidden),
            "run manifest section {section} contains forbidden content"
        );
    }
    for member in &manifest.members {
        let path = member.path.to_ascii_lowercase();
        ensure!(
            forbidden.iter().all(|forbidden| !path.contains(forbidden)),
            "evidence member path {} names forbidden content",
            member.path
        );
    }

    let bundle_root = manifest_path
        .parent()
        .ok_or_else(|| eyre::eyre!("run manifest has no parent directory"))?;
    validate_members(bundle_root, manifest_path, &manifest)?;

    let mut actual_discovery = discover(repo, ledger)?;
    actual_discovery.normalize();
    let mut claimed_discovery = manifest.discovery.clone();
    claimed_discovery.normalize();
    ensure!(
        claimed_discovery == actual_discovery,
        "manifest discovery differs from independent source discovery"
    );
    validate_discovery(&actual_discovery)?;

    let assertions = read_assertions(bundle_root, &manifest)?;
    validate_assertions(ledger, &manifest, &assertions)?;
    Ok(compute_report(ledger, &manifest, &assertions))
}

/// Build a visibly incomplete report when no bundle exists.
#[must_use]
pub fn missing_bundle_report(
    ledger: &PlanningLedger,
    mode: EvidenceMode,
    discovery: TestDiscoveryV1,
    reason: impl Into<String>,
) -> ClosureReportV1 {
    let task_id = match &mode {
        EvidenceMode::TaskProgress { task_id } => Some(task_id.clone()),
        EvidenceMode::PocClosure => None,
    };
    let mode_name = mode_name(&mode).to_owned();
    let mut missing = ledger.test_ids().into_iter().collect::<Vec<_>>();
    missing.sort();
    ClosureReportV1 {
        schema_version: RUNTIME_SCHEMA_VERSION,
        mode: mode_name,
        task_id,
        source_sha: None,
        status: "FAIL".to_owned(),
        passed_test_ids: Vec::new(),
        missing_test_ids: missing,
        non_pass_test_ids: Vec::new(),
        requirement_gaps: ledger.required_coverage().into_keys().collect(),
        deferred_requirement_ids: ledger.deferred.keys().cloned().collect(),
        errors: {
            let mut errors = vec![reason.into()];
            if !discovery.skipped.is_empty() {
                errors.push(format!("skipped tests: {:?}", discovery.skipped));
            }
            if !discovery.todo.is_empty() {
                errors.push(format!("todo tests: {:?}", discovery.todo));
            }
            if !discovery.quarantined.is_empty() {
                errors.push(format!("quarantined tests: {:?}", discovery.quarantined));
            }
            if !discovery.retried.is_empty() {
                errors.push(format!("retried tests: {:?}", discovery.retried));
            }
            errors
        },
    }
}

/// Narrow task-progress claim after the task-local command has passed.
pub fn task_progress_report(
    ledger: &PlanningLedger,
    task_id: &str,
    discovery: TestDiscoveryV1,
    passed: &[String],
) -> Result<ClosureReportV1> {
    validate_discovery(&discovery)?;
    let owned = ledger.owned_tests(task_id)?;
    let passed_set = passed.iter().cloned().collect::<BTreeSet<_>>();
    ensure!(
        passed_set.len() == passed.len(),
        "task-progress passed list contains duplicates"
    );
    let discovered = discovery
        .discovered
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let owned_set = owned.iter().cloned().collect::<BTreeSet<_>>();
    ensure!(
        owned_set.is_subset(&passed_set),
        "task {task_id} did not pass every owned ID: expected at least {owned_set:?}, got {passed_set:?}"
    );
    ensure!(
        passed_set == discovered,
        "task {task_id} must pass every stable test discovered at this revision: expected {discovered:?}, got {passed_set:?}"
    );

    let mut missing = ledger
        .test_ids()
        .difference(&passed_set)
        .cloned()
        .collect::<Vec<_>>();
    missing.sort();
    let requirement_gaps = ledger
        .required_coverage()
        .into_iter()
        .filter(|(_, tests)| tests.iter().any(|test| !passed_set.contains(test)))
        .map(|(id, _)| id)
        .collect();
    Ok(ClosureReportV1 {
        schema_version: RUNTIME_SCHEMA_VERSION,
        mode: "task_progress".to_owned(),
        task_id: Some(task_id.to_owned()),
        source_sha: None,
        status: "PASS".to_owned(),
        passed_test_ids: passed_set.into_iter().collect(),
        missing_test_ids: missing,
        non_pass_test_ids: Vec::new(),
        requirement_gaps,
        deferred_requirement_ids: ledger.deferred.keys().cloned().collect(),
        errors: Vec::new(),
    })
}

fn validate_manifest_shape(manifest: &RunManifestV1) -> Result<()> {
    ensure!(
        manifest.schema_version == RUNTIME_SCHEMA_VERSION,
        "unsupported runtime evidence schema"
    );
    ensure!(
        safe_identity(&manifest.run_id),
        "run ID is empty or filesystem-unsafe"
    );
    ensure!(
        manifest.started_at <= manifest.finished_at,
        "run finish precedes start"
    );
    ensure!(
        is_lower_hex(&manifest.source.sha),
        "source SHA is not lowercase hex"
    );
    for digest in [
        &manifest.source.cargo_lock_sha256,
        &manifest.source.rust_toolchain_sha256,
        &manifest.source.dependency_metadata_sha256,
    ] {
        ensure!(
            digest.len() == 64 && is_lower_hex(digest),
            "source identity contains an invalid SHA-256"
        );
    }
    if matches!(manifest.mode, EvidenceMode::PocClosure) {
        ensure!(
            !manifest.source.tracked_dirty && !manifest.source.untracked_dirty,
            "PoC closure requires a clean tracked and untracked source tree"
        );
    }
    Ok(())
}

fn validate_members(
    bundle_root: &Path,
    manifest_path: &Path,
    manifest: &RunManifestV1,
) -> Result<()> {
    ensure!(!manifest.members.is_empty(), "run manifest has no members");
    let mut expected = BTreeMap::<String, &MemberDigestV1>::new();
    let mut prior = None::<&str>;
    for member in &manifest.members {
        validate_member_path(&member.path)?;
        if let Some(previous) = prior {
            ensure!(
                previous < member.path.as_str(),
                "manifest members are not strictly path-sorted"
            );
        }
        prior = Some(&member.path);
        ensure!(
            member.sha256.len() == 64 && is_lower_hex(&member.sha256),
            "member {} has invalid SHA-256",
            member.path
        );
        ensure!(
            expected.insert(member.path.clone(), member).is_none(),
            "duplicate manifest member {}",
            member.path
        );
        let actual = hash_file(&bundle_root.join(&member.path))?;
        ensure!(
            actual.length == member.length && actual.sha256 == member.sha256,
            "member hash/length mismatch for {}",
            member.path
        );
    }
    ensure!(
        expected.contains_key(&manifest.assertions_path),
        "assertions file is not a manifest member"
    );

    let actual_files = WalkDir::new(bundle_root)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(bundle_root).ok()?;
            let relative = relative.to_string_lossy().replace('\\', "/");
            (!is_verifier_output(&relative) && relative != "run-manifest.json").then_some(relative)
        })
        .collect::<BTreeSet<_>>();
    let listed = expected.keys().cloned().collect::<BTreeSet<_>>();
    ensure!(
        actual_files == listed,
        "manifest member set differs from bundle files: listed={listed:?}, actual={actual_files:?}"
    );

    let manifest_modified = std::fs::metadata(manifest_path)
        .and_then(|metadata| metadata.modified())
        .wrap_err("read run manifest publication time")?;
    for member in expected.keys() {
        let modified = std::fs::metadata(bundle_root.join(member))
            .and_then(|metadata| metadata.modified())
            .wrap_err_with(|| format!("read publication time for {member}"))?;
        ensure!(
            modified <= manifest_modified,
            "run manifest was not published after member {member}"
        );
    }
    Ok(())
}

fn read_assertions(bundle_root: &Path, manifest: &RunManifestV1) -> Result<Vec<AssertionRecordV1>> {
    let path = bundle_root.join(&manifest.assertions_path);
    let text = std::fs::read_to_string(&path)
        .wrap_err_with(|| format!("read assertions {}", path.display()))?;
    ensure!(!text.trim().is_empty(), "assertions JSONL is empty");
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            ensure!(
                !line.trim().is_empty(),
                "blank assertion line {}",
                index + 1
            );
            serde_json::from_str::<AssertionRecordV1>(line)
                .wrap_err_with(|| format!("decode assertion line {}", index + 1))
        })
        .collect()
}

fn validate_assertions(
    ledger: &PlanningLedger,
    manifest: &RunManifestV1,
    assertions: &[AssertionRecordV1],
) -> Result<()> {
    let member_paths = manifest
        .members
        .iter()
        .map(|member| member.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut assertion_ids = BTreeSet::new();
    for assertion in assertions {
        ensure!(
            !assertion.assertion_id.trim().is_empty(),
            "assertion has an empty ID"
        );
        ensure!(
            assertion_ids.insert(assertion.assertion_id.as_str()),
            "duplicate assertion ID {}",
            assertion.assertion_id
        );
        let test = ledger.tests.get(&assertion.test_id).ok_or_else(|| {
            eyre::eyre!("assertion references unknown test {}", assertion.test_id)
        })?;
        ensure!(
            test.oracles.contains(&assertion.oracle),
            "test {} cannot use oracle {}",
            assertion.test_id,
            assertion.oracle
        );
        ensure!(
            assertion.run_id == manifest.run_id,
            "assertion {} belongs to another run",
            assertion.assertion_id
        );
        ensure!(
            assertion.source_sha == manifest.source.sha,
            "assertion {} belongs to another source revision",
            assertion.assertion_id
        );
        ensure!(
            assertion.observed_at >= manifest.started_at
                && assertion.observed_at <= manifest.finished_at,
            "assertion {} is stale or outside the run interval",
            assertion.assertion_id
        );
        ensure!(
            assertion.attempt == 1,
            "assertion {} was retried",
            assertion.assertion_id
        );
        ensure!(
            !assertion.expected_artifact_refs.is_empty()
                && !assertion.actual_artifact_refs.is_empty(),
            "assertion {} has empty artifact references",
            assertion.assertion_id
        );
        for reference in assertion
            .expected_artifact_refs
            .iter()
            .chain(&assertion.actual_artifact_refs)
        {
            ensure!(
                member_paths.contains(reference.as_str()),
                "assertion {} references unlisted member {reference}",
                assertion.assertion_id
            );
        }
        ensure!(
            assertion.status != AssertionStatus::Deferred,
            "stable test {} cannot self-declare DEFERRED",
            assertion.test_id
        );
    }
    Ok(())
}

fn compute_report(
    ledger: &PlanningLedger,
    manifest: &RunManifestV1,
    assertions: &[AssertionRecordV1],
) -> ClosureReportV1 {
    let mut by_test = BTreeMap::<String, Vec<AssertionStatus>>::new();
    for assertion in assertions {
        by_test
            .entry(assertion.test_id.clone())
            .or_default()
            .push(assertion.status);
    }
    let passed = by_test
        .iter()
        .filter(|(_, statuses)| {
            !statuses.is_empty()
                && statuses
                    .iter()
                    .all(|status| *status == AssertionStatus::Pass)
        })
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let non_pass = by_test
        .iter()
        .filter(|(_, statuses)| {
            statuses
                .iter()
                .any(|status| *status != AssertionStatus::Pass)
        })
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let all_tests = ledger.test_ids();
    let discovered = manifest
        .discovery
        .discovered
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut missing = all_tests
        .difference(&passed)
        .cloned()
        .collect::<BTreeSet<_>>();
    missing.extend(all_tests.difference(&discovered).cloned());

    let requirement_gaps = ledger
        .required_coverage()
        .into_iter()
        .filter(|(_, tests)| tests.iter().any(|test| !passed.contains(test)))
        .map(|(id, _)| id)
        .collect::<Vec<_>>();

    let mut errors = Vec::new();
    let mode_ok = match &manifest.mode {
        EvidenceMode::PocClosure => {
            if passed != all_tests {
                errors.push("not every stable test passed on this artifact set".to_owned());
            }
            if !requirement_gaps.is_empty() {
                errors.push("normative coverage is incomplete".to_owned());
            }
            passed == all_tests && requirement_gaps.is_empty()
        }
        EvidenceMode::TaskProgress { task_id } => {
            let owned = ledger
                .owned_tests(task_id)
                .unwrap_or_default()
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            let owned_ok = owned.is_subset(&passed);
            let discovered_ok = discovered.is_subset(&passed);
            if !owned_ok {
                errors.push(format!("task {task_id} owned tests did not all pass"));
            }
            if !discovered_ok {
                errors.push("a discovered OCOMP test did not pass".to_owned());
            }
            owned_ok && discovered_ok
        }
    };
    if !non_pass.is_empty() {
        errors.push("bundle contains non-PASS stable tests".to_owned());
    }
    let passed_ok = mode_ok && non_pass.is_empty();

    ClosureReportV1 {
        schema_version: RUNTIME_SCHEMA_VERSION,
        mode: mode_name(&manifest.mode).to_owned(),
        task_id: match &manifest.mode {
            EvidenceMode::TaskProgress { task_id } => Some(task_id.clone()),
            EvidenceMode::PocClosure => None,
        },
        source_sha: Some(manifest.source.sha.clone()),
        status: if passed_ok { "PASS" } else { "FAIL" }.to_owned(),
        passed_test_ids: passed.into_iter().collect(),
        missing_test_ids: missing.into_iter().collect(),
        non_pass_test_ids: non_pass.into_iter().collect(),
        requirement_gaps,
        deferred_requirement_ids: ledger.deferred.keys().cloned().collect(),
        errors,
    }
}

fn validate_member_path(path: &str) -> Result<()> {
    let value = Path::new(path);
    ensure!(!value.is_absolute(), "absolute member path {path}");
    ensure!(
        value
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_))),
        "member path contains traversal or platform prefix: {path}"
    );
    ensure!(
        !is_verifier_output(path),
        "manifest lists verifier output {path}"
    );
    ensure!(path != "run-manifest.json", "manifest lists itself");
    Ok(())
}

fn is_verifier_output(path: &str) -> bool {
    matches!(
        path,
        "closure-report.json" | "closure-report.md" | "closure-report.sha256"
    )
}

fn safe_identity(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn is_lower_hex(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn mode_name(mode: &EvidenceMode) -> &'static str {
    match mode {
        EvidenceMode::TaskProgress { .. } => "task_progress",
        EvidenceMode::PocClosure => "poc_closure",
    }
}

fn substantive_json(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
        serde_json::Value::String(value) => !value.trim().is_empty(),
        serde_json::Value::Array(values) => !values.is_empty(),
        serde_json::Value::Object(values) => !values.is_empty(),
    }
}

fn contains_forbidden_key(value: &serde_json::Value, forbidden: &BTreeSet<String>) -> bool {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| contains_forbidden_key(value, forbidden)),
        serde_json::Value::Object(values) => values.iter().any(|(key, value)| {
            forbidden.contains(&key.to_ascii_lowercase())
                || contains_forbidden_key(value, forbidden)
        }),
        _ => false,
    }
}

/// Resolve a manifest inside an evidence directory.
pub fn manifest_in(evidence_dir: &Path) -> PathBuf {
    evidence_dir.join("run-manifest.json")
}

/// Reject a report that did not prove its declared claim.
pub fn require_pass(report: &ClosureReportV1) -> Result<()> {
    if !report.passed() {
        bail!(
            "{} verification failed: missing={:?}, non_pass={:?}, gaps={:?}, errors={:?}",
            report.mode,
            report.missing_test_ids,
            report.non_pass_test_ids,
            report.requirement_gaps,
            report.errors
        );
    }
    Ok(())
}
