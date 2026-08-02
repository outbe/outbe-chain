//! Assembly of one PoC closure from independently verified lane bundles.
//!
//! Lane runners publish immutable manifests first. Closure assembly re-verifies
//! every required execution lane against the checked-out source, copies every
//! retained member into a new namespace, re-hashes the copied bytes and rewrites
//! assertion references to the closure namespace. `OCM-VERIFY` is the verifier
//! invocation over the resulting closure, not a self-attested input lane.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{ensure, Result, WrapErr};
use serde_json::{json, Value};

use super::{
    capture_source_identity, discover, publish_assertions, publish_manifest, publish_member,
    verify_lane_semantics, verify_manifest, AssertionRecordV1, EvidenceMode, MemberDigestV1,
    PlanningLedger, RunManifestV1, RUNTIME_SCHEMA_VERSION,
};

const VERIFIER_LANE: &str = "OCM-VERIFY";

/// Location of a lane manifest inside an OCM-27 evidence root.
#[must_use]
pub fn lane_manifest_in(evidence_root: &Path, lane: &str) -> PathBuf {
    evidence_root
        .join("lanes")
        .join(lane)
        .join("run-manifest.json")
}

/// Location of the assembled closure manifest inside an OCM-27 evidence root.
#[must_use]
pub fn closure_manifest_in(evidence_root: &Path) -> PathBuf {
    evidence_root.join("closure").join("run-manifest.json")
}

