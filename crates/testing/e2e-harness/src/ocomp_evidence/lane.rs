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

use super::{
    capture_source_identity, discover, hash_file, publish_assertions, publish_manifest,
    publish_member, AssertionRecordV1, AssertionStatus, EvidenceMode, MemberDigestV1,
    PlanningLedger, RunManifestV1, RUNTIME_SCHEMA_VERSION,
};

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
        "Completed result-vote replay is idempotent and changed binding is rejected",
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
    ensure!(
        lane == "OCM-PUBLIC",
        "lane assembly is not implemented for {lane}"
    );
    ensure!(
        !evidence_dir.join("run-manifest.json").exists(),
        "lane manifest already exists in {}",
        evidence_dir.display()
    );
    let source = capture_source_identity(repo)?;
    let mut scenarios = load_scenarios(evidence_dir)?;
    let mut members = scenario_members(evidence_dir, &scenarios)?;
    let mut assertions = Vec::with_capacity(PUBLIC_SCENARIOS.len());
    let mut observed_times = Vec::with_capacity(PUBLIC_SCENARIOS.len());

    let run_id = format!("ocomp-public-{}", unix_millis());
    for (test_id, scenario_name, oracle) in PUBLIC_SCENARIOS {
        let (path, scenario) = scenarios
            .remove(scenario_name)
            .ok_or_else(|| eyre::eyre!("missing required public scenario {scenario_name}"))?;
        validate_common_scenario(&scenario, &source.sha)?;
        match test_id {
            "OCM-PUB-001" => validate_applied_public_path(&scenario)?,
            "OCM-PUB-002" => {
                validate_applied_public_path(&scenario)?;
                let public = public_path(&scenario)?;
                ensure!(
                    bool_field(public, "non_quorum_changed_binding_reverted")?
                        && bool_field(public, "non_quorum_state_unchanged")?,
                    "public mutation scenario did not prove scoped rollback"
                );
            }
            "OCM-PUB-003" => validate_expired_public_path(&scenario)?,
            "OCM-PUB-004" => {
                validate_applied_public_path(&scenario)?;
                let public = public_path(&scenario)?;
                ensure!(
                    bool_field(public, "exact_completed_retry_succeeded")?
                        && bool_field(public, "changed_completed_binding_reverted")?
                        && bool_field(public, "completed_state_unchanged")?,
                    "completed public replay did not prove idempotency and rejection"
                );
            }
            _ => unreachable!("closed public scenario table"),
        }

        let expected_path = format!("expected/{test_id}.json");
        let expected = serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "test_id": test_id,
            "scenario": scenario_name,
            "oracle": oracle,
            "required_result": "passed",
        }))?;
        members.push(publish_member(evidence_dir, &expected_path, &expected)?);
        let observed_at = u64_field(&scenario, "recorded_at_unix_ms")?;
        observed_times.push(observed_at);
        assertions.push(AssertionRecordV1 {
            assertion_id: format!("{run_id}-{test_id}"),
            test_id: test_id.to_owned(),
            status: AssertionStatus::Pass,
            oracle: oracle.to_owned(),
            expected_artifact_refs: vec![expected_path],
            actual_artifact_refs: vec![path],
            observed_at,
            run_id: run_id.clone(),
            source_sha: source.sha.clone(),
            attempt: 1,
        });
    }
    ensure!(
        scenarios
            .values()
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
    let mut sections = BTreeMap::new();
    for section in &ledger.runtime_evidence.required_sections {
        sections.insert(
            section.clone(),
            json!({
                "lane": lane,
                "validated_scenarios": scenario_paths,
                "assertion_count": assertions.len(),
            }),
        );
    }
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
        binaries.is_object() && binaries.as_object().is_some_and(|value| value.len() == 3),
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
    ensure!(slots == [0, 1, 2, 3], "public accountability is not 4/4");

    let transactions = array_field(public, "result_vote_transactions")?;
    let successful_transactions = transactions
        .iter()
        .filter(|transaction| transaction.get("success").and_then(Value::as_bool) == Some(true))
        .collect::<Vec<_>>();
    ensure!(
        successful_transactions.len() == 4,
        "public path did not retain exactly four successful validator votes"
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
        signers.len() == 4 && saw_activation_tx,
        "public votes lack four signers or the q-forming transaction"
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
    use super::{validate_applied_public_path, validate_expired_public_path};
    use serde_json::{json, Value};

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
}
