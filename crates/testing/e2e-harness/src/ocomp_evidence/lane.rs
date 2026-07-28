//! Independent aggregation of completed OCOMP lane scenario records.
//!
//! Cucumber steps exercise the product and publish observational scenario JSON.
//! This module does not trust the scenario name alone: it re-checks the public
//! receipts, heights, vote slots, balances and terminal state needed by each
//! stable lane ID before publishing an assertion.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{ensure, Result, WrapErr};
use serde_json::{json, Value};

use super::command_lane::verify_command_lane_manifest_semantics;
use super::{
    assemble_command_lane, capture_source_identity, discover, hash_file, publish_assertions,
    publish_manifest, publish_member, AssertionRecordV1, AssertionStatus, EvidenceMode,
    MemberDigestV1, PlanningLedger, RunManifestV1, RUNTIME_SCHEMA_VERSION,
};

const REQUIRED_SCENARIO_BINARIES: [&str; 6] = [
    "outbe_chain",
    "outbe_cli",
    "outbe_e2e",
    "outbe_keygen",
    "outbe_ocomp",
    "outbe_tee_enclave",
];

const PUBLIC_SCENARIOS: [(&str, &str, &str); 4] = [
    (
        "OCM-PUB-001",
        "Four independent domains certify and atomically apply one public Lysis result",
        "PUBLIC_TX_RECEIPT",
    ),
    (
        "OCM-PUB-002",
        "A changed binding cannot mutate a non-quorum job or prevent exact recovery",
        "STATE_ROOT_DIFF",
    ),
    (
        "OCM-PUB-003",
        "Two timely votes cannot prevent exclusive-deadline expiry",
        "FINALIZED_PUBLIC_STATE",
    ),
    (
        "OCM-PUB-004",
        "Four independent domains certify and atomically apply one public Lysis result",
        "FINALIZED_PUBLIC_STATE",
    ),
];

const E2E_SCENARIOS: [(&str, &str, &str); 4] = [
    (
        "OCM-E2E-001",
        "Final public Tribute flows through four independent domains to certified Nod",
        "FINALIZED_PUBLIC_STATE",
    ),
    (
        "OCM-TRC-001",
        "Final public Tribute flows through four independent domains to certified Nod",
        "RUNTIME_BOUNDARY_TRACE",
    ),
    (
        "OCM-E2E-007",
        "An incompatible Supervisor cannot affect consensus or the other validator domains",
        "PROCESS_BOUNDARY",
    ),
    (
        "OCM-E2E-008",
        "A completed generation survives node and compute-process restart and replay",
        "FINALIZED_PUBLIC_STATE",
    ),
];

const ISO_SCENARIO: (&str, &str, &str) = (
    "OCM-ISO-001",
    "Four validator domains retain node liveness under bounded compute faults",
    "PROCESS_BOUNDARY",
);

/// Validate and publish one lane manifest from already completed scenario
/// records. The caller is responsible for running the lane exactly once.
pub fn assemble_lane(
    repo: &Path,
    ledger: &PlanningLedger,
    lane: &str,
    evidence_dir: &Path,
) -> Result<PathBuf> {
    match lane {
        "OCM-FAST" | "OCM-INT" => assemble_command_lane(repo, ledger, lane, evidence_dir),
        "OCM-PUBLIC" => assemble_public_lane(repo, ledger, evidence_dir),
        "OCM-E2E" => assemble_e2e_lane(repo, ledger, evidence_dir),
        "OCM-ISO" => assemble_iso_lane(repo, ledger, evidence_dir),
        _ => eyre::bail!("lane assembly is not implemented for {lane}"),
    }
}

/// Recompute lane-specific semantics from retained members without trusting
/// the PASS assertions emitted during lane assembly.
pub fn verify_lane_semantics(
    repo: &Path,
    ledger: &PlanningLedger,
    manifest_path: &Path,
) -> Result<()> {
    let manifest: RunManifestV1 = serde_json::from_slice(
        &std::fs::read(manifest_path)
            .wrap_err_with(|| format!("read lane manifest {}", manifest_path.display()))?,
    )
    .wrap_err("decode lane manifest for semantic verification")?;
    let lane = match &manifest.mode {
        EvidenceMode::Lane { lane } => lane.as_str(),
        _ => eyre::bail!("semantic lane verification requires lane mode"),
    };
    let evidence_dir = manifest_path
        .parent()
        .ok_or_else(|| eyre::eyre!("lane manifest has no parent directory"))?;
    match lane {
        "OCM-FAST" | "OCM-INT" => {
            verify_command_lane_manifest_semantics(repo, ledger, lane, &manifest, evidence_dir)
        }
        "OCM-PUBLIC" => {
            verify_scenario_lane(repo, &manifest, evidence_dir, &PUBLIC_SCENARIOS, false)
        }
        "OCM-E2E" => verify_scenario_lane(repo, &manifest, evidence_dir, &E2E_SCENARIOS, false),
        "OCM-ISO" => verify_scenario_lane(repo, &manifest, evidence_dir, &[ISO_SCENARIO], true),
        _ => eyre::bail!("semantic lane verification is not implemented for {lane}"),
    }
}

fn verify_scenario_lane(
    repo: &Path,
    manifest: &RunManifestV1,
    evidence_dir: &Path,
    expected_scenarios: &[(&str, &str, &str)],
    isolation: bool,
) -> Result<()> {
    let source = capture_source_identity(repo)?;
    ensure!(
        manifest.source == source,
        "scenario lane belongs to another source/toolchain identity"
    );
    let scenarios = load_scenarios(evidence_dir)?;
    let identity = scenario_run_identity(&scenarios)?;
    validate_scenario_manifest_identity(&manifest.sections, &identity)?;

    let mut assertions = read_lane_assertions(evidence_dir, manifest)?
        .into_iter()
        .map(|assertion| (assertion.test_id.clone(), assertion))
        .collect::<BTreeMap<_, _>>();
    ensure!(
        assertions.len() == expected_scenarios.len(),
        "scenario lane has duplicate or unexpected assertion count"
    );
    let mut used_scenarios = BTreeSet::new();
    for (test_id, scenario_name, oracle) in expected_scenarios {
        let (scenario_path, scenario) = scenarios
            .get(*scenario_name)
            .ok_or_else(|| eyre::eyre!("missing retained scenario {scenario_name}"))?;
        used_scenarios.insert(*scenario_name);
        validate_common_scenario(scenario, &source.sha)?;
        match *test_id {
            "OCM-ISO-001" => {
                validate_applied_public_path(scenario)?;
                validate_isolation_scenario(scenario)?;
            }
            id if id.starts_with("OCM-PUB-") => validate_public_scenario(id, scenario)?,
            id => validate_e2e_scenario(id, scenario)?,
        }
        let assertion = assertions
            .remove(*test_id)
            .ok_or_else(|| eyre::eyre!("missing assertion for {test_id}"))?;
        ensure!(
            assertion.status == AssertionStatus::Pass
                && assertion.oracle == *oracle
                && assertion.expected_artifact_refs == [format!("expected/{test_id}.json")]
                && assertion.actual_artifact_refs == [scenario_path.clone()]
                && assertion.observed_at == u64_field(scenario, "recorded_at_unix_ms")?,
            "scenario assertion {test_id} is not bound to its exact retained observation"
        );
    }
    ensure!(
        assertions.is_empty(),
        "scenario lane contains assertions outside its closed scenario table"
    );
    if isolation || expected_scenarios == E2E_SCENARIOS {
        ensure!(
            used_scenarios.len() == scenarios.len(),
            "scenario lane contains unexpected retained scenarios"
        );
    } else {
        ensure!(
            scenarios
                .iter()
                .filter(|(name, _)| !used_scenarios.contains(name.as_str()))
                .map(|(_, value)| value)
                .all(
                    |(_, scenario)| scenario.get("result").and_then(Value::as_str)
                        == Some("passed")
                ),
            "public lane contains a non-PASS auxiliary scenario"
        );
    }
    Ok(())
}