/// Verify all required execution lanes and publish one immutable closure.
pub fn assemble_closure(
    repo: &Path,
    ledger: &PlanningLedger,
    evidence_root: &Path,
) -> Result<PathBuf> {
    ledger.validate()?;
    let source = capture_source_identity(repo)?;
    ensure!(
        !source.tracked_dirty && !source.untracked_dirty,
        "PoC closure assembly requires a clean tracked and untracked source tree"
    );

    let output_dir = evidence_root.join("closure");
    ensure!(
        !output_dir.join("run-manifest.json").exists(),
        "closure manifest already exists in {}",
        output_dir.display()
    );

    let required_lanes = required_execution_lanes(ledger)?;
    let run_id = format!("ocomp-poc-closure-{}", unix_millis());
    let mut assertions = Vec::new();
    let mut members = Vec::new();
    let mut lane_index = BTreeMap::new();
    let mut all_test_ids = BTreeSet::new();
    let mut started_at = u64::MAX;
    let mut finished_at = 0_u64;
    let mut exact_binary_identity = None;
    let mut launch_identity = None;
    let mut gramine_image_identity = None;

    for lane in required_lanes {
        let manifest_path = lane_manifest_in(evidence_root, &lane);
        let report = verify_manifest(repo, ledger, &manifest_path)
            .wrap_err_with(|| format!("independently verify required lane {lane}"))?;
        ensure!(report.passed(), "required lane {lane} did not PASS");
        verify_lane_semantics(repo, ledger, &manifest_path)
            .wrap_err_with(|| format!("recompute retained semantics for lane {lane}"))?;

        let manifest = decode_manifest(&manifest_path)?;
        ensure!(
            manifest.source == source,
            "lane {lane} belongs to another exact source/toolchain identity"
        );
        ensure!(
            matches!(&manifest.mode, EvidenceMode::Lane { lane: actual } if actual == &lane),
            "manifest {} does not claim lane {lane}",
            manifest_path.display()
        );
        let lane_binaries = manifest
            .sections
            .get("exact_binary_hashes")
            .filter(|value| value.is_object())
            .ok_or_else(|| eyre::eyre!("lane {lane} lacks exact binary identity"))?;
        match &exact_binary_identity {
            Some(expected) => ensure!(
                expected == lane_binaries,
                "lane {lane} used another exact binary artifact set"
            ),
            None => exact_binary_identity = Some(lane_binaries.clone()),
        }
        if matches!(lane.as_str(), "OCM-PUBLIC" | "OCM-E2E") {
            let lane_launch = manifest
                .sections
                .get("genesis_fork_bundle_and_profiles")
                .filter(|value| value.is_object())
                .ok_or_else(|| eyre::eyre!("lane {lane} lacks exact launch identity"))?;
            match &launch_identity {
                Some(expected) => ensure!(
                    expected == lane_launch,
                    "lane {lane} used another genesis/fork/bundle identity"
                ),
                None => launch_identity = Some(lane_launch.clone()),
            }
            retain_scenario_image_identity(&mut gramine_image_identity, &lane, &manifest.sections)?;
        }

        let lane_assertions = decode_assertions(&manifest_path, &manifest)?;
        let expected_test_ids = ledger.lane_test_ids(&lane);
        let actual_test_ids = lane_assertions
            .iter()
            .map(|assertion| assertion.test_id.clone())
            .collect::<BTreeSet<_>>();
        ensure!(
            actual_test_ids == expected_test_ids
                && lane_assertions.len() == expected_test_ids.len(),
            "lane {lane} must contain exactly one assertion for every ledger test"
        );
        ensure!(
            all_test_ids.is_disjoint(&actual_test_ids),
            "lane {lane} repeats a stable test owned by another lane"
        );
        all_test_ids.extend(actual_test_ids);

        let lane_prefix = format!("lanes/{lane}");
        let lane_root = manifest_path
            .parent()
            .ok_or_else(|| eyre::eyre!("lane manifest has no parent directory"))?;
        for member in &manifest.members {
            members.push(copy_member(
                &lane_root.join(&member.path),
                &output_dir,
                &format!("{lane_prefix}/{}", member.path),
            )?);
        }
        let lane_manifest_member = copy_member(
            &manifest_path,
            &output_dir,
            &format!("{lane_prefix}/run-manifest.json"),
        )?;
        let lane_manifest_sha256 = lane_manifest_member.sha256.clone();
        members.push(lane_manifest_member);

        assertions.extend(rewrite_assertions(
            &lane,
            &lane_prefix,
            &run_id,
            &source.sha,
            lane_assertions,
        ));

        started_at = started_at.min(manifest.started_at);
        finished_at = finished_at.max(manifest.finished_at);
        lane_index.insert(
            lane.clone(),
            json!({
                "manifest": format!("{lane_prefix}/run-manifest.json"),
                "manifest_sha256": lane_manifest_sha256,
                "run_id": manifest.run_id,
                "started_at": manifest.started_at,
                "finished_at": manifest.finished_at,
                "test_ids": expected_test_ids,
                "status": report.status,
            }),
        );
    }

    ensure!(
        all_test_ids == ledger.test_ids(),
        "required execution lanes do not partition every stable test"
    );
    ensure!(
        started_at != u64::MAX && finished_at >= started_at,
        "required lanes produced no valid run interval"
    );
    assertions.sort_by(|left, right| left.assertion_id.cmp(&right.assertion_id));
    let assertions_path = "assertions.jsonl";
    members.push(publish_assertions(
        &output_dir,
        assertions_path,
        &assertions,
    )?);
    members.sort_by(|left, right| left.path.cmp(&right.path));

    let discovery = discover(repo, ledger)?;
    let exact_binary_identity = exact_binary_identity
        .ok_or_else(|| eyre::eyre!("closure has no exact binary artifact identity"))?;
    let launch_identity =
        launch_identity.ok_or_else(|| eyre::eyre!("closure has no exact launch identity"))?;
    let gramine_image_identity = gramine_image_identity
        .ok_or_else(|| eyre::eyre!("closure has no Gramine Docker image identity"))?;
    let sections = closure_sections(
        ledger,
        &source,
        &discovery,
        ClosureIdentity {
            exact_binaries: &exact_binary_identity,
            launch: &launch_identity,
            gramine_image: &gramine_image_identity,
        },
        &lane_index,
        &members,
    );
    let finished_at = unix_millis().max(finished_at);
    publish_manifest(
        &output_dir,
        &RunManifestV1 {
            schema_version: RUNTIME_SCHEMA_VERSION,
            run_id,
            mode: EvidenceMode::PocClosure,
            started_at,
            finished_at,
            source,
            discovery,
            assertions_path: assertions_path.to_owned(),
            members,
            sections,
        },
    )
}

