//! Exact Rust command lanes for OCOMP FAST and INT closure evidence.
//!
//! The command plan is closed and PoC-specific. A runner records the first
//! attempt, exact argv, exit status and immutable stdout/stderr for each batch.
//! Assembly independently compares that receipt with this plan and with the
//! ledger-owned stable IDs before it is allowed to emit PASS assertions.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{bail, ensure, Result, WrapErr};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    capture_source_identity, discover, hash_file, publish_assertions, publish_manifest,
    publish_member, AssertionRecordV1, AssertionStatus, EvidenceMode, MemberDigestV1,
    PlanningLedger, RunManifestV1, SourceIdentityV1, RUNTIME_SCHEMA_VERSION,
};

const COMMAND_RUN_FILE: &str = "command-lane-run-v1.json";
const REQUIRED_ARTIFACTS: [(&str, &str); 6] = [
    ("outbe_chain", "outbe-chain"),
    ("outbe_cli", "outbe-cli"),
    ("outbe_e2e", "outbe-e2e"),
    ("outbe_keygen", "outbe-keygen"),
    ("outbe_ocomp", "outbe-ocomp"),
    ("outbe_tee_enclave", "outbe-tee-enclave"),
];

/// One closed command and the stable IDs whose executable tests it runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandBatchV1 {
    pub command_id: &'static str,
    pub arguments: &'static [&'static str],
    pub test_ids: &'static [&'static str],
}

/// Immutable first-attempt observation of one command batch.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandObservationV1 {
    pub command_id: String,
    pub program: String,
    pub arguments: Vec<String>,
    pub test_ids: Vec<String>,
    pub started_at: u64,
    pub finished_at: u64,
    pub exit_code: Option<i32>,
    pub stdout: MemberDigestV1,
    pub stderr: MemberDigestV1,
}

/// Retained FAST/INT lane execution receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandLaneRunV1 {
    pub schema_version: u32,
    pub lane: String,
    pub attempt: u32,
    pub source: SourceIdentityV1,
    pub started_at: u64,
    pub finished_at: u64,
    pub artifact_set: BTreeMap<String, MemberDigestV1>,
    pub commands: Vec<CommandObservationV1>,
}

/// Execute the exact first-attempt FAST or INT command plan and retain output.
pub fn run_command_lane(
    repo: &Path,
    ledger: &PlanningLedger,
    lane: &str,
    artifact_set: &Path,
    evidence_dir: &Path,
) -> Result<PathBuf> {
    ledger.validate()?;
    let plan = command_plan(lane)?;
    ensure!(
        !evidence_dir.exists(),
        "command lane evidence directory already exists: {}",
        evidence_dir.display()
    );
    std::fs::create_dir_all(evidence_dir)
        .wrap_err_with(|| format!("create command lane evidence {}", evidence_dir.display()))?;

    let source = capture_source_identity(repo)?;
    ensure!(
        !source.tracked_dirty && !source.untracked_dirty,
        "exact command lane requires a clean tracked and untracked source tree"
    );
    let artifact_set = capture_artifact_set(artifact_set)?;
    let started_at = unix_millis();
    let mut commands = Vec::with_capacity(plan.len());
    let mut first_failure = None;
    for batch in plan {
        let command_started_at = unix_millis();
        let stdout_path = format!("commands/{}.stdout", batch.command_id);
        let stderr_path = format!("commands/{}.stderr", batch.command_id);
        let stdout = create_output(evidence_dir, &stdout_path)?;
        let stderr = create_output(evidence_dir, &stderr_path)?;
        let status = Command::new("cargo")
            .args(batch.arguments)
            .current_dir(repo)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .status()
            .wrap_err_with(|| format!("start exact command batch {}", batch.command_id))?;
        sync_output(evidence_dir, &stdout_path)?;
        sync_output(evidence_dir, &stderr_path)?;
        commands.push(CommandObservationV1 {
            command_id: batch.command_id.to_owned(),
            program: "cargo".to_owned(),
            arguments: batch
                .arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
            test_ids: batch
                .test_ids
                .iter()
                .map(|test_id| (*test_id).to_owned())
                .collect(),
            started_at: command_started_at,
            finished_at: unix_millis().max(command_started_at),
            exit_code: status.code(),
            stdout: relative_digest(evidence_dir, &stdout_path)?,
            stderr: relative_digest(evidence_dir, &stderr_path)?,
        });
        if !status.success() {
            first_failure = Some(batch.command_id);
            break;
        }
    }
    let finished_at = unix_millis().max(started_at);
    ensure!(
        capture_source_identity(repo)? == source,
        "source/toolchain identity changed while command lane was running"
    );
    let run = CommandLaneRunV1 {
        schema_version: RUNTIME_SCHEMA_VERSION,
        lane: lane.to_owned(),
        attempt: 1,
        source,
        started_at,
        finished_at,
        artifact_set,
        commands,
    };
    let mut bytes = serde_json::to_vec_pretty(&run)?;
    bytes.push(b'\n');
    let run_path = evidence_dir.join(COMMAND_RUN_FILE);
    publish_member(evidence_dir, COMMAND_RUN_FILE, &bytes)?;
    if let Some(command_id) = first_failure {
        bail!(
            "exact command batch {command_id} failed; first-attempt evidence retained at {}",
            run_path.display()
        );
    }
    Ok(run_path)
}