fn read_lane_assertions(
    evidence_dir: &Path,
    manifest: &RunManifestV1,
) -> Result<Vec<AssertionRecordV1>> {
    let text = std::fs::read_to_string(evidence_dir.join(&manifest.assertions_path))
        .wrap_err("read retained lane assertions")?;
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            ensure!(
                !line.trim().is_empty(),
                "blank retained assertion line {}",
                index + 1
            );
            serde_json::from_str(line)
                .wrap_err_with(|| format!("decode retained assertion line {}", index + 1))
        })
        .collect()
}

fn assemble_public_lane(
    repo: &Path,
    ledger: &PlanningLedger,
    evidence_dir: &Path,
) -> Result<PathBuf> {
    let lane = "OCM-PUBLIC";
    ensure!(
        !evidence_dir.join("run-manifest.json").exists(),
        "lane manifest already exists in {}",
        evidence_dir.display()
    );
    let source = capture_source_identity(repo)?;
    let scenarios = load_scenarios(evidence_dir)?;
    let scenario_identity = scenario_run_identity(&scenarios)?;
    let mut members = scenario_members(evidence_dir, &scenarios)?;
    let mut assertions = Vec::with_capacity(PUBLIC_SCENARIOS.len());
    let mut observed_times = Vec::with_capacity(PUBLIC_SCENARIOS.len());

    let run_id = format!("ocomp-public-{}", unix_millis());
    let mut used_scenarios = BTreeSet::new();
    for (test_id, scenario_name, oracle) in PUBLIC_SCENARIOS {
        let (path, scenario) = scenarios
            .get(scenario_name)
            .ok_or_else(|| eyre::eyre!("missing required public scenario {scenario_name}"))?;
        used_scenarios.insert(scenario_name);
        validate_common_scenario(scenario, &source.sha)?;
        validate_public_scenario(test_id, scenario)?;

        let expected_path = format!("expected/{test_id}.json");
        let expected = serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "test_id": test_id,
            "scenario": scenario_name,
            "oracle": oracle,
            "required_result": "passed",
        }))?;
        members.push(publish_member(evidence_dir, &expected_path, &expected)?);
        let observed_at = u64_field(scenario, "recorded_at_unix_ms")?;
        observed_times.push(observed_at);
        assertions.push(AssertionRecordV1 {
            assertion_id: format!("{run_id}-{test_id}"),
            test_id: test_id.to_owned(),
            status: AssertionStatus::Pass,
            oracle: oracle.to_owned(),
            expected_artifact_refs: vec![expected_path],
            actual_artifact_refs: vec![path.clone()],
            observed_at,
            run_id: run_id.clone(),
            source_sha: source.sha.clone(),
            attempt: 1,
        });
    }
    ensure!(
        scenarios
            .iter()
            .filter(|(name, _)| !used_scenarios.contains(name.as_str()))
            .map(|(_, value)| value)
            .all(|(_, scenario)| scenario.get("result").and_then(Value::as_str) == Some("passed")),
        "the public run contains a non-PASS auxiliary scenario"
    );

    let assertions_path = "assertions.jsonl";
    members.push(publish_assertions(
        evidence_dir,
        assertions_path,
        &assertions,
    )?);
    members.sort_by(|left, right| left.path.cmp(&right.path));
    let discovery = discover(repo, ledger)?;
    let started_at = observed_times
        .iter()
        .copied()
        .min()
        .unwrap_or_else(unix_millis);
    let finished_at = unix_millis().max(observed_times.iter().copied().max().unwrap_or(started_at));
    let scenario_paths = members
        .iter()
        .filter(|member| member.path.starts_with("scenario-"))
        .map(|member| member.path.clone())
        .collect::<Vec<_>>();
    let sections = scenario_sections(
        ledger,
        lane,
        &source,
        &discovery,
        &scenario_identity,
        &scenario_paths,
        assertions.len(),
        &members,
        false,
    );
    let manifest = RunManifestV1 {
        schema_version: RUNTIME_SCHEMA_VERSION,
        run_id,
        mode: EvidenceMode::Lane {
            lane: lane.to_owned(),
        },
        started_at,
        finished_at,
        source,
        discovery,
        assertions_path: assertions_path.to_owned(),
        members,
        sections,
    };
    publish_manifest(evidence_dir, &manifest)
}