/// Re-open every embedded lane and independently recompute a retained closure.
pub fn verify_closure_semantics(
    repo: &Path,
    ledger: &PlanningLedger,
    manifest_path: &Path,
) -> Result<()> {
    ledger.validate()?;
    let manifest = decode_manifest(manifest_path)?;
    ensure!(
        matches!(manifest.mode, EvidenceMode::PocClosure),
        "closure semantic verification requires poc_closure mode"
    );
    let source = capture_source_identity(repo)?;
    ensure!(
        manifest.source == source && !source.tracked_dirty && !source.untracked_dirty,
        "retained closure differs from the clean verifier source identity"
    );
    let closure_root = manifest_path
        .parent()
        .ok_or_else(|| eyre::eyre!("closure manifest has no parent directory"))?;
    let mut expected_assertions = Vec::new();
    let mut lane_index = BTreeMap::new();
    let mut all_test_ids = BTreeSet::new();
    let mut exact_binary_identity = None;
    let mut launch_identity = None;
    let mut gramine_image_identity = None;
    let mut minimum_started_at = u64::MAX;
    let mut maximum_finished_at = 0_u64;

    for lane in required_execution_lanes(ledger)? {
        let embedded_manifest_path = closure_root
            .join("lanes")
            .join(&lane)
            .join("run-manifest.json");
        let report = verify_manifest(repo, ledger, &embedded_manifest_path)
            .wrap_err_with(|| format!("verify embedded lane {lane}"))?;
        ensure!(report.passed(), "embedded lane {lane} did not PASS");
        verify_lane_semantics(repo, ledger, &embedded_manifest_path)
            .wrap_err_with(|| format!("recompute embedded lane {lane}"))?;
        let embedded = decode_manifest(&embedded_manifest_path)?;
        ensure!(
            matches!(&embedded.mode, EvidenceMode::Lane { lane: actual } if actual == &lane)
                && embedded.source == source,
            "embedded lane {lane} has the wrong mode or source"
        );

        let lane_assertions = decode_assertions(&embedded_manifest_path, &embedded)?;
        let expected_test_ids = ledger.lane_test_ids(&lane);
        let actual_test_ids = lane_assertions
            .iter()
            .map(|assertion| assertion.test_id.clone())
            .collect::<BTreeSet<_>>();
        ensure!(
            actual_test_ids == expected_test_ids
                && lane_assertions.len() == expected_test_ids.len()
                && all_test_ids.is_disjoint(&actual_test_ids),
            "embedded lane {lane} does not contain one unique assertion per owned test"
        );
        all_test_ids.extend(actual_test_ids);
        expected_assertions.extend(rewrite_assertions(
            &lane,
            &format!("lanes/{lane}"),
            &manifest.run_id,
            &source.sha,
            lane_assertions,
        ));

        let lane_binaries = embedded
            .sections
            .get("exact_binary_hashes")
            .filter(|value| value.is_object())
            .ok_or_else(|| eyre::eyre!("embedded lane {lane} lacks binary identity"))?;
        match &exact_binary_identity {
            Some(expected) => ensure!(
                expected == lane_binaries,
                "embedded lane {lane} used another artifact set"
            ),
            None => exact_binary_identity = Some(lane_binaries.clone()),
        }
        if matches!(lane.as_str(), "OCM-PUBLIC" | "OCM-E2E") {
            let lane_launch = embedded
                .sections
                .get("genesis_fork_bundle_and_profiles")
                .filter(|value| value.is_object())
                .ok_or_else(|| eyre::eyre!("embedded lane {lane} lacks launch identity"))?;
            match &launch_identity {
                Some(expected) => ensure!(
                    expected == lane_launch,
                    "embedded lane {lane} used another launch identity"
                ),
                None => launch_identity = Some(lane_launch.clone()),
            }
            retain_scenario_image_identity(&mut gramine_image_identity, &lane, &embedded.sections)?;
        }

        let relative_manifest = format!("lanes/{lane}/run-manifest.json");
        let manifest_digest = super::hash_file(&embedded_manifest_path)?;
        lane_index.insert(
            lane,
            json!({
                "manifest": relative_manifest,
                "manifest_sha256": manifest_digest.sha256,
                "run_id": embedded.run_id,
                "started_at": embedded.started_at,
                "finished_at": embedded.finished_at,
                "test_ids": expected_test_ids,
                "status": report.status,
            }),
        );
        minimum_started_at = minimum_started_at.min(embedded.started_at);
        maximum_finished_at = maximum_finished_at.max(embedded.finished_at);
    }
    ensure!(
        all_test_ids == ledger.test_ids()
            && manifest.started_at == minimum_started_at
            && manifest.finished_at >= maximum_finished_at,
        "closure lane partition or aggregate interval is incorrect"
    );

    expected_assertions.sort_by(|left, right| left.assertion_id.cmp(&right.assertion_id));
    let mut actual_assertions = decode_assertions(manifest_path, &manifest)?;
    actual_assertions.sort_by(|left, right| left.assertion_id.cmp(&right.assertion_id));
    ensure!(
        actual_assertions == expected_assertions,
        "closure assertions differ from independently rewritten lane assertions"
    );

    let exact_binary_identity = exact_binary_identity
        .ok_or_else(|| eyre::eyre!("retained closure has no binary identity"))?;
    let launch_identity =
        launch_identity.ok_or_else(|| eyre::eyre!("retained closure has no launch identity"))?;
    let gramine_image_identity = gramine_image_identity
        .ok_or_else(|| eyre::eyre!("retained closure has no Gramine Docker image identity"))?;
    let expected_sections = closure_sections(
        ledger,
        &source,
        &manifest.discovery,
        ClosureIdentity {
            exact_binaries: &exact_binary_identity,
            launch: &launch_identity,
            gramine_image: &gramine_image_identity,
        },
        &lane_index,
        &manifest.members,
    );
    ensure!(
        manifest.sections == expected_sections,
        "closure sections differ from independently recomputed lane evidence"
    );
    Ok(())
}