/// Independently validate one completed FAST/INT receipt and publish its lane manifest.
pub fn assemble_command_lane(
    repo: &Path,
    ledger: &PlanningLedger,
    lane: &str,
    evidence_dir: &Path,
) -> Result<PathBuf> {
    ensure!(
        !evidence_dir.join("run-manifest.json").exists(),
        "lane manifest already exists in {}",
        evidence_dir.display()
    );
    let (source, run) = verify_command_lane_evidence(repo, ledger, lane, evidence_dir)?;

    let run_id = format!(
        "ocomp-{}-{}",
        lane.trim_start_matches("OCM-").to_ascii_lowercase(),
        unix_millis()
    );
    let mut members = vec![relative_digest(evidence_dir, COMMAND_RUN_FILE)?];
    for command in &run.commands {
        members.push(rehash_relative(evidence_dir, &command.stdout)?);
        members.push(rehash_relative(evidence_dir, &command.stderr)?);
    }

    let mut assertions = Vec::new();
    for command in &run.commands {
        for test_id in &command.test_ids {
            let test = ledger
                .tests
                .get(test_id)
                .ok_or_else(|| eyre::eyre!("command receipt references unknown test {test_id}"))?;
            let expected_path = format!("expected/{test_id}.json");
            let expected = serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "lane": lane,
                "test_id": test_id,
                "command_id": command.command_id,
                "program": command.program,
                "arguments": command.arguments,
                "required_exit_code": 0,
                "attempt": 1,
            }))?;
            members.push(publish_member(evidence_dir, &expected_path, &expected)?);
            assertions.push(AssertionRecordV1 {
                assertion_id: format!("{run_id}-{test_id}"),
                test_id: test_id.clone(),
                status: AssertionStatus::Pass,
                oracle: test.oracles[0].clone(),
                expected_artifact_refs: vec![expected_path],
                actual_artifact_refs: vec![
                    COMMAND_RUN_FILE.to_owned(),
                    command.stdout.path.clone(),
                    command.stderr.path.clone(),
                ],
                observed_at: command.finished_at,
                run_id: run_id.clone(),
                source_sha: source.sha.clone(),
                attempt: 1,
            });
        }
    }
    assertions.sort_by(|left, right| left.test_id.cmp(&right.test_id));
    let assertions_path = "assertions.jsonl";
    members.push(publish_assertions(
        evidence_dir,
        assertions_path,
        &assertions,
    )?);
    members.sort_by(|left, right| left.path.cmp(&right.path));

    let discovery = discover(repo, ledger)?;
    let mut sections = BTreeMap::new();
    for section in &ledger.runtime_evidence.required_sections {
        let value = match section.as_str() {
            "source_and_toolchain" => json!(&source),
            "exact_binary_hashes" => json!(&run.artifact_set),
            "test_discovery" => json!(&discovery),
            "skip_todo_quarantine_retry_and_timeout_records" => json!({
                "attempt": run.attempt,
                "automatic_retries": 0,
                "skipped": &discovery.skipped,
                "todo": &discovery.todo,
                "quarantined": &discovery.quarantined,
                "retried": &discovery.retried,
            }),
            "member_hash_index_and_retention" => json!({
                "publish_last": true,
                "members": &members,
            }),
            _ => json!({
                "lane": lane,
                "command_receipt": COMMAND_RUN_FILE,
                "command_count": run.commands.len(),
                "assertion_count": assertions.len(),
            }),
        };
        sections.insert(section.clone(), value);
    }
    publish_manifest(
        evidence_dir,
        &RunManifestV1 {
            schema_version: RUNTIME_SCHEMA_VERSION,
            run_id,
            mode: EvidenceMode::Lane {
                lane: lane.to_owned(),
            },
            started_at: run.started_at,
            finished_at: run.finished_at,
            source,
            discovery,
            assertions_path: assertions_path.to_owned(),
            members,
            sections,
        },
    )
}