fn assemble_e2e_lane(repo: &Path, ledger: &PlanningLedger, evidence_dir: &Path) -> Result<PathBuf> {
    let lane = "OCM-E2E";
    ensure!(
        !evidence_dir.join("run-manifest.json").exists(),
        "lane manifest already exists in {}",
        evidence_dir.display()
    );
    let source = capture_source_identity(repo)?;
    let scenarios = load_scenarios(evidence_dir)?;
    let scenario_identity = scenario_run_identity(&scenarios)?;
    let mut members = scenario_members(evidence_dir, &scenarios)?;
    let mut assertions = Vec::with_capacity(E2E_SCENARIOS.len());
    let mut observed_times = Vec::with_capacity(E2E_SCENARIOS.len());
    let run_id = format!("ocomp-e2e-{}", unix_millis());

    let mut used_scenarios = BTreeSet::new();
    for (test_id, scenario_name, oracle) in E2E_SCENARIOS {
        let (path, scenario) = scenarios
            .get(scenario_name)
            .ok_or_else(|| eyre::eyre!("missing required E2E scenario {scenario_name}"))?;
        used_scenarios.insert(scenario_name);
        validate_common_scenario(scenario, &source.sha)?;
        validate_e2e_scenario(test_id, scenario)?;

        let expected_path = format!("expected/{test_id}.json");
        let expected = serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "test_id": test_id,
            "scenario": scenario_name,
            "oracle": oracle,
            "required_result": "passed",
        }))?;
        members.push(publish_member(evidence_dir, &expected_path, &expected)?);
        let observed_at = u64_field(scenario, "recorded_at_unix_ms")?;
        observed_times.push(observed_at);
        assertions.push(AssertionRecordV1 {
            assertion_id: format!("{run_id}-{test_id}"),
            test_id: test_id.to_owned(),
            status: AssertionStatus::Pass,
            oracle: oracle.to_owned(),
            expected_artifact_refs: vec![expected_path],
            actual_artifact_refs: vec![path.clone()],
            observed_at,
            run_id: run_id.clone(),
            source_sha: source.sha.clone(),
            attempt: 1,
        });
    }
    ensure!(
        used_scenarios.len() == scenarios.len(),
        "the E2E evidence directory contains unexpected scenarios: {:?}",
        scenarios
            .keys()
            .filter(|name| !used_scenarios.contains(name.as_str()))
            .collect::<Vec<_>>()
    );

    let assertions_path = "assertions.jsonl";
    members.push(publish_assertions(
        evidence_dir,
        assertions_path,
        &assertions,
    )?);
    members.sort_by(|left, right| left.path.cmp(&right.path));
    let discovery = discover(repo, ledger)?;
    let started_at = observed_times
        .iter()
        .copied()
        .min()
        .unwrap_or_else(unix_millis);
    let finished_at = unix_millis().max(observed_times.iter().copied().max().unwrap_or(started_at));
    let scenario_paths = members
        .iter()
        .filter(|member| member.path.starts_with("scenario-"))
        .map(|member| member.path.clone())
        .collect::<Vec<_>>();
    let sections = scenario_sections(
        ledger,
        lane,
        &source,
        &discovery,
        &scenario_identity,
        &scenario_paths,
        assertions.len(),
        &members,
        false,
    );
    publish_manifest(
        evidence_dir,
        &RunManifestV1 {
            schema_version: RUNTIME_SCHEMA_VERSION,
            run_id,
            mode: EvidenceMode::Lane {
                lane: lane.to_owned(),
            },
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

fn assemble_iso_lane(repo: &Path, ledger: &PlanningLedger, evidence_dir: &Path) -> Result<PathBuf> {
    let lane = "OCM-ISO";
    ensure!(
        !evidence_dir.join("run-manifest.json").exists(),
        "lane manifest already exists in {}",
        evidence_dir.display()
    );
    let source = capture_source_identity(repo)?;
    let mut scenarios = load_scenarios(evidence_dir)?;
    let scenario_identity = scenario_run_identity(&scenarios)?;
    let mut members = scenario_members(evidence_dir, &scenarios)?;
    let (test_id, scenario_name, oracle) = ISO_SCENARIO;
    let (scenario_path, scenario) = scenarios
        .remove(scenario_name)
        .ok_or_else(|| eyre::eyre!("missing required ISO scenario {scenario_name}"))?;
    ensure!(
        scenarios.is_empty(),
        "the ISO evidence directory contains unexpected scenarios: {:?}",
        scenarios.keys().collect::<Vec<_>>()
    );
    validate_common_scenario(&scenario, &source.sha)?;
    validate_applied_public_path(&scenario)?;
    validate_isolation_scenario(&scenario)?;

    let run_id = format!("ocomp-iso-{}", unix_millis());
    let expected_path = format!("expected/{test_id}.json");
    let expected = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "test_id": test_id,
        "scenario": scenario_name,
        "oracle": oracle,
        "required_result": "passed",
        "required_runtime": {
            "validator_domains": 4,
            "roles_per_domain": 4,
            "cgroup_version": 2,
            "worker_cap_per_domain": 4,
            "quota_filesystems_per_domain": 4,
            "faults": ["stop_supervisor", "stop_worker"],
            "finality_must_advance": true,
        },
    }))?;
    members.push(publish_member(evidence_dir, &expected_path, &expected)?);
    let observed_at = u64_field(&scenario, "recorded_at_unix_ms")?;
    let assertions_path = "assertions.jsonl";
    members.push(publish_assertions(
        evidence_dir,
        assertions_path,
        &[AssertionRecordV1 {
            assertion_id: format!("{run_id}-{test_id}"),
            test_id: test_id.to_owned(),
            status: AssertionStatus::Pass,
            oracle: oracle.to_owned(),
            expected_artifact_refs: vec![expected_path],
            actual_artifact_refs: vec![scenario_path],
            observed_at,
            run_id: run_id.clone(),
            source_sha: source.sha.clone(),
            attempt: 1,
        }],
    )?);
    members.sort_by(|left, right| left.path.cmp(&right.path));
    let discovery = discover(repo, ledger)?;
    let scenario_paths = members
        .iter()
        .filter(|member| member.path.starts_with("scenario-"))
        .map(|member| member.path.clone())
        .collect::<Vec<_>>();
    let sections = scenario_sections(
        ledger,
        lane,
        &source,
        &discovery,
        &scenario_identity,
        &scenario_paths,
        1,
        &members,
        true,
    );
    publish_manifest(
        evidence_dir,
        &RunManifestV1 {
            schema_version: RUNTIME_SCHEMA_VERSION,
            run_id,
            mode: EvidenceMode::Lane {
                lane: lane.to_owned(),
            },
            started_at: observed_at,
            finished_at: unix_millis().max(observed_at),
            source,
            discovery,
            assertions_path: assertions_path.to_owned(),
            members,
            sections,
        },
    )
}

