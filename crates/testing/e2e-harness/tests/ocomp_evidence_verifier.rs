// OCOMP-TEST-ID: OCM-EVD-001

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use outbe_e2e_harness::ocomp_evidence::{
    capture_source_identity, discover, publish_assertions, publish_manifest, publish_member,
    publish_report, task_progress_report, verify_manifest, verify_retained_semantics,
    AssertionRecordV1, AssertionStatus, ClosureReportV1, EvidenceMode, PlanningLedger,
    RunManifestV1, RUNTIME_SCHEMA_VERSION, TEST_ID_MARKER,
};
use serde_json::json;
use tempfile::TempDir;

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn planning_ledger_path() -> PathBuf {
    repository_root().join("outbe-plan/off-chain-poc-evidence-ledger.yaml")
}

#[test]
fn checked_in_ledger_has_exact_owned_references() {
    let ledger = PlanningLedger::parse(&planning_ledger_path()).expect("checked-in ledger");
    assert_eq!(ledger.tests.len(), 37);
    assert_eq!(ledger.task_ownership.len(), 28);
    assert_eq!(ledger.task_dependencies.len(), 28);
    assert_eq!(ledger.task_commands.len(), 28);
    assert_eq!(ledger.requirements.adr.len(), 56);
    assert_eq!(
        ledger.deferred.keys().cloned().collect::<Vec<_>>(),
        ["PFS-002-07", "PFS-002-08"]
    );
}

#[test]
fn unknown_task_dependency_is_rejected() {
    let original = std::fs::read_to_string(planning_ledger_path()).expect("ledger text");
    let mutated = original.replacen("OCM-01: [OCM-00]", "OCM-01: [OCM-99]", 1);
    assert_ne!(mutated, original);
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("unknown-dependency.yaml");
    std::fs::write(&path, mutated).expect("mutated ledger");
    assert_error_contains(PlanningLedger::parse(&path), "unknown dependency OCM-99");
}

#[test]
fn cyclic_task_dependency_is_rejected() {
    let original = std::fs::read_to_string(planning_ledger_path()).expect("ledger text");
    let mutated = original.replacen("OCM-00: []", "OCM-00: [OCM-27]", 1);
    assert_ne!(mutated, original);
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("cyclic-dependency.yaml");
    std::fs::write(&path, mutated).expect("mutated ledger");
    assert_error_contains(
        PlanningLedger::parse(&path),
        "task dependency graph contains a cycle",
    );
}

#[test]
fn duplicate_yaml_key_is_rejected_before_deserialization() {
    let original = std::fs::read_to_string(planning_ledger_path()).expect("ledger text");
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("duplicate.yaml");
    std::fs::write(&path, format!("schema_version: 1\n{original}")).expect("duplicate ledger");
    let error = PlanningLedger::parse(&path).expect_err("duplicate key must fail");
    assert!(
        format!("{error:#}").contains("duplicate YAML mapping key"),
        "{error:#}"
    );
}

#[test]
fn unknown_requirement_reference_is_rejected() {
    let original = std::fs::read_to_string(planning_ledger_path()).expect("ledger text");
    let mutated = original.replacen("tests: [OCM-EVD-001]", "tests: [OCM-NOPE-999]", 1);
    assert_ne!(mutated, original);
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("unknown-reference.yaml");
    std::fs::write(&path, mutated).expect("mutated ledger");
    let error = PlanningLedger::parse(&path).expect_err("unknown test must fail");
    assert!(format!("{error:#}").contains("unknown test"), "{error:#}");
}

#[test]
fn complete_synthetic_bundle_passes_manifest_coverage_layer() {
    let fixture = Fixture::new(FixtureMutation::None);
    let report = fixture.verify().expect("complete fixture verifies");
    assert!(report.passed(), "{report:#?}");
    assert_eq!(report.passed_test_ids.len(), fixture.ledger.tests.len());
    assert!(report.missing_test_ids.is_empty());
    assert!(report.requirement_gaps.is_empty());
}

#[test]
fn self_declared_closure_without_embedded_lane_evidence_is_rejected() {
    let fixture = Fixture::new(FixtureMutation::None);
    fixture.verify().expect("generic manifest layer");
    assert_error_contains(
        verify_retained_semantics(&fixture.repo, &fixture.ledger, &fixture.manifest_path),
        "embedded lane",
    );
}

#[test]
fn missing_assertion_remains_visible_and_fails_closure() {
    let fixture = Fixture::new(FixtureMutation::OmitOneAssertion);
    let report = fixture
        .verify()
        .expect("structurally valid incomplete fixture");
    assert!(!report.passed());
    assert!(!report.missing_test_ids.is_empty());
    assert!(!report.requirement_gaps.is_empty());
}

#[test]
fn mixed_revision_is_rejected() {
    let fixture = Fixture::new(FixtureMutation::MixedRevision);
    assert_error_contains(fixture.verify(), "another source revision");
}

#[test]
fn skipped_test_is_rejected() {
    let fixture = Fixture::new(FixtureMutation::Skipped);
    assert_error_contains(fixture.verify(), "skipped OCOMP tests");
}

