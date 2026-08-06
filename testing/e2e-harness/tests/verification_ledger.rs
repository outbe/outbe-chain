use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use outbe_e2e_harness::ocomp_evidence::PlanningLedger;
use outbe_e2e_harness::verification_ledger::{
    validate_domain_evidence_manifest, verify_domain_evidence, verify_evidence_set,
    AssertionStatus, DomainEvidenceBundle, DomainEvidenceV1, EvidenceTestV1, MemberDigestV1,
    SourceIdentityV1, TestDiscoveryV1, VerificationLedger,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn checked_in_index_loads_both_packs_without_changing_ocomp_catalog() {
    let root = repository_root();
    let generic =
        VerificationLedger::parse(&root.join("outbe-plan/verification-ledger.yaml")).unwrap();
    let ocomp =
        PlanningLedger::parse(&root.join("outbe-plan/off-chain-poc-evidence-ledger.yaml")).unwrap();

    assert_eq!(generic.domain_packs.len(), 2);
    assert_eq!(
        generic
            .domain_pack("OCOMP")
            .unwrap()
            .tests
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        ocomp.test_ids()
    );
    assert_eq!(
        generic
            .domain_pack("METADOSIS")
            .unwrap()
            .requirements
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["C-1", "D-1", "H-1", "H-2", "H-3", "M-1", "M-2", "M-3", "M-4",]
    );
}

#[test]
fn domain_neutral_cli_reaches_the_checked_in_index() {
    let root = repository_root();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_outbe-verification-ledger"))
        .args(["--repo", root.to_str().unwrap(), "validate-index"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "verification ledger PASS: 2 domain packs\n"
    );
}

#[test]
fn domain_neutral_cli_rejects_self_attested_metadosis_pass() {
    let root = repository_root();
    let temporary = TempDir::new().unwrap();
    let evidence_path = temporary.path().join("metadosis-evidence.json");
    let bundle = temporary.path().join("bundle");
    std::fs::create_dir(&bundle).unwrap();
    let evidence = evidence("METADOSIS", "MET-ENV-001", "PROCESS_Q_FORMING_IMPORT");
    std::fs::write(&evidence_path, serde_json::to_vec(&evidence).unwrap()).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_outbe-verification-ledger"))
        .args(["--repo", root.to_str().unwrap(), "verify", "--evidence"])
        .arg(&evidence_path)
        .arg("--bundle-root")
        .arg(&bundle)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Metadosis evidence has no content-addressed runner-receipt.json"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn checked_in_adr_flow_and_abi_contract_is_synchronized_without_status_promotion() {
    let root = repository_root();
    for (path, required) in [
        (
            "docs/adr/core/ADR-C-MET-001-metadosis-worldwide-day-fsm.md",
            "CapacityForfeiture",
        ),
        (
            "docs/adr/core/ADR-C-LYS-001-lysis-tribute-to-nod-transformation.md",
            "Metadosis is not a synchronous Lysis caller",
        ),
        (
            "docs/adr/system/ADR-S-OCM-004-certified-activation-job-fsm-and-protocol-versioning.md",
            "exactly block `1`",
        ),
        (
            "docs/adr/core/ADR-C-DES-001-desis-cross-chain-auction-state-machine.md",
            "SupplyExceedsAuctionDomain",
        ),
        (
            "docs/adr/core/ADR-C-TRB-001-authenticated-tribute-ledger.md",
            "two FAILED-day authorities",
        ),
        (
            "docs/adr/core/ADR-C-PRM-003-promis-unallocated-limit.md",
            "exactly three new typed contribution sources",
        ),
        (
            "docs/adr/system/ADR-S-CYC-001-deterministic-cycle-scheduling.md",
            "DayLimitFormationReceipt",
        ),
        (
            "docs/adr/blockchain/ADR-B-GEN-001-genesis-chain-identity-and-schema-activation.md",
            "Fresh-devnet Metadosis contract",
        ),
        (
            "docs/adr/blockchain/ADR-B-TST-001-production-verification-and-evidence-architecture.md",
            "Versioned VerificationLedger",
        ),
        (
            "docs/adr/blockchain/ADR-B-OCD-011-partition-retirement.md",
            "Two closed Metadosis terminal retirement authorities",
        ),
    ] {
        let document = std::fs::read_to_string(root.join(path)).unwrap();
        assert!(
            document.contains(required),
            "{path} is missing normative fragment {required:?}"
        );
    }

    let metadosis = std::fs::read_to_string(
        root.join("docs/adr/core/ADR-C-MET-001-metadosis-worldwide-day-fsm.md"),
    )
    .unwrap();
    let lysis = std::fs::read_to_string(
        root.join("docs/adr/core/ADR-C-LYS-001-lysis-tribute-to-nod-transformation.md"),
    )
    .unwrap();
    assert!(metadosis.contains("- **Status:** Proposed;"));
    assert!(lysis.contains("- **Status:** Proposed;"));

    let index = std::fs::read_to_string(root.join("docs/adr/index.md")).unwrap();
    assert!(index.lines().any(|line| {
        line.contains("ADR-S-OCM-004") && line.trim_end().ends_with("| Accepted |")
    }));
    assert!(index.lines().any(|line| {
        line.contains("ADR-B-RLS-001") && line.trim_end().ends_with("| Accepted |")
    }));

    let pfs_002 =
        std::fs::read_to_string(root.join("docs/flows/002-off-chain-poc-protocol-flow.md"))
            .unwrap();
    let pfs_009 =
        std::fs::read_to_string(root.join("docs/flows/009-multichain-auction-day.md")).unwrap();
    assert!(!pfs_002.contains("feat/ocomp-poc"));
    assert!(pfs_002.contains("- **Status:** Draft"));
    assert!(pfs_002.contains("Metadosis profile uses"));
    assert!(pfs_002.contains("`Measurement` install at height `1`"));
    assert!(pfs_002.contains("`Final` fixture at its"));
    assert!(pfs_002.contains("activation height `32`"));
    assert!(pfs_009.contains("OCOMP q-forming apply"));
    assert!(pfs_009.contains("SupplyExceedsAuctionDomain"));
}

#[test]
fn index_rejects_duplicate_keys_namespaces_ids_and_schema_drift() {
    let (repo, index) = fixture();

    mutate(
        &repo.path().join("outbe-plan/a.yaml"),
        "schema_version: 1\n  namespace: A",
        "schema_version: 1\n  schema_version: 1\n  namespace: A",
    );
    assert_error(
        VerificationLedger::parse(&index),
        "duplicate YAML mapping key",
    );

    let (repo, index) = fixture();
    mutate(
        &repo.path().join("outbe-plan/verification-ledger.yaml"),
        "  - namespace: B",
        "  - namespace: A",
    );
    mutate(
        &repo.path().join("outbe-plan/verification-ledger.yaml"),
        "path: outbe-plan/b.yaml",
        "path: outbe-plan/a.yaml",
    );
    assert_error(VerificationLedger::parse(&index), "duplicate namespace A");

    let (repo, index) = fixture();
    mutate(&repo.path().join("outbe-plan/b.yaml"), "T-B", "T-A");
    assert_error(VerificationLedger::parse(&index), "duplicate test ID T-A");

    let (repo, index) = fixture();
    mutate(&repo.path().join("outbe-plan/b.yaml"), "R-B", "R-A");
    assert_error(
        VerificationLedger::parse(&index),
        "duplicate requirement ID R-A",
    );

    let (repo, index) = fixture();
    mutate(
        &repo.path().join("outbe-plan/b.yaml"),
        "schema_version: 1\n  namespace: B",
        "schema_version: 2\n  namespace: B",
    );
    assert_error(
        VerificationLedger::parse(&index),
        "domain-pack schema mismatch for B",
    );
}

#[test]
fn index_rejects_unknown_and_cross_pack_requirement_references() {
    let (repo, index) = fixture();
    mutate(
        &repo.path().join("outbe-plan/a.yaml"),
        "tests: [T-A]",
        "tests: [\"UNKNOWN::T-X\"]",
    );
    assert_error(
        VerificationLedger::parse(&index),
        "unknown namespace UNKNOWN",
    );

    let (repo, index) = fixture();
    mutate(
        &repo.path().join("outbe-plan/a.yaml"),
        "tests: [T-A]",
        "tests: [\"B::T-B\"]",
    );
    assert_error(
        VerificationLedger::parse(&index),
        "cross-pack requirement reference",
    );
}

#[test]
fn required_missing_renamed_ignored_and_skipped_tests_fail_closed() {
    let (repo, index) = fixture();
    let ledger = VerificationLedger::parse(&index).unwrap();
    let pack = ledger.domain_pack("A").unwrap();

    let mut missing = evidence("A", "T-A", "A_INTERFACE");
    missing.discovery.discovered.clear();
    missing.discovery.missing.push("T-A".to_owned());
    missing.tests.clear();
    assert_error(
        validate_domain_evidence_manifest(pack, &missing),
        "required tests are missing",
    );

    let renamed = evidence("A", "T-RENAMED", "A_INTERFACE");
    assert_error(
        validate_domain_evidence_manifest(pack, &renamed),
        "test discovery contains unknown test T-RENAMED",
    );

    let mut unknown_discovered = evidence("A", "T-A", "A_INTERFACE");
    unknown_discovered
        .discovery
        .discovered
        .push("T-UNKNOWN".to_owned());
    assert_error(
        validate_domain_evidence_manifest(pack, &unknown_discovered),
        "test discovery contains unknown test T-UNKNOWN",
    );

    let mut stale_name = evidence("A", "T-A", "A_INTERFACE");
    stale_name.tests[0].name = "renamed_test".to_owned();
    assert_error(
        validate_domain_evidence_manifest(pack, &stale_name),
        "used name renamed_test, expected exact_test_name",
    );

    let mut wrong_target = evidence("A", "T-A", "A_INTERFACE");
    wrong_target.tests[0].target = "other-crate".to_owned();
    assert_error(
        validate_domain_evidence_manifest(pack, &wrong_target),
        "used target other-crate, expected crate",
    );

    let mut wrong_test_lane = evidence("A", "T-A", "A_INTERFACE");
    wrong_test_lane.tests[0].lane = "OTHER-LANE".to_owned();
    assert_error(
        validate_domain_evidence_manifest(pack, &wrong_test_lane),
        "used lane OTHER-LANE, expected LINUX",
    );

    let mut undeclared_substitution = evidence("A", "T-A", "A_INTERFACE");
    undeclared_substitution.tests[0]
        .substitutions
        .push("fixture".to_owned());
    assert_error(
        validate_domain_evidence_manifest(pack, &undeclared_substitution),
        "used substitutions",
    );

    let mut skipped = evidence("A", "T-A", "A_INTERFACE");
    skipped.discovery.skipped.push("T-A".to_owned());
    skipped.tests[0].status = AssertionStatus::Skipped;
    assert_error(
        validate_domain_evidence_manifest(pack, &skipped),
        "required tests are ignored or skipped",
    );

    mutate(
        &repo.path().join("outbe-plan/a.yaml"),
        "ignored: false",
        "ignored: true",
    );
    assert_error(
        VerificationLedger::parse(&index),
        "required test T-A is ignored or skipped",
    );
}

#[test]
fn evidence_rejects_wrong_interface_revision_profile_lane_and_namespace() {
    let (_repo, index) = fixture();
    let ledger = VerificationLedger::parse(&index).unwrap();
    let pack = ledger.domain_pack("A").unwrap();

    let wrong_interface = evidence("A", "T-A", "FIXTURE");
    assert_error(
        validate_domain_evidence_manifest(pack, &wrong_interface),
        "used evidence interface FIXTURE",
    );

    let mut wrong_revision = evidence("A", "T-A", "A_INTERFACE");
    wrong_revision.source.sha = "not-a-revision".to_owned();
    assert_error(
        validate_domain_evidence_manifest(pack, &wrong_revision),
        "source revision is not exact",
    );

    let mut uppercase_revision = evidence("A", "T-A", "A_INTERFACE");
    uppercase_revision.source.sha = "A".repeat(40);
    assert_error(
        validate_domain_evidence_manifest(pack, &uppercase_revision),
        "source revision is not exact",
    );

    let mut untracked = evidence("A", "T-A", "A_INTERFACE");
    untracked.source.untracked_dirty = true;
    assert_error(
        validate_domain_evidence_manifest(pack, &untracked),
        "untracked source tree is dirty",
    );

    let mut wrong_profile = evidence("A", "T-A", "A_INTERFACE");
    wrong_profile.artifact_profile = "OTHER".to_owned();
    assert_error(
        validate_domain_evidence_manifest(pack, &wrong_profile),
        "artifact profile mismatch",
    );

    let mut wrong_lane = evidence("A", "T-A", "A_INTERFACE");
    wrong_lane.lane = "MACOS".to_owned();
    assert_error(
        validate_domain_evidence_manifest(pack, &wrong_lane),
        "Linux lane mismatch",
    );

    let mut wrong_image = evidence("A", "T-A", "A_INTERFACE");
    wrong_image.linux_image_digest = "latest".to_owned();
    assert_error(
        validate_domain_evidence_manifest(pack, &wrong_image),
        "Linux image digest is not an exact sha256 content identity",
    );

    let mut unknown = evidence("UNKNOWN", "T-A", "A_INTERFACE");
    let unknown_bundle = materialize(&mut unknown);
    assert_error(
        verify_evidence_set(
            &ledger,
            &[DomainEvidenceBundle::new(&unknown, unknown_bundle.path())],
        ),
        "unknown verification namespace UNKNOWN",
    );
}

#[test]
fn evidence_rejects_invalid_toolchain_and_member_identity() {
    let (_repo, index) = fixture();
    let ledger = VerificationLedger::parse(&index).unwrap();
    let pack = ledger.domain_pack("A").unwrap();

    let mut invalid_toolchain = evidence("A", "T-A", "A_INTERFACE");
    invalid_toolchain.source.cargo_lock_sha256 = "ABC".to_owned();
    assert_error(
        validate_domain_evidence_manifest(pack, &invalid_toolchain),
        "Cargo.lock digest is not exact SHA-256",
    );

    let mut invalid_member = evidence("A", "T-A", "A_INTERFACE");
    invalid_member.members.push(MemberDigestV1 {
        path: "../escape".to_owned(),
        length: 1,
        sha256: "d".repeat(64),
    });
    assert_error(
        validate_domain_evidence_manifest(pack, &invalid_member),
        "invalid evidence member path",
    );

    let mut duplicate_member = evidence("A", "T-A", "A_INTERFACE");
    duplicate_member.members = vec![
        MemberDigestV1 {
            path: "results/test.json".to_owned(),
            length: 1,
            sha256: "d".repeat(64),
        },
        MemberDigestV1 {
            path: "results/test.json".to_owned(),
            length: 1,
            sha256: "d".repeat(64),
        },
    ];
    assert_error(
        validate_domain_evidence_manifest(pack, &duplicate_member),
        "duplicate evidence member",
    );

    let mut empty = evidence("A", "T-A", "A_INTERFACE");
    empty.members.clear();
    empty.tests[0].evidence_member_refs.clear();
    assert_error(
        validate_domain_evidence_manifest(pack, &empty),
        "domain evidence has no content-addressed members",
    );

    let mut unreferenced = evidence("A", "T-A", "A_INTERFACE");
    unreferenced.members.push(MemberDigestV1 {
        path: "results/unreferenced.json".to_owned(),
        length: 1,
        sha256: "d".repeat(64),
    });
    assert_error(
        validate_domain_evidence_manifest(pack, &unreferenced),
        "domain evidence contains unreferenced members",
    );
}

#[test]
fn bundle_verification_rejects_member_substitution_and_length_drift() {
    let (_repo, index) = fixture();
    let ledger = VerificationLedger::parse(&index).unwrap();
    let pack = ledger.domain_pack("A").unwrap();
    let member_path = "results/test.json";
    let original = b"{\"status\":\"PASS\"}";
    let mut exact = evidence("A", "T-A", "A_INTERFACE");
    let bundle = materialize_bytes(&mut exact, original);
    verify_domain_evidence(pack, &exact, bundle.path()).unwrap();

    std::fs::write(bundle.path().join(member_path), b"{\"status\":\"FAIL\"}").unwrap();
    assert_error(
        verify_domain_evidence(pack, &exact, bundle.path()),
        "member hash/length mismatch",
    );

    std::fs::write(bundle.path().join(member_path), original).unwrap();
    exact.members[0].length += 1;
    assert_error(
        verify_domain_evidence(pack, &exact, bundle.path()),
        "member hash/length mismatch",
    );
}

#[test]
fn evidence_set_rejects_mixed_revision_and_profile_but_domains_close_independently() {
    let (_repo, index) = fixture();
    let ledger = VerificationLedger::parse(&index).unwrap();
    let mut a = evidence("A", "T-A", "A_INTERFACE");
    let mut b = evidence("B", "T-B", "B_INTERFACE");
    let a_bundle = materialize(&mut a);
    let b_bundle = materialize(&mut b);

    verify_domain_evidence(ledger.domain_pack("A").unwrap(), &a, a_bundle.path()).unwrap();
    verify_domain_evidence(ledger.domain_pack("B").unwrap(), &b, b_bundle.path()).unwrap();
    verify_evidence_set(
        &ledger,
        &[
            DomainEvidenceBundle::new(&a, a_bundle.path()),
            DomainEvidenceBundle::new(&b, b_bundle.path()),
        ],
    )
    .unwrap();

    let mut mixed_revision = b.clone();
    mixed_revision.source.sha = "2".repeat(40);
    assert_error(
        verify_evidence_set(
            &ledger,
            &[
                DomainEvidenceBundle::new(&a, a_bundle.path()),
                DomainEvidenceBundle::new(&mixed_revision, b_bundle.path()),
            ],
        ),
        "mixed source revision or toolchain identity",
    );

    let mut mixed_profile = b.clone();
    mixed_profile.artifact_profile = "OTHER".to_owned();
    assert_error(
        verify_evidence_set(
            &ledger,
            &[
                DomainEvidenceBundle::new(&a, a_bundle.path()),
                DomainEvidenceBundle::new(&mixed_profile, b_bundle.path()),
            ],
        ),
        "mixed artifact profile evidence",
    );

    let mut mixed_image = b;
    mixed_image.linux_image_digest = format!("sha256:{}", "f".repeat(64));
    assert_error(
        verify_evidence_set(
            &ledger,
            &[
                DomainEvidenceBundle::new(&a, a_bundle.path()),
                DomainEvidenceBundle::new(&mixed_image, b_bundle.path()),
            ],
        ),
        "mixed Linux image digest evidence",
    );
}

fn fixture() -> (TempDir, PathBuf) {
    let repo = TempDir::new().unwrap();
    let plan = repo.path().join("outbe-plan");
    std::fs::create_dir_all(&plan).unwrap();
    std::fs::write(
        plan.join("verification-ledger.yaml"),
        r#"schema_version: 1
kind: outbe_verification_ledger_index
domain_packs:
  - namespace: A
    schema_version: 1
    path: outbe-plan/a.yaml
  - namespace: B
    schema_version: 1
    path: outbe-plan/b.yaml
"#,
    )
    .unwrap();
    std::fs::write(plan.join("a.yaml"), pack("A", "T-A", "R-A", "A_INTERFACE")).unwrap();
    std::fs::write(plan.join("b.yaml"), pack("B", "T-B", "R-B", "B_INTERFACE")).unwrap();
    let index = plan.join("verification-ledger.yaml");
    (repo, index)
}

fn pack(namespace: &str, test_id: &str, requirement_id: &str, interface: &str) -> String {
    format!(
        r#"schema_version: 1
kind: synthetic_domain_pack
verification:
  schema_version: 1
  namespace: {namespace}
  evidence_identity:
    exact_revision: true
    artifact_profile: PROFILE
    linux_lane: LINUX
    require_clean_tracked_tree: true
    require_clean_untracked_tree: true
  closure_policy:
    require_all_required_tests: true
    reject_ignored_or_skipped: true
    reject_unknown_tests: true
tests:
  {test_id}:
    target: crate
    name: exact_test_name
    production_interface: {interface}
    required: true
    lane: LINUX
    substitutions: []
    ignored: false
    skipped: false
requirements:
  {requirement_id}:
    status: required
    tests: [{test_id}]
"#
    )
}

fn evidence(namespace: &str, test_id: &str, interface: &str) -> DomainEvidenceV1 {
    DomainEvidenceV1 {
        schema_version: 1,
        namespace: namespace.to_owned(),
        source: SourceIdentityV1 {
            sha: "1".repeat(40),
            tracked_dirty: false,
            untracked_dirty: false,
            cargo_lock_sha256: "a".repeat(64),
            rust_toolchain_sha256: "b".repeat(64),
            dependency_metadata_sha256: "c".repeat(64),
        },
        artifact_profile: "PROFILE".to_owned(),
        lane: "LINUX".to_owned(),
        linux_image_digest: format!("sha256:{}", "d".repeat(64)),
        discovery: TestDiscoveryV1 {
            discovered: vec![test_id.to_owned()],
            ..TestDiscoveryV1::default()
        },
        tests: vec![EvidenceTestV1 {
            test_id: test_id.to_owned(),
            target: "crate".to_owned(),
            name: "exact_test_name".to_owned(),
            lane: "LINUX".to_owned(),
            production_interface: interface.to_owned(),
            substitutions: Vec::new(),
            status: AssertionStatus::Pass,
            evidence_member_refs: vec!["results/test.json".to_owned()],
        }],
        members: vec![MemberDigestV1 {
            path: "results/test.json".to_owned(),
            length: 1,
            sha256: "d".repeat(64),
        }],
    }
}

fn materialize(evidence: &mut DomainEvidenceV1) -> TempDir {
    let bytes = format!("{}:{}", evidence.namespace, evidence.tests[0].test_id);
    materialize_bytes(evidence, bytes.as_bytes())
}

fn materialize_bytes(evidence: &mut DomainEvidenceV1, bytes: &[u8]) -> TempDir {
    let bundle = TempDir::new().unwrap();
    let member = &mut evidence.members[0];
    let path = bundle.path().join(&member.path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    member.length = u64::try_from(bytes.len()).unwrap();
    member.sha256 = hex::encode(Sha256::digest(bytes));
    bundle
}

fn mutate(path: &Path, from: &str, to: &str) {
    let original = std::fs::read_to_string(path).unwrap();
    assert!(original.contains(from), "fixture mutation source missing");
    std::fs::write(path, original.replacen(from, to, 1)).unwrap();
}

fn assert_error<T: std::fmt::Debug>(result: eyre::Result<T>, needle: &str) {
    let error = result.expect_err("fixture must fail closed");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains(needle),
        "expected error containing {needle:?}, got {rendered}"
    );
}