fn validate_scenario_manifest_identity(
    sections: &BTreeMap<String, Value>,
    identity: &ScenarioRunIdentity,
) -> Result<()> {
    let manifest_image_id = sections
        .get("exact_config_and_service_unit_hashes")
        .and_then(Value::as_object)
        .and_then(|section| section.get("gramine_image_id"));
    ensure!(
        sections.get("exact_binary_hashes") == Some(&identity.exact_binaries)
            && sections.get("genesis_fork_bundle_and_profiles") == Some(&identity.launch_identity)
            && manifest_image_id == Some(&identity.gramine_image_id),
        "scenario lane manifest identity differs from retained scenarios"
    );
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ScenarioRunIdentity {
    exact_binaries: Value,
    launch_identity: Value,
    gramine_image_id: Value,
    effective_systemd_unit_policies: Vec<Value>,
}

fn scenario_run_identity(
    scenarios: &BTreeMap<String, (String, Value)>,
) -> Result<ScenarioRunIdentity> {
    let mut exact_binaries = None;
    let mut launch_identity = None;
    let mut gramine_image_id = None;
    let mut effective_systemd_unit_policies = Vec::new();
    for (scenario_name, (_, scenario)) in scenarios {
        let binaries = path(scenario, &["ocomp", "exact_binaries"])?;
        ensure!(
            binaries.as_object().is_some_and(|value| {
                value.len() == REQUIRED_SCENARIO_BINARIES.len()
                    && REQUIRED_SCENARIO_BINARIES
                        .iter()
                        .all(|name| value.contains_key(*name))
            }),
            "scenario {scenario_name} lacks the exact OCOMP artifact set"
        );
        match &exact_binaries {
            Some(expected) => ensure!(
                expected == binaries,
                "scenario {scenario_name} used another exact binary artifact set"
            ),
            None => exact_binaries = Some(binaries.clone()),
        }

        let launch = path(scenario, &["ocomp", "topology", "launch_identity"])?;
        ensure!(
            launch.is_object(),
            "scenario {scenario_name} lacks exact launch identity"
        );
        match &launch_identity {
            Some(expected) => ensure!(
                expected == launch,
                "scenario {scenario_name} used another genesis/fork/bundle identity"
            ),
            None => launch_identity = Some(launch.clone()),
        }

        let image_id = path(scenario, &["environment", "gramine_image_id"])?;
        let image_id_text = image_id
            .as_str()
            .ok_or_else(|| eyre::eyre!("scenario {scenario_name} lacks Gramine Docker image ID"))?;
        crate::internal::proc::DockerImageId::from_inspect_output(image_id_text).wrap_err_with(
            || format!("scenario {scenario_name} has invalid Gramine Docker image ID"),
        )?;
        match &gramine_image_id {
            Some(expected) => ensure!(
                expected == image_id,
                "scenario {scenario_name} used another Gramine Docker image"
            ),
            None => gramine_image_id = Some(image_id.clone()),
        }

        if let Some(policies) = path(scenario, &["ocomp", "topology", "systemd_isolation"])?
            .as_object()
            .and_then(|isolation| isolation.get("unit_policies"))
            .and_then(Value::as_array)
        {
            effective_systemd_unit_policies.extend(policies.iter().cloned());
        }
    }
    Ok(ScenarioRunIdentity {
        exact_binaries: exact_binaries
            .ok_or_else(|| eyre::eyre!("scenario set has no exact binary identity"))?,
        launch_identity: launch_identity
            .ok_or_else(|| eyre::eyre!("scenario set has no exact launch identity"))?,
        gramine_image_id: gramine_image_id
            .ok_or_else(|| eyre::eyre!("scenario set has no Gramine Docker image identity"))?,
        effective_systemd_unit_policies,
    })
}

#[allow(clippy::too_many_arguments)]
fn scenario_sections(
    ledger: &PlanningLedger,
    lane: &str,
    source: &super::SourceIdentityV1,
    discovery: &super::TestDiscoveryV1,
    identity: &ScenarioRunIdentity,
    scenario_paths: &[String],
    assertion_count: usize,
    members: &[MemberDigestV1],
    isolation: bool,
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
                    "launch_identity": &identity.launch_identity,
                    "gramine_image_id": &identity.gramine_image_id,
                    "effective_systemd_unit_policies":
                        &identity.effective_systemd_unit_policies,
                }),
                "genesis_fork_bundle_and_profiles" => identity.launch_identity.clone(),
                "test_discovery" => json!(discovery),
                "machine_and_service_topology" => json!({
                    "validated_scenarios": scenario_paths,
                    "systemd_isolation": isolation,
                    "gramine_image_id": &identity.gramine_image_id,
                }),
                "skip_todo_quarantine_retry_and_timeout_records" => json!({
                    "automatic_retries": 0,
                    "skipped": &discovery.skipped,
                    "todo": &discovery.todo,
                    "quarantined": &discovery.quarantined,
                    "retried": &discovery.retried,
                }),
                "member_hash_index_and_retention" => json!({
                    "publish_last": true,
                    "members": members,
                }),
                _ => json!({
                    "lane": lane,
                    "validated_scenarios": scenario_paths,
                    "assertion_count": assertion_count,
                    "process_boundary": isolation,
                    "resource_counter": isolation,
                }),
            };
            (section.clone(), value)
        })
        .collect()
}

fn load_scenarios(evidence_dir: &Path) -> Result<BTreeMap<String, (String, Value)>> {
    let mut scenarios = BTreeMap::new();
    for entry in std::fs::read_dir(evidence_dir)
        .wrap_err_with(|| format!("read evidence directory {}", evidence_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("scenario-") || !name.ends_with(".json") {
            continue;
        }
        let value: Value = serde_json::from_slice(&std::fs::read(entry.path())?)
            .wrap_err_with(|| format!("decode scenario evidence {name}"))?;
        let scenario = string_field(&value, "scenario")?.to_owned();
        ensure!(
            scenarios.insert(scenario.clone(), (name, value)).is_none(),
            "duplicate scenario evidence for {scenario}"
        );
    }
    ensure!(
        !scenarios.is_empty(),
        "evidence directory has no scenario records"
    );
    Ok(scenarios)
}

fn scenario_members(
    evidence_dir: &Path,
    scenarios: &BTreeMap<String, (String, Value)>,
) -> Result<Vec<MemberDigestV1>> {
    scenarios
        .values()
        .map(|(relative, _)| {
            let mut digest = hash_file(&evidence_dir.join(relative))?;
            digest.path = relative.clone();
            Ok(digest)
        })
        .collect()
}

fn validate_public_scenario(test_id: &str, scenario: &Value) -> Result<()> {
    match test_id {
        "OCM-PUB-001" => validate_applied_public_path(scenario),
        "OCM-PUB-002" => {
            validate_applied_public_path(scenario)?;
            let public = public_path(scenario)?;
            ensure!(
                bool_field(public, "non_quorum_changed_binding_reverted")?
                    && bool_field(public, "non_quorum_state_unchanged")?,
                "public mutation scenario did not prove scoped rollback"
            );
            Ok(())
        }
        "OCM-PUB-003" => validate_expired_public_path(scenario),
        "OCM-PUB-004" => {
            validate_applied_public_path(scenario)?;
            let public = public_path(scenario)?;
            ensure!(
                bool_field(public, "exact_completed_retry_succeeded")?
                    && bool_field(public, "changed_completed_binding_reverted")?
                    && bool_field(public, "completed_state_unchanged")?,
                "completed public replay did not prove idempotency and rejection"
            );
            Ok(())
        }
        _ => eyre::bail!("unknown public lane test {test_id}"),
    }
}

fn validate_e2e_scenario(test_id: &str, scenario: &Value) -> Result<()> {
    let public = public_path(scenario)?;
    match test_id {
        "OCM-E2E-001" => validate_applied_public_path(scenario),
        "OCM-E2E-007" => validate_applied_public_path_with_vote_count(scenario, 3),
        "OCM-E2E-008" => {
            validate_applied_public_path(scenario)?;
            ensure!(
                bool_field(public, "restart_replay_verified")?,
                "completed generation restart/replay proof is absent"
            );
            Ok(())
        }
        "OCM-TRC-001" => {
            validate_applied_public_path(scenario)?;
            validate_execution_trace(public)
        }
        _ => eyre::bail!("unknown E2E lane test {test_id}"),
    }
}

fn validate_common_scenario(scenario: &Value, source_sha: &str) -> Result<()> {
    ensure!(
        string_field(scenario, "result")? == "passed",
        "scenario did not pass"
    );
    ensure!(
        path(scenario, &["source", "sha"])?.as_str() == Some(source_sha),
        "scenario was produced by another source revision"
    );
    let binaries = path(scenario, &["ocomp", "exact_binaries"])?;
    ensure!(
        binaries.is_object()
            && binaries.as_object().is_some_and(|value| {
                value.len() == REQUIRED_SCENARIO_BINARIES.len()
                    && REQUIRED_SCENARIO_BINARIES
                        .iter()
                        .all(|name| value.contains_key(*name))
            }),
        "scenario lacks exact OCOMP binary identities"
    );
    for (name, binary) in binaries
        .as_object()
        .expect("object shape checked immediately above")
    {
        let binary_path = Path::new(string_field(binary, "path")?);
        let actual = hash_file(binary_path)
            .wrap_err_with(|| format!("rehash exact scenario binary {name}"))?;
        ensure!(
            actual.length == u64_field(binary, "length")?
                && actual.sha256 == string_field(binary, "sha256")?,
            "scenario binary {name} differs from its retained identity"
        );
    }
    let topology = path(scenario, &["ocomp", "topology"])?;
    ensure!(
        path(topology, &["launch_identity"])?.is_object(),
        "scenario lacks immutable launch identity"
    );
    Ok(())
}

fn validate_applied_public_path(scenario: &Value) -> Result<()> {
    validate_applied_public_path_with_vote_count(scenario, 4)
}

fn validate_applied_public_path_with_vote_count(
    scenario: &Value,
    expected_vote_count: usize,
) -> Result<()> {
    ensure!(
        matches!(expected_vote_count, 3 | 4),
        "unsupported PoC vote count {expected_vote_count}"
    );
    let public = public_path(scenario)?;
    let request = object_field(public, "job_request")?;
    validate_window(request)?;
    let activation = object_field(public, "activation")?;
    let generation = object_field(public, "certified_generation")?;
    let accountability = object_field(public, "vote_accountability")?;
    ensure!(
        string_field(activation, "job_id")? == string_field(generation, "job_id")?
            && string_field(activation, "job_id")? == string_field(accountability, "job_id")?,
        "applied public evidence disagrees on JobId"
    );
    ensure!(
        string_field(activation, "result_digest")?
            == string_field(accountability, "quorum_result_digest")?,
        "activation digest differs from immutable quorum"
    );
    let open_height = u64_field(request, "open_height")?;
    let deadline_height = u64_field(request, "deadline_height")?;
    let quorum_height = u64_field(accountability, "quorum_height")?;
    ensure!(
        open_height <= quorum_height && quorum_height < deadline_height,
        "public quorum height lies outside the response window"
    );
    ensure!(
        u64_field(accountability, "quorum_signer_bitmap")?.count_ones() == 3,
        "public quorum bitmap is not exactly q=3"
    );
    let slots = array_field(accountability, "slot_validator_indexes")?
        .iter()
        .map(Value::as_u64)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| eyre::eyre!("vote slot indexes are not integers"))?;
    let expected_slots = if expected_vote_count == 4 {
        vec![0, 1, 2, 3]
    } else {
        vec![1, 2, 3]
    };
    ensure!(
        slots == expected_slots,
        "public accountability does not contain the expected validator slots"
    );

    let transactions = array_field(public, "result_vote_transactions")?;
    let successful_transactions = transactions
        .iter()
        .filter(|transaction| transaction.get("success").and_then(Value::as_bool) == Some(true))
        .collect::<Vec<_>>();
    ensure!(
        successful_transactions.len() == expected_vote_count,
        "public path did not retain the expected successful validator votes"
    );
    let mut signers = BTreeSet::new();
    let activation_tx = string_field(activation, "transaction_hash")?;
    let mut saw_activation_tx = false;
    for transaction in successful_transactions {
        let height = u64_field(transaction, "block_number")?;
        ensure!(
            open_height <= height && height < deadline_height,
            "a successful vote was included outside the response window"
        );
        signers.insert(string_field(transaction, "signer")?.to_owned());
        saw_activation_tx |= string_field(transaction, "transaction_hash")? == activation_tx;
    }
    ensure!(
        signers.len() == expected_vote_count && saw_activation_tx,
        "public votes lack expected distinct signers or the q-forming transaction"
    );
    ensure!(
        public.get("validator_balances_before") == public.get("validator_balances_after"),
        "a validator paid for a public ResultVote"
    );
    ensure!(
        bool_field(public, "atomic_quorum_apply_verified")?,
        "q-forming public vote did not prove atomic Nod apply"
    );
    ensure!(
        u64_field(generation, "tribute_count")? > 0
            && u64_field(generation, "tribute_count")? == u64_field(generation, "nod_count")?,
        "certified generation count conservation failed"
    );
    Ok(())
}