#[test]
fn retried_away_test_is_rejected() {
    let fixture = Fixture::new(FixtureMutation::Retried);
    assert_error_contains(fixture.verify(), "automatically retried OCOMP tests");
}

#[test]
fn stale_assertion_is_rejected() {
    let fixture = Fixture::new(FixtureMutation::Stale);
    assert_error_contains(fixture.verify(), "outside the run interval");
}

#[test]
fn second_attempt_is_rejected() {
    let fixture = Fixture::new(FixtureMutation::SecondAttempt);
    assert_error_contains(fixture.verify(), "was retried");
}

#[test]
fn dirty_source_cannot_claim_closure() {
    let fixture = Fixture::new(FixtureMutation::DirtySource);
    assert_error_contains(fixture.verify(), "requires a clean tracked and untracked");
}

#[test]
fn forbidden_secret_class_is_rejected() {
    let fixture = Fixture::new(FixtureMutation::ForbiddenSecret);
    assert_error_contains(fixture.verify(), "contains forbidden content");
}

#[test]
fn hash_corruption_is_rejected() {
    let fixture = Fixture::new(FixtureMutation::None);
    std::fs::write(
        fixture.bundle.path().join("protocol/oracle.json"),
        b"corrupt",
    )
    .expect("corrupt retained member");
    assert_error_contains(fixture.verify(), "member hash/length mismatch");
}

#[test]
fn report_bytes_are_deterministic() {
    let fixture = Fixture::new(FixtureMutation::None);
    let report = fixture.verify().expect("complete fixture");
    let first = TempDir::new().expect("first report dir");
    let second = TempDir::new().expect("second report dir");
    publish_report(first.path(), &report).expect("first report");
    publish_report(second.path(), &report).expect("second report");
    for name in [
        "closure-report.json",
        "closure-report.md",
        "closure-report.sha256",
    ] {
        assert_eq!(
            std::fs::read(first.path().join(name)).expect("first bytes"),
            std::fs::read(second.path().join(name)).expect("second bytes"),
            "{name} differs"
        );
    }
}

#[test]
fn immutable_manifest_cannot_be_republished() {
    let fixture = Fixture::new(FixtureMutation::None);
    let error = publish_manifest(fixture.bundle.path(), &fixture.manifest)
        .expect_err("immutable manifest replacement must fail");
    assert!(
        format!("{error:#}").contains("refusing to replace immutable evidence"),
        "{error:#}"
    );
}

#[test]
fn task_progress_passes_all_discovered_ids_and_leaves_future_ids_missing() {
    let ledger = PlanningLedger::parse(&planning_ledger_path()).expect("ledger");
    let discovery = discover(&repository_root(), &ledger).expect("repository discovery");
    let passed = discovery.discovered.clone();
    assert!(passed.contains(&"OCM-EVD-001".to_owned()));
    assert!(passed.contains(&"OCM-SEM-001".to_owned()));
    let report =
        task_progress_report(&ledger, "OCM-01", discovery, &passed).expect("OCM-01 task progress");
    assert!(report.passed());
    assert_eq!(report.mode, "task_progress");
    assert_eq!(report.task_id.as_deref(), Some("OCM-01"));
    assert_eq!(report.passed_test_ids, passed);
    assert_eq!(
        report.missing_test_ids.len(),
        ledger.tests.len() - report.passed_test_ids.len()
    );
}