pub(crate) fn verify_command_lane_evidence(
    repo: &Path,
    ledger: &PlanningLedger,
    lane: &str,
    evidence_dir: &Path,
) -> Result<(SourceIdentityV1, CommandLaneRunV1)> {
    let plan = command_plan(lane)?;
    let source = capture_source_identity(repo)?;
    let run_path = evidence_dir.join(COMMAND_RUN_FILE);
    let run: CommandLaneRunV1 = serde_json::from_slice(
        &std::fs::read(&run_path)
            .wrap_err_with(|| format!("read command lane receipt {}", run_path.display()))?,
    )
    .wrap_err("decode command lane receipt")?;
    validate_command_run(repo, ledger, lane, &source, &run, plan, evidence_dir)?;
    Ok((source, run))
}

pub(crate) fn verify_command_lane_manifest_semantics(
    repo: &Path,
    ledger: &PlanningLedger,
    lane: &str,
    manifest: &RunManifestV1,
    evidence_dir: &Path,
) -> Result<()> {
    let (source, run) = verify_command_lane_evidence(repo, ledger, lane, evidence_dir)?;
    ensure!(
        manifest.source == source
            && manifest.sections.get("exact_binary_hashes") == Some(&json!(&run.artifact_set)),
        "command lane manifest identity differs from its independently verified receipt"
    );
    let assertions = read_manifest_assertions(evidence_dir, manifest)?;
    let mut by_test = BTreeMap::new();
    for assertion in assertions {
        ensure!(
            by_test
                .insert(assertion.test_id.clone(), assertion)
                .is_none(),
            "command lane contains duplicate assertion for one stable test"
        );
    }
    for command in &run.commands {
        for test_id in &command.test_ids {
            let assertion = by_test
                .remove(test_id)
                .ok_or_else(|| eyre::eyre!("command lane lacks assertion for {test_id}"))?;
            ensure!(
                assertion.expected_artifact_refs == [format!("expected/{test_id}.json")]
                    && assertion.actual_artifact_refs
                        == [
                            COMMAND_RUN_FILE.to_owned(),
                            command.stdout.path.clone(),
                            command.stderr.path.clone(),
                        ]
                    && assertion.observed_at == command.finished_at,
                "command lane assertion {test_id} is not bound to its exact command receipt"
            );
        }
    }
    ensure!(
        by_test.is_empty(),
        "command lane manifest contains assertions outside its exact command plan"
    );
    Ok(())
}