fn validate_execution_trace(public: &Value) -> Result<()> {
    let trace = object_field(public, "execution_trace")?;
    ensure!(
        u64_field(trace, "request_height")? > 0
            && u64_field(trace, "q_forming_height")? >= u64_field(trace, "request_height")?,
        "trace heights are incomplete or unordered"
    );
    ensure!(
        !array_field(trace, "proposal_request_nodes")?.is_empty()
            && array_field(trace, "canonical_request_nodes")?.len() >= 3
            && array_field(trace, "canonical_q_vote_nodes")?.len() == 4,
        "trace does not cover proposer and four-node canonical execution"
    );
    ensure!(
        bool_field(trace, "historical_request_observed")?
            && bool_field(trace, "historical_q_vote_observed")?
            && string_field(trace, "historical_replay_node")? == "follower",
        "late historical replay did not execute both OCOMP boundaries"
    );
    ensure!(
        u64_field(trace, "forbidden_calculation_entries")? == 0,
        "runtime trace entered an on-chain calculation module"
    );
    Ok(())
}

fn validate_expired_public_path(scenario: &Value) -> Result<()> {
    let public = public_path(scenario)?;
    let request = object_field(public, "job_request")?;
    validate_window(request)?;
    ensure!(
        public.get("activation").is_some_and(Value::is_null)
            && public
                .get("certified_generation")
                .is_some_and(Value::is_null),
        "expired public job produced activation or Nod generation"
    );
    let accountability = object_field(public, "vote_accountability")?;
    let deadline = u64_field(request, "deadline_height")?;
    ensure!(
        u64_field(accountability, "closed_height")? == deadline
            && accountability
                .get("quorum_result_digest")
                .is_some_and(Value::is_null),
        "no-quorum accountability did not close exactly at the deadline"
    );
    ensure!(
        u64_field(accountability, "timely_bitmap")? == 0b0011
            && u64_field(accountability, "missing_bitmap")? == 0b1100,
        "expired job accountability bitmaps are incorrect"
    );
    ensure!(
        bool_field(public, "expired_without_nod")?
            && bool_field(public, "late_vote_reverted")?
            && u64_field(public, "late_vote_inclusion_height")? == deadline,
        "exclusive-deadline public vote behavior was not proved"
    );
    Ok(())
}

fn validate_window(request: &Value) -> Result<()> {
    let finality = u64_field(request, "finality_recorded_height")?;
    let open = u64_field(request, "open_height")?;
    let deadline = u64_field(request, "deadline_height")?;
    ensure!(
        finality.checked_add(4) == Some(open) && open < deadline,
        "public response window is not finality+4 with an exclusive deadline"
    );
    Ok(())
}

