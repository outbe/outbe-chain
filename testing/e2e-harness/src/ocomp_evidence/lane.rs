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

const REQUIRED_SCENARIO_BINARIES: [&str; 7] = [
    "outbe_chain",
    "outbe_cli",
    "outbe_e2e",
    "outbe_feeder",
    "outbe_keygen",
    "outbe_ocomp",
    "outbe_tee_enclave",
];

const PUBLIC_SCENARIOS: [(&str, &str, &str); 4] = [
    (
        "OCM-PUB-001",
        "A public Tribute completes real OCOMP, FullNode verification, NOD, replay, and contributor payout",
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
        "A public Tribute completes real OCOMP, FullNode verification, NOD, replay, and contributor payout",
        "FINALIZED_PUBLIC_STATE",
    ),
];

const E2E_SCENARIOS: [(&str, &str, &str); 3] = [
    (
        "OCM-E2E-001",
        "A public Tribute completes real OCOMP, FullNode verification, NOD, replay, and contributor payout",
        "FINALIZED_PUBLIC_STATE",
    ),
    (
        "OCM-TRC-001",
        "A public Tribute completes real OCOMP, FullNode verification, NOD, replay, and contributor payout",
        "RUNTIME_BOUNDARY_TRACE",
    ),
    (
        "OCM-E2E-008",
        "A public Tribute completes real OCOMP, FullNode verification, NOD, replay, and contributor payout",
        "FINALIZED_PUBLIC_STATE",
    ),
];

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
        _ => {
            eyre::bail!("lane assembly is not implemented for {lane}");
        }
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
        _ => {
            eyre::bail!("semantic lane verification requires lane mode");
        }
    };
    let evidence_dir = manifest_path
        .parent()
        .ok_or_else(|| eyre::eyre!("lane manifest has no parent directory"))?;
    match lane {
        "OCM-FAST" | "OCM-INT" => {
            verify_command_lane_manifest_semantics(repo, ledger, lane, &manifest, evidence_dir)
        }
        "OCM-PUBLIC" => verify_scenario_lane(repo, &manifest, evidence_dir, &PUBLIC_SCENARIOS),
        "OCM-E2E" => verify_scenario_lane(repo, &manifest, evidence_dir, &E2E_SCENARIOS),
        _ => {
            eyre::bail!("semantic lane verification is not implemented for {lane}");
        }
    }
}