/// Dispatch independent retained-member verification by manifest mode.
pub fn verify_retained_semantics(
    repo: &Path,
    ledger: &PlanningLedger,
    manifest_path: &Path,
) -> Result<()> {
    let manifest = decode_manifest(manifest_path)?;
    match manifest.mode {
        EvidenceMode::Lane { .. } => verify_lane_semantics(repo, ledger, manifest_path),
        EvidenceMode::PocClosure => verify_closure_semantics(repo, ledger, manifest_path),
        EvidenceMode::TaskProgress { .. } => {
            eyre::bail!("task_progress is a report mode, not a retained evidence bundle")
        }
    }
}

fn required_execution_lanes(ledger: &PlanningLedger) -> Result<Vec<String>> {
    ensure!(
        ledger
            .lanes
            .get(VERIFIER_LANE)
            .is_some_and(|lane| lane.required),
        "{VERIFIER_LANE} must remain a required closure lane"
    );
    let lanes = ledger
        .lanes
        .iter()
        .filter(|(lane, definition)| definition.required && lane.as_str() != VERIFIER_LANE)
        .map(|(lane, _)| lane.clone())
        .collect::<Vec<_>>();
    ensure!(
        !lanes.is_empty(),
        "planning ledger has no required execution lanes"
    );
    for lane in &lanes {
        ensure!(
            !ledger.lane_test_ids(lane).is_empty(),
            "required execution lane {lane} owns no stable tests"
        );
    }
    Ok(lanes)
}