fn validate_isolation_scenario(scenario: &Value) -> Result<()> {
    const EXPECTED_MEMORY_MAX: u64 = 2 * 1024 * 1024 * 1024;
    const EXPECTED_TASKS_MAX: u64 = 16;
    const EXPECTED_QUOTA_MAX: u64 = 96 * 1024 * 1024;

    let topology = path(scenario, &["ocomp", "topology"])?;
    let isolation = object_field(topology, "systemd_isolation")?;
    ensure!(
        !string_field(isolation, "systemd_version")?.is_empty()
            && u64_field(isolation, "cgroup_version")? == 2,
        "ISO scenario did not run on systemd with unified cgroup v2"
    );
    let slice = string_field(isolation, "slice")?;
    ensure!(
        !slice.is_empty(),
        "ISO scenario has no aggregate systemd slice"
    );
    let role_uids = [
        ("node", u64_field(isolation, "node_uid")?),
        ("supervisor", u64_field(isolation, "supervisor_uid")?),
        (
            "snapshot_exporter",
            u64_field(isolation, "snapshot_exporter_uid")?,
        ),
        ("worker", u64_field(isolation, "worker_uid")?),
    ];
    ensure!(
        role_uids.iter().all(|(_, uid)| *uid != 0)
            && role_uids
                .iter()
                .map(|(_, uid)| *uid)
                .collect::<BTreeSet<_>>()
                .len()
                == 4,
        "ISO roles do not have four distinct non-root UIDs"
    );

    let processes = array_field(isolation, "processes")?;
    ensure!(
        processes.len() == 16,
        "ISO process inventory does not contain four roles in four domains"
    );
    let mut process_keys = BTreeSet::new();
    let mut process_ids = BTreeSet::new();
    let mut node_mounts = BTreeMap::new();
    for process in processes {
        let validator = u64_field(process, "validator_index")?;
        let role = string_field(process, "role")?;
        let pid = u64_field(process, "pid")?;
        let uid = u64_field(process, "uid")?;
        let expected_uid = role_uids
            .iter()
            .find_map(|(expected_role, expected_uid)| {
                (*expected_role == role).then_some(*expected_uid)
            })
            .ok_or_else(|| eyre::eyre!("unknown ISO role {role}"))?;
        ensure!(
            validator < 4
                && pid != 0
                && uid == expected_uid
                && process_keys.insert((validator, role.to_owned()))
                && process_ids.insert(pid)
                && string_field(process, "cgroup")?.starts_with('/')
                && string_field(process, "mount_namespace")?.starts_with("mnt:["),
            "ISO process identity is duplicate, incomplete or uses the wrong UID"
        );
        if role == "node" {
            node_mounts.insert(
                validator,
                string_field(process, "mount_namespace")?.to_owned(),
            );
        }
    }
    ensure!(
        (0_u64..4).all(|validator| {
            ["node", "supervisor", "snapshot_exporter", "worker"]
                .into_iter()
                .all(|role| process_keys.contains(&(validator, role.to_owned())))
        }),
        "ISO process inventory is missing a validator/role pair"
    );
    for process in processes {
        if string_field(process, "role")? == "node" {
            continue;
        }
        let validator = u64_field(process, "validator_index")?;
        ensure!(
            string_field(process, "cgroup")?.contains("outbe-ocomp-iso")
                && node_mounts.get(&validator).map(String::as_str)
                    != Some(string_field(process, "mount_namespace")?),
            "compute process shares the node cgroup or mount namespace"
        );
    }

    let policies = array_field(isolation, "unit_policies")?;
    ensure!(
        policies.len() == 12,
        "ISO unit policy inventory does not contain every compute role"
    );
    let mut policy_keys = BTreeSet::new();
    for policy in policies {
        let validator = u64_field(policy, "validator_index")?;
        let role = string_field(policy, "role")?;
        ensure!(
            validator < 4
                && matches!(role, "supervisor" | "snapshot_exporter" | "worker")
                && policy_keys.insert((validator, role.to_owned()))
                && string_field(policy, "slice")? == slice
                && u64_field(policy, "memory_max")? == EXPECTED_MEMORY_MAX
                && u64_field(policy, "tasks_max")? == EXPECTED_TASKS_MAX
                && bool_field(policy, "cpu_accounting")?
                && bool_field(policy, "memory_accounting")?
                && bool_field(policy, "tasks_accounting")?
                && string_field(policy, "protect_system")? == "strict"
                && bool_field(policy, "no_new_privileges")?
                && bool_field(policy, "private_devices")?
                && bool_field(policy, "restrict_suid_sgid")?,
            "ISO systemd unit lacks the exact bounded security/resource policy"
        );
    }

    let quota_mounts = array_field(isolation, "quota_mounts")?;
    ensure!(
        quota_mounts.len() == 16,
        "ISO scenario did not retain four quota filesystems per domain"
    );
    let mut mountpoints = BTreeSet::new();
    for mount in quota_mounts {
        let options = string_field(mount, "options")?;
        ensure!(
            u64_field(mount, "validator_index")? < 4
                && string_field(mount, "filesystem_type")? == "tmpfs"
                && (1..=EXPECTED_QUOTA_MAX).contains(&u64_field(mount, "size_bytes")?)
                && mountpoints.insert(string_field(mount, "mountpoint")?.to_owned())
                && ["nodev", "nosuid", "noexec"]
                    .into_iter()
                    .all(|required| options.split(',').any(|actual| actual == required)),
            "ISO quota filesystem is missing, shared or unbounded"
        );
    }

    ensure!(
        (1..=16).contains(&u64_field(isolation, "max_live_workers_total")?)
            && u64_field(isolation, "node_control_handshakes")? == 8
            && u64_field(isolation, "worker_handshakes")? == 4,
        "ISO runtime lacks bounded worker or authenticated UDS evidence"
    );
    let per_domain = array_field(isolation, "max_live_workers_per_domain")?;
    ensure!(
        per_domain.len() == 4
            && per_domain.iter().enumerate().all(|(validator, entry)| {
                entry.as_array().is_some_and(|fields| {
                    fields.len() == 2
                        && fields[0].as_u64() == u64::try_from(validator).ok()
                        && fields[1]
                            .as_u64()
                            .is_some_and(|observed| (1..=4).contains(&observed))
                })
            }),
        "ISO per-domain worker concurrency evidence is incomplete or over cap"
    );
    let before = u64_field(isolation, "finality_before_faults")?;
    let after = u64_field(isolation, "finality_after_faults")?;
    ensure!(
        after > before,
        "ISO faults did not preserve advancing consensus finality"
    );

    let faults = array_field(topology, "faults")?;
    ensure!(
        faults.len() == 2,
        "ISO scenario did not apply exactly two typed compute faults"
    );
    let observed_faults = faults
        .iter()
        .map(|record| {
            let fault = object_field(record, "fault")?;
            Ok((
                string_field(fault, "kind")?.to_owned(),
                u64_field(fault, "validator_index")?,
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        observed_faults
            == BTreeSet::from([
                ("stop_supervisor".to_owned(), 0),
                ("stop_worker".to_owned(), 1),
            ]),
        "ISO scenario applied the wrong process faults"
    );
    Ok(())
}

fn public_path(scenario: &Value) -> Result<&Value> {
    let value = path(scenario, &["ocomp", "public_path"])?;
    ensure!(value.is_object(), "scenario lacks public OCOMP evidence");
    Ok(value)
}

fn path<'a>(value: &'a Value, components: &[&str]) -> Result<&'a Value> {
    components.iter().try_fold(value, |current, component| {
        current
            .get(*component)
            .ok_or_else(|| eyre::eyre!("missing JSON field {}", components.join(".")))
    })
}

fn object_field<'a>(value: &'a Value, field: &str) -> Result<&'a Value> {
    let value = value
        .get(field)
        .ok_or_else(|| eyre::eyre!("missing JSON field {field}"))?;
    ensure!(value.is_object(), "JSON field {field} is not an object");
    Ok(value)
}