fn verify_scenario_lane(
    repo: &Path,
    manifest: &RunManifestV1,
    evidence_dir: &Path,
    expected_scenarios: &[(&str, &str, &str)],
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
    if expected_scenarios == E2E_SCENARIOS {
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

fn validate_scenario_manifest_identity(
    sections: &BTreeMap<String, Value>,
    identity: &ScenarioRunIdentity,
) -> Result<()> {
    let manifest_config = sections
        .get("exact_config_and_service_unit_hashes")
        .and_then(Value::as_object);
    ensure!(
        sections.get("exact_binary_hashes") == Some(&identity.exact_binaries)
            && sections.get("genesis_fork_bundle_and_profiles")
                == Some(&identity.launch_evidence())
            && manifest_config.and_then(|section| section.get("scenario_launch_identities"))
                == Some(&json!(identity.scenario_launch_identities))
            && manifest_config.and_then(|section| section.get("gramine_image_id"))
                == Some(&identity.gramine_image_id)
            && manifest_config.and_then(|section| section.get("execution_profile"))
                == Some(&identity.execution_profile),
        "scenario lane manifest identity differs from retained scenarios"
    );
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ScenarioRunIdentity {
    exact_binaries: Value,
    scenario_launch_identities: BTreeMap<String, Value>,
    gramine_image_id: Value,
    execution_profile: Value,
}

impl ScenarioRunIdentity {
    fn launch_evidence(&self) -> Value {
        json!({
            "scenario_launch_identities": &self.scenario_launch_identities,
        })
    }
}

fn validate_launch_identity(launch: &Value, scenario_name: &str) -> Result<()> {
    ensure!(
        launch.is_object(),
        "scenario {scenario_name} lacks exact launch identity"
    );
    ensure!(
        path(launch, &["chain_id"])?.as_u64().is_some()
            && path(launch, &["activation_height"])?.as_u64().is_some(),
        "scenario {scenario_name} has invalid numeric launch identity"
    );
    for field in [
        "genesis_hash",
        "protocol_bundle_hash",
        "fork_install_hash",
        "classification",
        "metadosis_storage_layout_hash",
    ] {
        ensure!(
            path(launch, &[field])?
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "scenario {scenario_name} has invalid launch identity field {field}"
        );
    }
    Ok(())
}

fn scenario_run_identity(
    scenarios: &BTreeMap<String, (String, Value)>,
) -> Result<ScenarioRunIdentity> {
    let mut exact_binaries = None;
    let mut scenario_launch_identities = BTreeMap::new();
    let mut gramine_image_id = None;
    let mut execution_profile = None;
    for (scenario_name, (scenario_path, scenario)) in scenarios {
        let tee = path(scenario, &["environment", "tee"])?;
        let sudo = path(scenario, &["environment", "sudo"])?;
        let all = path(scenario, &["environment", "all"])?;
        ensure!(
            tee.as_str() == Some("sgx-no-attest")
                && sudo.as_bool() == Some(true)
                && all.as_bool() == Some(true),
            "scenario {scenario_name} did not use release SGX-no-attest with sudo and fail-not-skip"
        );
        let profile = json!({"tee": tee, "sudo": sudo, "all": all});
        match &execution_profile {
            Some(expected) => ensure!(
                expected == &profile,
                "scenario {scenario_name} used another execution profile"
            ),
            None => execution_profile = Some(profile),
        }

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
        validate_launch_identity(launch, scenario_name)?;
        ensure!(
            scenario_launch_identities
                .insert(scenario_path.clone(), launch.clone())
                .is_none(),
            "scenario launch identity member {scenario_path} is duplicated"
        );

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
    }
    Ok(ScenarioRunIdentity {
        exact_binaries: exact_binaries
            .ok_or_else(|| eyre::eyre!("scenario set has no exact binary identity"))?,
        scenario_launch_identities,
        gramine_image_id: gramine_image_id
            .ok_or_else(|| eyre::eyre!("scenario set has no Gramine Docker image identity"))?,
        execution_profile: execution_profile
            .ok_or_else(|| eyre::eyre!("scenario set has no execution profile"))?,
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
                    "scenario_launch_identities": &identity.scenario_launch_identities,
                    "gramine_image_id": &identity.gramine_image_id,
                    "execution_profile": &identity.execution_profile,
                }),
                "genesis_fork_bundle_and_profiles" => identity.launch_evidence(),
                "test_discovery" => json!(discovery),
                "machine_and_service_topology" => json!({
                    "validated_scenarios": scenario_paths,
                    "process_orchestration": "rust_e2e_harness",
                    "gramine_image_id": &identity.gramine_image_id,
                    "execution_profile": &identity.execution_profile,
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
                    "process_boundary": "harness_owned_processes",
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
        _ => {
            eyre::bail!("unknown public lane test {test_id}");
        }
    }
}

fn validate_e2e_scenario(test_id: &str, scenario: &Value) -> Result<()> {
    let public = public_path(scenario)?;
    match test_id {
        "OCM-E2E-001" => validate_applied_public_path(scenario),
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
        _ => {
            eyre::bail!("unknown E2E lane test {test_id}");
        }
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
    let quorum_signer_bitmap = four_member_bitmap(accountability, "quorum_signer_bitmap")?;
    ensure!(
        quorum_signer_bitmap
            .iter()
            .map(|byte| byte.count_ones())
            .sum::<u32>()
            == 3,
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
        four_member_bitmap(accountability, "timely_bitmap")? == [0b0011]
            && four_member_bitmap(accountability, "missing_bitmap")? == [0b1100],
        "expired job accountability bitmaps are incorrect"
    );
    let late_vote_inclusion_height = u64_field(public, "late_vote_inclusion_height")?;
    ensure!(
        bool_field(public, "expired_without_nod")?
            && bool_field(public, "late_vote_reverted")?
            && late_vote_inclusion_height >= deadline
            && late_vote_inclusion_height.saturating_sub(deadline) <= 1,
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

fn four_member_bitmap(value: &Value, field: &str) -> Result<Vec<u8>> {
    let bitmap = array_field(value, field)?
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            byte.as_u64()
                .and_then(|byte| u8::try_from(byte).ok())
                .ok_or_else(|| eyre::eyre!("JSON bitmap {field}[{index}] is not a byte"))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        bitmap.len() == 1 && bitmap[0] & 0b1111_0000 == 0,
        "JSON bitmap {field} is not a canonical four-member LSB0 bitmap"
    );
    Ok(bitmap)
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
        validate_scenario_manifest_identity, E2E_SCENARIOS, PUBLIC_SCENARIOS,
        REQUIRED_SCENARIO_BINARIES,
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
        assert_eq!(e2e["OCM-E2E-001"], e2e["OCM-E2E-008"]);
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
                        "quorum_signer_bitmap": [7],
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
                        "timely_bitmap": [3],
                        "missing_bitmap": [12],
                    },
                    "expired_without_nod": true,
                    "late_vote_reverted": true,
                    "late_vote_inclusion_height": 30,
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
                "tee": "sgx-no-attest",
                "sudo": true,
                "all": true,
                "gramine_image_id": gramine_image_id,
            },
            "ocomp": {
                "exact_binaries": binaries,
                "topology": {
                    "launch_identity": {
                        "chain_id": 3151908,
                        "genesis_hash": genesis_hash,
                        "protocol_bundle_hash": "0xbundle",
                        "fork_install_hash": format!("0xfork-{genesis_hash}"),
                        "classification": "measurement",
                        "activation_height": 1,
                        "metadosis_storage_layout_hash": "0xlayout",
                    },
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
    fn public_evidence_accepts_canonical_dynamic_bitmaps() {
        let mut applied = applied_scenario();
        applied["ocomp"]["public_path"]["vote_accountability"]["quorum_signer_bitmap"] =
            json!([0b0111]);
        validate_applied_public_path(&applied).expect("canonical quorum bitmap");

        let mut expired = expired_scenario();
        expired["ocomp"]["public_path"]["vote_accountability"]["timely_bitmap"] = json!([0b0011]);
        expired["ocomp"]["public_path"]["vote_accountability"]["missing_bitmap"] = json!([0b1100]);
        validate_expired_public_path(&expired).expect("canonical accountability bitmaps");
    }

    #[test]
    fn expiry_evidence_accepts_deadline_or_next_block_and_no_nod() {
        let valid = expired_scenario();
        validate_expired_public_path(&valid).expect("valid expiry evidence");

        let mut late_in_next_block = valid.clone();
        late_in_next_block["ocomp"]["public_path"]["late_vote_inclusion_height"] = json!(31);
        validate_expired_public_path(&late_in_next_block).expect("next-block late vote evidence");

        let mut before_deadline = valid.clone();
        before_deadline["ocomp"]["public_path"]["late_vote_inclusion_height"] = json!(29);
        assert!(validate_expired_public_path(&before_deadline).is_err());

        let mut two_blocks_late = valid.clone();
        two_blocks_late["ocomp"]["public_path"]["late_vote_inclusion_height"] = json!(32);
        assert!(validate_expired_public_path(&two_blocks_late).is_err());

        let mut generated_nod = valid;
        generated_nod["ocomp"]["public_path"]["certified_generation"] = json!({"job_id": "0xjob"});
        assert!(validate_expired_public_path(&generated_nod).is_err());
    }

    #[test]
    fn scenario_set_allows_distinct_fresh_launches_with_one_binary_and_gramine_identity() {
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
                identity.launch_evidence(),
            ),
            (
                "exact_config_and_service_unit_hashes".to_owned(),
                json!({
                    "scenario_launch_identities": identity.scenario_launch_identities.clone(),
                    "gramine_image_id": identity.gramine_image_id.clone(),
                    "execution_profile": identity.execution_profile.clone(),
                }),
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
        let changed_identity = scenario_run_identity(&changed_chain)
            .expect("fresh scenarios retain their own exact genesis identity");
        assert_eq!(
            changed_identity.scenario_launch_identities["scenario-002.json"]["genesis_hash"],
            json!("0xother")
        );
        assert_eq!(changed_identity.scenario_launch_identities.len(), 2);

        let mut missing_launch = sections.clone();
        missing_launch
            .get_mut("genesis_fork_bundle_and_profiles")
            .expect("launch evidence")["scenario_launch_identities"]
            .as_object_mut()
            .expect("scenario launch map")
            .remove("scenario-002.json");
        assert!(validate_scenario_manifest_identity(&missing_launch, &identity).is_err());

        let mut changed_profile = scenarios.clone();
        changed_profile.get_mut("second").expect("second").1["ocomp"]["topology"]
            ["launch_identity"]["protocol_bundle_hash"] = json!("0xother-bundle");
        let changed_profile_identity = scenario_run_identity(&changed_profile)
            .expect("each retained scenario may exercise another exact launch profile");
        assert_eq!(
            changed_profile_identity.scenario_launch_identities["scenario-002.json"]
                ["protocol_bundle_hash"],
            json!("0xother-bundle")
        );

        let mut malformed_launch = scenarios.clone();
        malformed_launch.get_mut("second").expect("second").1["ocomp"]["topology"]
            ["launch_identity"]
            .as_object_mut()
            .expect("launch identity")
            .remove("fork_install_hash");
        assert!(scenario_run_identity(&malformed_launch).is_err());

        let mut changed_image = scenarios.clone();
        changed_image.get_mut("second").expect("second").1["environment"]["gramine_image_id"] =
            json!(format!("sha256:{}", "cd".repeat(32)));
        assert!(scenario_run_identity(&changed_image).is_err());

        let mut wrong_tee = scenarios.clone();
        wrong_tee.get_mut("second").expect("second").1["environment"]["tee"] =
            json!("gramine-direct");
        assert!(scenario_run_identity(&wrong_tee).is_err());

        let mut without_sudo = scenarios.clone();
        without_sudo.get_mut("second").expect("second").1["environment"]["sudo"] = json!(false);
        assert!(scenario_run_identity(&without_sudo).is_err());

        let mut fail_can_skip = scenarios.clone();
        fail_can_skip.get_mut("second").expect("second").1["environment"]["all"] = json!(false);
        assert!(scenario_run_identity(&fail_can_skip).is_err());

        let mut missing_image = scenarios;
        missing_image.get_mut("second").expect("second").1["environment"]
            .as_object_mut()
            .expect("environment object")
            .remove("gramine_image_id");
        assert!(scenario_run_identity(&missing_image).is_err());
    }
}