fn read_manifest_assertions(
    evidence_dir: &Path,
    manifest: &RunManifestV1,
) -> Result<Vec<AssertionRecordV1>> {
    let text = std::fs::read_to_string(evidence_dir.join(&manifest.assertions_path))
        .wrap_err("read command lane assertions")?;
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            ensure!(
                !line.trim().is_empty(),
                "blank command assertion line {}",
                index + 1
            );
            serde_json::from_str(line)
                .wrap_err_with(|| format!("decode command assertion line {}", index + 1))
        })
        .collect()
}

fn validate_command_run(
    _repo: &Path,
    ledger: &PlanningLedger,
    lane: &str,
    source: &SourceIdentityV1,
    run: &CommandLaneRunV1,
    plan: &[CommandBatchV1],
    evidence_dir: &Path,
) -> Result<()> {
    ensure!(
        run.schema_version == RUNTIME_SCHEMA_VERSION
            && run.lane == lane
            && run.attempt == 1
            && &run.source == source
            && run.started_at <= run.finished_at,
        "command lane receipt has the wrong schema, lane, attempt, source or interval"
    );
    ensure!(
        run.commands.len() == plan.len(),
        "command lane receipt is incomplete"
    );
    validate_artifact_set(&run.artifact_set)?;

    let mut observed_test_ids = BTreeSet::new();
    for (observed, expected) in run.commands.iter().zip(plan) {
        let expected_arguments = expected
            .arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect::<Vec<_>>();
        let expected_test_ids = expected
            .test_ids
            .iter()
            .map(|test_id| (*test_id).to_owned())
            .collect::<Vec<_>>();
        ensure!(
            observed.command_id == expected.command_id
                && observed.program == "cargo"
                && observed.arguments == expected_arguments
                && observed.test_ids == expected_test_ids
                && observed.started_at >= run.started_at
                && observed.finished_at >= observed.started_at
                && observed.finished_at <= run.finished_at
                && observed.exit_code == Some(0),
            "command batch {} differs from the exact first-attempt plan or failed",
            expected.command_id
        );
        ensure!(
            observed.stdout.path == format!("commands/{}.stdout", expected.command_id)
                && observed.stderr.path == format!("commands/{}.stderr", expected.command_id),
            "command batch {} uses unexpected output members",
            expected.command_id
        );
        let _ = rehash_relative(evidence_dir, &observed.stdout)?;
        let _ = rehash_relative(evidence_dir, &observed.stderr)?;
        for test_id in &observed.test_ids {
            ensure!(
                observed_test_ids.insert(test_id.clone()),
                "stable test {test_id} is claimed by multiple command batches"
            );
        }
    }
    ensure!(
        observed_test_ids == ledger.lane_test_ids(lane),
        "command batches do not partition every {lane} stable test"
    );
    Ok(())
}

fn command_plan(lane: &str) -> Result<&'static [CommandBatchV1]> {
    match lane {
        "OCM-FAST" => Ok(FAST_PLAN),
        "OCM-INT" => Ok(INT_PLAN),
        _ => {
            bail!("command lane execution is not implemented for {lane}");
        }
    }
}

fn capture_artifact_set(root: &Path) -> Result<BTreeMap<String, MemberDigestV1>> {
    REQUIRED_ARTIFACTS
        .iter()
        .map(|(name, file)| {
            let digest = hash_file(&root.join(file))
                .wrap_err_with(|| format!("hash exact artifact {file}"))?;
            Ok(((*name).to_owned(), digest))
        })
        .collect()
}

fn validate_artifact_set(artifacts: &BTreeMap<String, MemberDigestV1>) -> Result<()> {
    ensure!(
        artifacts
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == REQUIRED_ARTIFACTS
                .iter()
                .map(|(name, _)| *name)
                .collect::<BTreeSet<_>>(),
        "command lane artifact set has the wrong binary names"
    );
    for (name, artifact) in artifacts {
        let actual = hash_file(Path::new(&artifact.path))
            .wrap_err_with(|| format!("rehash retained exact artifact {name}"))?;
        ensure!(
            actual.length == artifact.length && actual.sha256 == artifact.sha256,
            "retained exact artifact {name} changed after the command run"
        );
    }
    Ok(())
}