fn decode_manifest(path: &Path) -> Result<RunManifestV1> {
    serde_json::from_slice(
        &std::fs::read(path).wrap_err_with(|| format!("read lane manifest {}", path.display()))?,
    )
    .wrap_err_with(|| format!("decode lane manifest {}", path.display()))
}

fn decode_assertions(
    manifest_path: &Path,
    manifest: &RunManifestV1,
) -> Result<Vec<AssertionRecordV1>> {
    let root = manifest_path
        .parent()
        .ok_or_else(|| eyre::eyre!("lane manifest has no parent directory"))?;
    let text = std::fs::read_to_string(root.join(&manifest.assertions_path))
        .wrap_err("read lane assertions")?;
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            ensure!(
                !line.trim().is_empty(),
                "blank lane assertion line {}",
                index + 1
            );
            serde_json::from_str(line)
                .wrap_err_with(|| format!("decode lane assertion line {}", index + 1))
        })
        .collect()
}

fn copy_member(source: &Path, output_dir: &Path, relative: &str) -> Result<MemberDigestV1> {
    let bytes = std::fs::read(source)
        .wrap_err_with(|| format!("read retained member {}", source.display()))?;
    publish_member(output_dir, relative, &bytes)
}

fn prefix_refs(references: &mut [String], prefix: &str) {
    for reference in references {
        *reference = format!("{prefix}/{reference}");
    }
}

fn rewrite_assertions(
    lane: &str,
    lane_prefix: &str,
    run_id: &str,
    source_sha: &str,
    assertions: Vec<AssertionRecordV1>,
) -> Vec<AssertionRecordV1> {
    assertions
        .into_iter()
        .map(|mut assertion| {
            assertion.assertion_id = format!("closure-{lane}-{}", assertion.assertion_id);
            assertion.run_id = run_id.to_owned();
            assertion.source_sha = source_sha.to_owned();
            prefix_refs(&mut assertion.expected_artifact_refs, lane_prefix);
            prefix_refs(&mut assertion.actual_artifact_refs, lane_prefix);
            assertion
        })
        .collect()
}