fn array_field<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| eyre::eyre!("JSON field {field} is not an array"))
}

fn string_field<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| eyre::eyre!("JSON field {field} is not a string"))
}

fn u64_field(value: &Value, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| eyre::eyre!("JSON field {field} is not a u64"))
}

fn bool_field(value: &Value, field: &str) -> Result<bool> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| eyre::eyre!("JSON field {field} is not a bool"))
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
    use super::{
        scenario_run_identity, validate_applied_public_path, validate_expired_public_path,
        validate_isolation_scenario, validate_scenario_manifest_identity, E2E_SCENARIOS,
        PUBLIC_SCENARIOS, REQUIRED_SCENARIO_BINARIES,
    };
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    #[test]
    fn expensive_scenarios_share_only_compatible_terminal_observations() {
        let public = PUBLIC_SCENARIOS
            .iter()
            .map(|(test_id, scenario, _)| (*test_id, *scenario))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(public["OCM-PUB-001"], public["OCM-PUB-004"]);
        assert_ne!(public["OCM-PUB-001"], public["OCM-PUB-002"]);
        assert_ne!(public["OCM-PUB-001"], public["OCM-PUB-003"]);

        let e2e = E2E_SCENARIOS
            .iter()
            .map(|(test_id, scenario, _)| (*test_id, *scenario))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(e2e["OCM-E2E-001"], e2e["OCM-TRC-001"]);
        assert_ne!(e2e["OCM-E2E-001"], e2e["OCM-E2E-007"]);
        assert_ne!(e2e["OCM-E2E-001"], e2e["OCM-E2E-008"]);
    }

    fn applied_scenario() -> Value {
        let transactions = (0_u64..4)
            .map(|validator| {
                json!({
                    "block_number": 16 + validator,
                    "signer": format!("validator-{validator}"),
                    "transaction_hash": if validator == 2 { "0xquorum" } else { "0xvote" },
                    "success": true,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "ocomp": {
                "public_path": {
                    "job_request": {
                        "finality_recorded_height": 10,
                        "open_height": 14,
                        "deadline_height": 30,
                    },
                    "activation": {
                        "job_id": "0xjob",
                        "result_digest": "0xresult",
                        "transaction_hash": "0xquorum",
                    },
                    "certified_generation": {
                        "job_id": "0xjob",
                        "tribute_count": 8,
                        "nod_count": 8,
                    },
                    "vote_accountability": {
                        "job_id": "0xjob",
                        "quorum_result_digest": "0xresult",
                        "quorum_height": 18,
                        "quorum_signer_bitmap": 7,
                        "slot_validator_indexes": [0, 1, 2, 3],
                    },
                    "result_vote_transactions": transactions,
                    "validator_balances_before": [["validator-0", "0"]],
                    "validator_balances_after": [["validator-0", "0"]],
                    "atomic_quorum_apply_verified": true,
                },
            },
        })
    }

    fn expired_scenario() -> Value {
        json!({
            "ocomp": {
                "public_path": {
                    "job_request": {
                        "finality_recorded_height": 10,
                        "open_height": 14,
                        "deadline_height": 30,
                    },
                    "activation": null,
                    "certified_generation": null,
                    "vote_accountability": {
                        "closed_height": 30,
                        "quorum_result_digest": null,
                        "timely_bitmap": 3,
                        "missing_bitmap": 12,
                    },
                    "expired_without_nod": true,
                    "late_vote_reverted": true,
                    "late_vote_inclusion_height": 30,
                },
            },
        })
    }

    fn isolation_scenario() -> Value {
        let role_uids = [
            ("node", 1001_u64),
            ("supervisor", 1002),
            ("snapshot_exporter", 1003),
            ("worker", 1004),
        ];
        let mut processes = Vec::new();
        let mut unit_policies = Vec::new();
        let mut quota_mounts = Vec::new();
        for validator in 0_u64..4 {
            for (role_index, (role, uid)) in role_uids.iter().enumerate() {
                let pid = 10_000 + validator * 10 + u64::try_from(role_index).unwrap_or_default();
                let mount_namespace = if *role == "node" {
                    format!("mnt:[{}]", 20_000 + validator)
                } else {
                    format!("mnt:[{}]", 30_000 + pid)
                };
                processes.push(json!({
                    "validator_index": validator,
                    "role": role,
                    "pid": pid,
                    "uid": uid,
                    "gid": uid,
                    "cgroup": if *role == "node" {
                        format!("/system.slice/outbe-node-{validator}.service")
                    } else {
                        format!("/outbe-ocomp-iso.slice/domain-{validator}/{role}")
                    },
                    "mount_namespace": mount_namespace,
                    "unit": format!("outbe-{role}-{validator}.service"),
                }));
                if *role != "node" {
                    unit_policies.push(json!({
                        "validator_index": validator,
                        "role": role,
                        "unit": format!("outbe-{role}-{validator}.service"),
                        "slice": "outbe-ocomp-iso.slice",
                        "memory_max": 2_u64 * 1024 * 1024 * 1024,
                        "tasks_max": 16,
                        "cpu_accounting": true,
                        "memory_accounting": true,
                        "tasks_accounting": true,
                        "protect_system": "strict",
                        "no_new_privileges": true,
                        "private_devices": true,
                        "restrict_suid_sgid": true,
                    }));
                }
            }
            for store in ["cas", "exporter", "supervisor", "worker-inbox"] {
                quota_mounts.push(json!({
                    "validator_index": validator,
                    "mountpoint": format!("/run/outbe-ocomp-iso/{validator}/{store}"),
                    "filesystem_type": "tmpfs",
                    "size_bytes": 96_u64 * 1024 * 1024,
                    "options": "rw,nodev,nosuid,noexec",
                }));
            }
        }
        json!({
            "ocomp": {
                "topology": {
                    "systemd_isolation": {
                        "systemd_version": "systemd 255",
                        "cgroup_version": 2,
                        "slice": "outbe-ocomp-iso.slice",
                        "node_uid": 1001,
                        "supervisor_uid": 1002,
                        "snapshot_exporter_uid": 1003,
                        "worker_uid": 1004,
                        "processes": processes,
                        "unit_policies": unit_policies,
                        "quota_mounts": quota_mounts,
                        "max_live_workers_total": 4,
                        "max_live_workers_per_domain": [[0, 1], [1, 1], [2, 1], [3, 1]],
                        "node_control_handshakes": 8,
                        "worker_handshakes": 4,
                        "finality_before_faults": 40,
                        "finality_after_faults": 42,
                    },
                    "faults": [
                        {"fault": {"kind": "stop_supervisor", "validator_index": 0}},
                        {"fault": {"kind": "stop_worker", "validator_index": 1}},
                    ],
                },
            },
        })
    }

    fn run_identity_scenario(
        binary_digest: &str,
        genesis_hash: &str,
        gramine_image_id: &str,
    ) -> Value {
        let binaries = REQUIRED_SCENARIO_BINARIES
            .into_iter()
            .map(|name| {
                (
                    name.to_owned(),
                    json!({
                        "path": format!("/artifact-set/{name}"),
                        "length": 10,
                        "sha256": binary_digest,
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        json!({
            "environment": {
                "gramine_image_id": gramine_image_id,
            },
            "ocomp": {
                "exact_binaries": binaries,
                "topology": {
                    "launch_identity": {
                        "chain_id": 3151908,
                        "genesis_hash": genesis_hash,
                        "protocol_bundle_hash": "0xbundle",
                        "fork_install_hash": "0xfork",
                    },
                    "systemd_isolation": null,
                },
            },
        })
    }

    #[test]
    fn applied_evidence_requires_real_window_balances_and_four_slots() {
        let valid = applied_scenario();
        validate_applied_public_path(&valid).expect("valid applied evidence");

        let mut invalid_window = valid.clone();
        invalid_window["ocomp"]["public_path"]["job_request"]["open_height"] = json!(13);
        assert!(validate_applied_public_path(&invalid_window).is_err());

        let mut charged_validator = valid.clone();
        charged_validator["ocomp"]["public_path"]["validator_balances_after"] =
            json!([["validator-0", "1"]]);
        assert!(validate_applied_public_path(&charged_validator).is_err());

        let mut missing_slot = valid;
        missing_slot["ocomp"]["public_path"]["vote_accountability"]["slot_validator_indexes"] =
            json!([0, 1, 2]);
        assert!(validate_applied_public_path(&missing_slot).is_err());
    }

    #[test]
    fn expiry_evidence_requires_exclusive_deadline_and_no_nod() {
        let valid = expired_scenario();
        validate_expired_public_path(&valid).expect("valid expiry evidence");

        let mut late_in_next_block = valid.clone();
        late_in_next_block["ocomp"]["public_path"]["late_vote_inclusion_height"] = json!(31);
        assert!(validate_expired_public_path(&late_in_next_block).is_err());

        let mut generated_nod = valid;
        generated_nod["ocomp"]["public_path"]["certified_generation"] = json!({"job_id": "0xjob"});
        assert!(validate_expired_public_path(&generated_nod).is_err());
    }

    #[test]
    fn isolation_evidence_requires_real_domain_separation_and_bounded_resources() {
        let valid = isolation_scenario();
        validate_isolation_scenario(&valid).expect("valid isolation evidence");

        let mut shared_uid = valid.clone();
        shared_uid["ocomp"]["topology"]["systemd_isolation"]["worker_uid"] = json!(1002);
        assert!(validate_isolation_scenario(&shared_uid).is_err());

        let mut shared_mount_namespace = valid.clone();
        let node_mount = shared_mount_namespace["ocomp"]["topology"]["systemd_isolation"]
            ["processes"][0]["mount_namespace"]
            .clone();
        shared_mount_namespace["ocomp"]["topology"]["systemd_isolation"]["processes"][3]
            ["mount_namespace"] = node_mount;
        assert!(validate_isolation_scenario(&shared_mount_namespace).is_err());

        let mut unbounded_worker = valid.clone();
        unbounded_worker["ocomp"]["topology"]["systemd_isolation"]["unit_policies"][2]
            ["memory_max"] = json!(u64::MAX);
        assert!(validate_isolation_scenario(&unbounded_worker).is_err());

        let mut oversized_quota = valid.clone();
        oversized_quota["ocomp"]["topology"]["systemd_isolation"]["quota_mounts"][0]
            ["size_bytes"] = json!(97_u64 * 1024 * 1024);
        assert!(validate_isolation_scenario(&oversized_quota).is_err());
    }

    #[test]
    fn isolation_evidence_requires_typed_faults_and_advancing_finality() {
        let valid = isolation_scenario();

        let mut stalled_finality = valid.clone();
        stalled_finality["ocomp"]["topology"]["systemd_isolation"]["finality_after_faults"] =
            json!(40);
        assert!(validate_isolation_scenario(&stalled_finality).is_err());

        let mut wrong_fault = valid.clone();
        wrong_fault["ocomp"]["topology"]["faults"][1]["fault"]["kind"] =
            json!("stop_snapshot_exporter");
        assert!(validate_isolation_scenario(&wrong_fault).is_err());

        let mut aggregate_over_cap = valid;
        aggregate_over_cap["ocomp"]["topology"]["systemd_isolation"]["max_live_workers_total"] =
            json!(17);
        assert!(validate_isolation_scenario(&aggregate_over_cap).is_err());
    }

    #[test]
    fn scenario_set_requires_one_binary_chain_and_gramine_image_identity() {
        let image_id = format!("sha256:{}", "ab".repeat(32));
        let first = run_identity_scenario("aa", "0xgenesis", &image_id);
        let second = first.clone();
        let scenarios = BTreeMap::from([
            ("first".to_owned(), ("scenario-001.json".to_owned(), first)),
            (
                "second".to_owned(),
                ("scenario-002.json".to_owned(), second),
            ),
        ]);
        let identity = scenario_run_identity(&scenarios).expect("one exact scenario identity");
        let mut sections = BTreeMap::from([
            (
                "exact_binary_hashes".to_owned(),
                identity.exact_binaries.clone(),
            ),
            (
                "genesis_fork_bundle_and_profiles".to_owned(),
                identity.launch_identity.clone(),
            ),
            (
                "exact_config_and_service_unit_hashes".to_owned(),
                json!({"gramine_image_id": identity.gramine_image_id}),
            ),
        ]);
        validate_scenario_manifest_identity(&sections, &identity)
            .expect("lane manifest binds retained image identity");
        sections
            .get_mut("exact_config_and_service_unit_hashes")
            .expect("config section")["gramine_image_id"] =
            json!(format!("sha256:{}", "cd".repeat(32)));
        assert!(validate_scenario_manifest_identity(&sections, &identity).is_err());

        let mut changed_binary = scenarios.clone();
        changed_binary.get_mut("second").expect("second").1["ocomp"]["exact_binaries"]
            ["outbe_chain"]["sha256"] = json!("bb");
        assert!(scenario_run_identity(&changed_binary).is_err());

        let mut changed_chain = scenarios.clone();
        changed_chain.get_mut("second").expect("second").1["ocomp"]["topology"]
            ["launch_identity"]["genesis_hash"] = json!("0xother");
        assert!(scenario_run_identity(&changed_chain).is_err());

        let mut changed_image = scenarios.clone();
        changed_image.get_mut("second").expect("second").1["environment"]["gramine_image_id"] =
            json!(format!("sha256:{}", "cd".repeat(32)));
        assert!(scenario_run_identity(&changed_image).is_err());

        let mut missing_image = scenarios;
        missing_image.get_mut("second").expect("second").1["environment"]
            .as_object_mut()
            .expect("environment object")
            .remove("gramine_image_id");
        assert!(scenario_run_identity(&missing_image).is_err());
    }
}