fn create_output(root: &Path, relative: &str) -> Result<File> {
    let path = root.join(relative);
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("command output has no parent directory"))?;
    std::fs::create_dir_all(parent)
        .wrap_err_with(|| format!("create command output directory {}", parent.display()))?;
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .wrap_err_with(|| format!("create immutable command output {}", path.display()))
}

fn sync_output(root: &Path, relative: &str) -> Result<()> {
    File::open(root.join(relative))
        .and_then(|file| file.sync_all())
        .wrap_err_with(|| format!("sync command output {relative}"))
}

fn relative_digest(root: &Path, relative: &str) -> Result<MemberDigestV1> {
    let mut digest = hash_file(&root.join(relative))?;
    digest.path = relative.to_owned();
    Ok(digest)
}

fn rehash_relative(root: &Path, expected: &MemberDigestV1) -> Result<MemberDigestV1> {
    let actual = relative_digest(root, &expected.path)?;
    ensure!(
        actual.length == expected.length && actual.sha256 == expected.sha256,
        "command output {} changed after execution",
        expected.path
    );
    Ok(actual)
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

const FAST_PLAN: &[CommandBatchV1] = &[
    CommandBatchV1 {
        command_id: "fast-evidence",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "outbe-e2e-harness",
            "--test",
            "ocomp_evidence_verifier",
        ],
        test_ids: &["OCM-EVD-001"],
    },
    CommandBatchV1 {
        command_id: "fast-protocol",
        arguments: &["test", "--locked", "-p", "outbe-ocomp-protocol"],
        test_ids: &[
            "OCM-BYT-001",
            "OCM-BYT-002",
            "OCM-CAP-001",
            "OCM-FIN-001",
            "OCM-BND-003",
        ],
    },
    CommandBatchV1 {
        command_id: "fast-lysis",
        arguments: &["test", "--locked", "-p", "outbe-lysis"],
        test_ids: &[
            "OCM-SEM-001",
            "OCM-SEM-002",
            "OCM-APL-001",
            "OCM-BND-001",
            "OCM-BND-002",
        ],
    },
    CommandBatchV1 {
        command_id: "fast-metadosis",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "outbe-metadosis",
            "--test",
            "ocomp_fsm_model",
        ],
        test_ids: &["OCM-FSM-001"],
    },
    CommandBatchV1 {
        command_id: "fast-clippy",
        arguments: &[
            "clippy",
            "--locked",
            "-p",
            "outbe-e2e-harness",
            "-p",
            "outbe-ocomp-protocol",
            "-p",
            "outbe-lysis",
            "-p",
            "outbe-metadosis",
            "-p",
            "outbe-evm",
            "-p",
            "outbe-node",
            "-p",
            "outbe-ocomp",
            "--features",
            "outbe-e2e-harness/ocomp-integration",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        test_ids: &[],
    },
];