fn assert_error_contains<T: std::fmt::Debug>(result: eyre::Result<T>, needle: &str) {
    let error = result.expect_err("verification must fail");
    assert!(format!("{error:#}").contains(needle), "{error:#}");
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixtureMutation {
    None,
    OmitOneAssertion,
    MixedRevision,
    Skipped,
    Retried,
    Stale,
    SecondAttempt,
    DirtySource,
    ForbiddenSecret,
}

struct Fixture {
    _root: TempDir,
    repo: PathBuf,
    bundle: TempDir,
    ledger: PlanningLedger,
    manifest: RunManifestV1,
    manifest_path: PathBuf,
}

impl Fixture {
    fn new(mutation: FixtureMutation) -> Self {
        let ledger = PlanningLedger::parse(&planning_ledger_path()).expect("ledger");
        let root = TempDir::new().expect("fixture root");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).expect("fixture repo");
        materialize_discovery_sources(&repo, &ledger, mutation);
        initialize_fixture_repository(&repo, mutation);
        let discovery = discover(&repo, &ledger).expect("synthetic discovery");

        let bundle = TempDir::new().expect("bundle root");
        let oracle_member = publish_member(
            bundle.path(),
            "protocol/oracle.json",
            b"{\"oracle\":\"fixture\"}\n",
        )
        .expect("oracle member");

        let run_id = "synthetic-ocomp-evidence-v1".to_owned();
        let source = capture_source_identity(&repo).expect("fixture source identity");
        let source_sha = source.sha.clone();
        let mut assertions = Vec::new();
        for (index, (test_id, test)) in ledger.tests.iter().enumerate() {
            if mutation == FixtureMutation::OmitOneAssertion && index == 0 {
                continue;
            }
            assertions.push(AssertionRecordV1 {
                assertion_id: format!("assertion-{index:03}"),
                test_id: test_id.clone(),
                status: AssertionStatus::Pass,
                oracle: test.oracles[0].clone(),
                expected_artifact_refs: vec![oracle_member.path.clone()],
                actual_artifact_refs: vec![oracle_member.path.clone()],
                observed_at: if mutation == FixtureMutation::Stale && index == 0 {
                    99
                } else {
                    150
                },
                run_id: run_id.clone(),
                source_sha: if mutation == FixtureMutation::MixedRevision && index == 0 {
                    "b".repeat(40)
                } else {
                    source_sha.clone()
                },
                attempt: if mutation == FixtureMutation::SecondAttempt && index == 0 {
                    2
                } else {
                    1
                },
            });
        }
        let assertion_member = publish_assertions(bundle.path(), "assertions.jsonl", &assertions)
            .expect("assertions member");
        let discovery_bytes = serde_json::to_vec_pretty(&discovery).expect("discovery JSON");
        let discovery_member = publish_member(bundle.path(), "discovery.json", &discovery_bytes)
            .expect("discovery member");
        let mut members = vec![oracle_member, assertion_member, discovery_member];
        members.sort_by(|left, right| left.path.cmp(&right.path));

        let mut sections = ledger
            .runtime_evidence
            .required_sections
            .iter()
            .map(|section| {
                (
                    section.clone(),
                    json!({ "synthetic_fixture_section": section }),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if mutation == FixtureMutation::ForbiddenSecret {
            sections.insert(
                "source_and_toolchain".to_owned(),
                json!({ "private_keys": "must-not-be-retained" }),
            );
        }
        let manifest = RunManifestV1 {
            schema_version: RUNTIME_SCHEMA_VERSION,
            run_id,
            mode: EvidenceMode::PocClosure,
            started_at: 100,
            finished_at: 200,
            source,
            discovery,
            assertions_path: "assertions.jsonl".to_owned(),
            members,
            sections,
        };
        let manifest_path =
            publish_manifest(bundle.path(), &manifest).expect("publish manifest last");
        Self {
            _root: root,
            repo,
            bundle,
            ledger,
            manifest,
            manifest_path,
        }
    }

    fn verify(&self) -> eyre::Result<ClosureReportV1> {
        verify_manifest(&self.repo, &self.ledger, &self.manifest_path)
    }
}

fn initialize_fixture_repository(repo: &Path, mutation: FixtureMutation) {
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"ocomp-evidence-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("fixture Cargo.toml");
    std::fs::write(
        repo.join("Cargo.lock"),
        "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname = \"ocomp-evidence-fixture\"\nversion = \"0.0.0\"\n",
    )
    .expect("fixture Cargo.lock");
    std::fs::copy(
        repository_root().join("rust-toolchain.toml"),
        repo.join("rust-toolchain.toml"),
    )
    .expect("fixture rust toolchain");
    std::fs::create_dir(repo.join("src")).expect("fixture source directory");
    std::fs::write(
        repo.join("src/lib.rs"),
        b"pub const FIXTURE: &str = \"ocomp\";\n",
    )
    .expect("fixture Rust target");
    std::fs::write(repo.join("dirty-sentinel"), b"clean\n").expect("fixture dirty sentinel");
    run_git(repo, &["init", "--quiet"]);
    run_git(repo, &["add", "."]);
    run_git(
        repo,
        &[
            "-c",
            "user.name=OCOMP Fixture",
            "-c",
            "user.email=ocomp-fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    );
    if mutation == FixtureMutation::DirtySource {
        std::fs::write(repo.join("dirty-sentinel"), b"dirty\n").expect("dirty fixture source");
    }
}

fn run_git(repo: &Path, arguments: &[&str]) {
    let output = std::process::Command::new("git")
        .args(arguments)
        .current_dir(repo)
        .output()
        .expect("run fixture git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn materialize_discovery_sources(repo: &Path, ledger: &PlanningLedger, mutation: FixtureMutation) {
    let mut by_path = BTreeMap::<PathBuf, Vec<String>>::new();
    for (test_id, test) in &ledger.tests {
        by_path
            .entry(PathBuf::from(&test.planned_path))
            .or_default()
            .push(test_id.clone());
    }
    for (index, (relative, test_ids)) in by_path.into_iter().enumerate() {
        let path = repo.join(relative);
        std::fs::create_dir_all(path.parent().expect("planned path parent"))
            .expect("planned path directory");
        let mut contents = test_ids
            .iter()
            .map(|test_id| format!("// {TEST_ID_MARKER} {test_id}\n"))
            .collect::<String>();
        if index == 0 && mutation == FixtureMutation::Skipped {
            contents.push_str(&format!("// OCOMP-TEST-SKIP: {}\n", test_ids[0]));
        }
        if index == 0 && mutation == FixtureMutation::Retried {
            contents.push_str(&format!("// OCOMP-TEST-RETRY: {}\n", test_ids[0]));
        }
        std::fs::write(path, contents).expect("synthetic test source");
    }
}