fn retain_scenario_image_identity(
    retained: &mut Option<Value>,
    lane: &str,
    sections: &BTreeMap<String, Value>,
) -> Result<()> {
    let identity = sections
        .get("exact_config_and_service_unit_hashes")
        .and_then(Value::as_object)
        .and_then(|section| section.get("gramine_image_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| eyre::eyre!("lane {lane} lacks Gramine Docker image identity"))?;
    crate::internal::proc::DockerImageId::from_inspect_output(identity)
        .wrap_err_with(|| format!("lane {lane} has invalid Gramine Docker image identity"))?;
    let identity = Value::String(identity.to_owned());
    match retained {
        Some(expected) => ensure!(
            expected == &identity,
            "lane {lane} used another Gramine Docker image"
        ),
        None => *retained = Some(identity),
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ClosureIdentity<'a> {
    exact_binaries: &'a Value,
    launch: &'a Value,
    gramine_image: &'a Value,
}

fn closure_sections(
    ledger: &PlanningLedger,
    source: &super::SourceIdentityV1,
    discovery: &super::TestDiscoveryV1,
    identity: ClosureIdentity<'_>,
    lane_index: &BTreeMap<String, Value>,
    members: &[MemberDigestV1],
) -> BTreeMap<String, Value> {
    ledger
        .runtime_evidence
        .required_sections
        .iter()
        .map(|section| {
            let value = match section.as_str() {
                "source_and_toolchain" => json!(source),
                "exact_binary_hashes" => identity.exact_binaries.clone(),
                "exact_config_and_service_unit_hashes" => json!({
                    "launch_identity": identity.launch,
                    "gramine_image_id": identity.gramine_image,
                    "verified_lane_manifests": lane_index,
                }),
                "genesis_fork_bundle_and_profiles" => identity.launch.clone(),
                "test_discovery" => json!(discovery),
                "member_hash_index_and_retention" => json!({
                    "publish_last": true,
                    "members": members,
                }),
                "skip_todo_quarantine_retry_and_timeout_records" => json!({
                    "skipped": &discovery.skipped,
                    "todo": &discovery.todo,
                    "quarantined": &discovery.quarantined,
                    "retried": &discovery.retried,
                    "automatic_retries": 0,
                }),
                _ => json!({
                    "verified_lane_manifests": lane_index,
                    "verifier_lane": VERIFIER_LANE,
                }),
            };
            (section.clone(), value)
        })
        .collect()
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{required_execution_lanes, retain_scenario_image_identity, rewrite_assertions};
    use crate::ocomp_evidence::{AssertionRecordV1, AssertionStatus, PlanningLedger};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn ledger() -> PlanningLedger {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        PlanningLedger::parse(&root.join("outbe-plan/off-chain-poc-evidence-ledger.yaml"))
            .expect("checked-in planning ledger")
    }

    #[test]
    fn closure_requires_every_execution_lane_but_not_a_self_attested_verify_input() {
        assert_eq!(
            required_execution_lanes(&ledger()).expect("required lanes"),
            ["OCM-E2E", "OCM-FAST", "OCM-INT", "OCM-PUBLIC"]
        );
    }

    #[test]
    fn closure_requires_one_immutable_gramine_image_across_scenario_lanes() {
        let sections = |image_id: &str| {
            BTreeMap::from([(
                "exact_config_and_service_unit_hashes".to_owned(),
                json!({"gramine_image_id": image_id}),
            )])
        };
        let first = format!("sha256:{}", "ab".repeat(32));
        let second = format!("sha256:{}", "cd".repeat(32));
        let mut retained = None;

        retain_scenario_image_identity(&mut retained, "OCM-PUBLIC", &sections(&first))
            .expect("first scenario lane establishes image identity");
        retain_scenario_image_identity(&mut retained, "OCM-E2E", &sections(&first))
            .expect("same image identity is accepted");
        assert!(
            retain_scenario_image_identity(&mut retained, "OCM-E2E", &sections(&second)).is_err()
        );
        assert!(
            retain_scenario_image_identity(&mut retained, "OCM-E2E", &BTreeMap::new()).is_err()
        );
    }

    #[test]
    fn closure_rebinds_assertions_and_every_artifact_reference() {
        let rewritten = rewrite_assertions(
            "OCM-FAST",
            "lanes/OCM-FAST",
            "closure-run",
            "aabb",
            vec![AssertionRecordV1 {
                assertion_id: "lane-assertion".to_owned(),
                test_id: "OCM-EVD-001".to_owned(),
                status: AssertionStatus::Pass,
                oracle: "CANONICAL_BYTES".to_owned(),
                expected_artifact_refs: vec!["expected.json".to_owned()],
                actual_artifact_refs: vec![
                    "commands/test.stdout".to_owned(),
                    "commands/test.stderr".to_owned(),
                ],
                observed_at: 10,
                run_id: "lane-run".to_owned(),
                source_sha: "old".to_owned(),
                attempt: 1,
            }],
        );
        assert_eq!(rewritten.len(), 1);
        let assertion = &rewritten[0];
        assert_eq!(assertion.assertion_id, "closure-OCM-FAST-lane-assertion");
        assert_eq!(assertion.run_id, "closure-run");
        assert_eq!(assertion.source_sha, "aabb");
        assert_eq!(
            assertion.expected_artifact_refs,
            ["lanes/OCM-FAST/expected.json"]
        );
        assert_eq!(
            assertion.actual_artifact_refs,
            [
                "lanes/OCM-FAST/commands/test.stdout",
                "lanes/OCM-FAST/commands/test.stderr",
            ]
        );
    }
}