const INT_PLAN: &[CommandBatchV1] = &[
    CommandBatchV1 {
        command_id: "int-evm",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "outbe-evm",
            "--test",
            "ocomp_request_lifecycle",
        ],
        test_ids: &["OCM-REQ-001"],
    },
    CommandBatchV1 {
        command_id: "int-metadosis-semantic",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "outbe-metadosis",
            "--lib",
            "ocomp_semantic_migrations",
        ],
        test_ids: &["OCM-VOT-001", "OCM-APL-002", "OCM-TIM-001", "OCM-E2E-006"],
    },
    CommandBatchV1 {
        command_id: "int-metadosis-empty-day",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "outbe-metadosis",
            "active_ocomp_profile_preserves_the_empty_day_compatibility_branch",
        ],
        test_ids: &["OCM-E2E-002"],
    },
    CommandBatchV1 {
        command_id: "int-tribute-duplicate",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "outbe-tribute",
            "--test",
            "runtime_reads",
            "issue_is_visible_and_rejects_duplicates_before_projection",
        ],
        test_ids: &["OCM-E2E-004"],
    },
    CommandBatchV1 {
        command_id: "int-node",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "outbe-node",
            "--lib",
            "ocomp::tests",
        ],
        test_ids: &["OCM-PIN-001", "OCM-SIG-001"],
    },
    CommandBatchV1 {
        command_id: "int-ocomp-core",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "outbe-ocomp",
            "--test",
            "input_artifact_closure",
            "--test",
            "deterministic_schedules",
        ],
        test_ids: &["OCM-CAS-001", "OCM-DET-001"],
    },
    CommandBatchV1 {
        command_id: "int-ocomp-worker-process",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "outbe-ocomp",
            "--test",
            "worker_one_unit",
        ],
        test_ids: &["OCM-WRK-001"],
    },
    CommandBatchV1 {
        command_id: "int-ocomp-worker-transport",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "outbe-ocomp",
            "--lib",
            "worker_transport::tests",
        ],
        test_ids: &["OCM-WTR-001"],
    },
    CommandBatchV1 {
        command_id: "int-ocomp-worker-observability",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "outbe-ocomp",
            "--lib",
            "worker_observability::tests",
        ],
        test_ids: &["OCM-WOBS-001"],
    },
];

#[cfg(test)]
mod tests {
    use super::{
        capture_artifact_set, command_plan, relative_digest, validate_command_run,
        CommandLaneRunV1, CommandObservationV1, REQUIRED_ARTIFACTS, RUNTIME_SCHEMA_VERSION,
    };
    use crate::ocomp_evidence::{PlanningLedger, SourceIdentityV1};
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn repository_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn ledger() -> PlanningLedger {
        PlanningLedger::parse(
            &repository_root().join("outbe-plan/off-chain-poc-evidence-ledger.yaml"),
        )
        .expect("checked-in planning ledger")
    }

    fn source() -> SourceIdentityV1 {
        SourceIdentityV1 {
            sha: "a".repeat(40),
            tracked_dirty: false,
            untracked_dirty: false,
            cargo_lock_sha256: "b".repeat(64),
            rust_toolchain_sha256: "c".repeat(64),
            dependency_metadata_sha256: "d".repeat(64),
        }
    }

    fn valid_run(
        lane: &str,
    ) -> (
        TempDir,
        TempDir,
        PlanningLedger,
        SourceIdentityV1,
        CommandLaneRunV1,
    ) {
        let evidence = TempDir::new().expect("evidence tempdir");
        let artifacts = TempDir::new().expect("artifact tempdir");
        for (_, file) in REQUIRED_ARTIFACTS {
            std::fs::write(artifacts.path().join(file), format!("artifact:{file}"))
                .expect("artifact bytes");
        }
        let plan = command_plan(lane).expect("known command lane");
        let mut commands = Vec::new();
        for (ordinal, batch) in plan.iter().enumerate() {
            let stdout = format!("commands/{}.stdout", batch.command_id);
            let stderr = format!("commands/{}.stderr", batch.command_id);
            std::fs::create_dir_all(evidence.path().join("commands"))
                .expect("command output directory");
            std::fs::write(evidence.path().join(&stdout), format!("ok:{ordinal}")).expect("stdout");
            std::fs::write(evidence.path().join(&stderr), []).expect("stderr");
            commands.push(CommandObservationV1 {
                command_id: batch.command_id.to_owned(),
                program: "cargo".to_owned(),
                arguments: batch
                    .arguments
                    .iter()
                    .map(|argument| (*argument).to_owned())
                    .collect(),
                test_ids: batch
                    .test_ids
                    .iter()
                    .map(|test_id| (*test_id).to_owned())
                    .collect(),
                started_at: 110 + u64::try_from(ordinal).unwrap_or_default(),
                finished_at: 120 + u64::try_from(ordinal).unwrap_or_default(),
                exit_code: Some(0),
                stdout: relative_digest(evidence.path(), &stdout).expect("stdout digest"),
                stderr: relative_digest(evidence.path(), &stderr).expect("stderr digest"),
            });
        }
        let run = CommandLaneRunV1 {
            schema_version: RUNTIME_SCHEMA_VERSION,
            lane: lane.to_owned(),
            attempt: 1,
            source: source(),
            started_at: 100,
            finished_at: 200,
            artifact_set: capture_artifact_set(artifacts.path()).expect("artifact identities"),
            commands,
        };
        (evidence, artifacts, ledger(), source(), run)
    }

    #[test]
    fn exact_command_receipt_covers_every_fast_test_once() {
        let (evidence, _artifacts, ledger, source, run) = valid_run("OCM-FAST");
        validate_command_run(
            Path::new("."),
            &ledger,
            "OCM-FAST",
            &source,
            &run,
            command_plan("OCM-FAST").expect("plan"),
            evidence.path(),
        )
        .expect("valid command receipt");
    }

    #[test]
    fn exact_command_receipt_covers_every_integration_test_once() {
        let (evidence, _artifacts, ledger, source, run) = valid_run("OCM-INT");
        validate_command_run(
            Path::new("."),
            &ledger,
            "OCM-INT",
            &source,
            &run,
            command_plan("OCM-INT").expect("plan"),
            evidence.path(),
        )
        .expect("valid integration command receipt");
    }

    #[test]
    fn integration_plan_uses_only_current_executable_ocomp_targets() {
        let plan = command_plan("OCM-INT").expect("integration command plan");
        let arguments = plan
            .iter()
            .flat_map(|batch| batch.arguments.iter().copied())
            .collect::<Vec<_>>();
        for removed in [
            "finalized_discovery",
            "finalized_export",
            "control_protocol",
            "public_rpc_input_path",
        ] {
            assert!(
                !arguments.contains(&removed),
                "integration lane must not execute removed or ignored target {removed}"
            );
        }

        let planned_ids = plan
            .iter()
            .flat_map(|batch| batch.test_ids.iter().copied())
            .collect::<std::collections::BTreeSet<_>>();
        for current in ["OCM-WRK-001", "OCM-WTR-001", "OCM-WOBS-001"] {
            assert!(
                planned_ids.contains(current),
                "integration lane omits current focused evidence {current}"
            );
        }

        let ledger = ledger();
        for retired in ["OCM-DIS-001", "OCM-EXP-001", "OCM-CTL-001"] {
            assert!(
                ledger.retired_tests.contains_key(retired),
                "deleted transport-era claim {retired} must remain a tombstone"
            );
            assert!(
                !ledger.tests.contains_key(retired),
                "deleted transport-era claim {retired} remains executable"
            );
        }
    }

    #[test]
    fn command_receipt_rejects_failure_retry_drift_and_changed_artifact() {
        let (evidence, _artifacts, ledger, source, run) = valid_run("OCM-INT");
        let plan = command_plan("OCM-INT").expect("plan");

        let mut failed = run.clone();
        failed.commands[0].exit_code = Some(1);
        assert!(validate_command_run(
            Path::new("."),
            &ledger,
            "OCM-INT",
            &source,
            &failed,
            plan,
            evidence.path(),
        )
        .is_err());

        let mut retried = run.clone();
        retried.attempt = 2;
        assert!(validate_command_run(
            Path::new("."),
            &ledger,
            "OCM-INT",
            &source,
            &retried,
            plan,
            evidence.path(),
        )
        .is_err());

        let mut changed_arguments = run.clone();
        changed_arguments.commands[0]
            .arguments
            .push("--ignored".to_owned());
        assert!(validate_command_run(
            Path::new("."),
            &ledger,
            "OCM-INT",
            &source,
            &changed_arguments,
            plan,
            evidence.path(),
        )
        .is_err());

        let artifact = run.artifact_set.values().next().expect("retained artifact");
        std::fs::write(&artifact.path, "changed").expect("mutate retained artifact");
        assert!(validate_command_run(
            Path::new("."),
            &ledger,
            "OCM-INT",
            &source,
            &run,
            plan,
            evidence.path(),
        )
        .is_err());
    }
}
