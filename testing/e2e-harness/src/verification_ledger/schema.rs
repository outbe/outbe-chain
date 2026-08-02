//! Domain-neutral schemas used by the verification-ledger parser and verifier.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value;

/// Current index and domain-pack schema version.
pub const VERIFICATION_LEDGER_SCHEMA_VERSION: u32 = 1;
/// Alias used where the domain-pack version is called out explicitly.
pub const DOMAIN_PACK_SCHEMA_VERSION: u32 = VERIFICATION_LEDGER_SCHEMA_VERSION;
/// Exact kind accepted for the repository-level index.
pub const VERIFICATION_LEDGER_INDEX_KIND: &str = "outbe_verification_ledger_index";

/// The only statuses accepted in assertion and test evidence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AssertionStatus {
    /// The named assertion proved its claim.
    Pass,
    /// The named assertion observed a product failure.
    Fail,
    /// Infrastructure prevented completion.
    InfraError,
    /// The assertion exceeded its deadline.
    Timeout,
    /// Discovery found the assertion but did not execute it.
    Skipped,
    /// The required assertion was not discovered.
    Missing,
    /// A domain-owned requirement is intentionally outside the current scope.
    Deferred,
}

/// Exact source and toolchain identity shared by every member of one run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentityV1 {
    /// Git revision used to build all binaries.
    pub sha: String,
    /// Whether tracked or staged files differed from `sha`.
    pub tracked_dirty: bool,
    /// Whether non-ignored untracked files existed.
    pub untracked_dirty: bool,
    /// SHA-256 of the exact `Cargo.lock`.
    pub cargo_lock_sha256: String,
    /// SHA-256 of the checked-in Rust toolchain descriptor.
    pub rust_toolchain_sha256: String,
    /// SHA-256 of normalized `cargo metadata --locked --no-deps` output.
    pub dependency_metadata_sha256: String,
}

/// Content-addressed member published before the enclosing manifest.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemberDigestV1 {
    /// Slash-separated path relative to the bundle directory.
    pub path: String,
    /// Exact byte length.
    pub length: u64,
    /// Lowercase SHA-256 hex of the exact bytes.
    pub sha256: String,
}

/// Exact test inventory observed by a run.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestDiscoveryV1 {
    /// Stable IDs found at their ledger-owned source paths.
    pub discovered: Vec<String>,
    /// Stable IDs declared by the ledger but not found.
    pub missing: Vec<String>,
    /// Discovered IDs carrying an ignore/skip marker.
    pub skipped: Vec<String>,
    /// Discovered IDs carrying a todo marker.
    pub todo: Vec<String>,
    /// Discovered IDs carrying a quarantine marker.
    pub quarantined: Vec<String>,
    /// Discovered IDs carrying an automatic-retry marker.
    pub retried: Vec<String>,
}

impl TestDiscoveryV1 {
    pub(crate) fn normalize(&mut self) {
        for values in [
            &mut self.discovered,
            &mut self.missing,
            &mut self.skipped,
            &mut self.todo,
            &mut self.quarantined,
            &mut self.retried,
        ] {
            values.sort();
            values.dedup();
        }
    }
}

/// One independently verifiable claim emitted by a test.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssertionRecordV1 {
    /// Run-unique assertion identity.
    pub assertion_id: String,
    /// Stable domain-pack test identity.
    pub test_id: String,
    /// Closed assertion status.
    pub status: AssertionStatus,
    /// Oracle selected from the domain pack allowlist.
    pub oracle: String,
    /// Evidence members describing the expected value.
    pub expected_artifact_refs: Vec<String>,
    /// Evidence members describing the observed value.
    pub actual_artifact_refs: Vec<String>,
    /// Logical observation time in Unix milliseconds.
    pub observed_at: u64,
    /// Run identity copied into the record to reject mixed bundles.
    pub run_id: String,
    /// Source revision copied into the record to reject mixed revisions.
    pub source_sha: String,
    /// Execution attempt.
    pub attempt: u32,
}

/// Repository-level index. It contains paths and version pins only.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationLedgerIndex {
    /// Index schema version.
    pub schema_version: u32,
    /// Stable index discriminator.
    pub kind: String,
    /// The only domain packs admitted by this index.
    pub domain_packs: Vec<DomainPackIndexEntry>,
}

/// One exact domain-pack binding.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainPackIndexEntry {
    /// Domain namespace expected inside the pack.
    pub namespace: String,
    /// Domain-pack schema expected at the path.
    pub schema_version: u32,
    /// Repository-relative YAML path.
    pub path: String,
}

/// Generic envelope decoded from a domain-owned planning ledger.
///
/// Unknown top-level fields deliberately remain available to the domain policy
/// adapter through [`raw`]. The generic core interprets only the shared
/// verification header plus the test and requirement catalogs.
#[derive(Clone, Debug)]
pub struct DomainPack {
    /// Original YAML document for the domain policy adapter.
    pub raw: Value,
    /// Shared verification header.
    pub verification: DomainPackVerificationV1,
    /// Generic view of the stable test catalog.
    pub tests: BTreeMap<String, TestDefinitionV1>,
    /// Generic view of requirement-to-test references.
    pub requirements: BTreeMap<String, RequirementDefinitionV1>,
}

/// Shared header embedded in every domain pack.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainPackVerificationV1 {
    /// Shared domain-pack schema version.
    pub schema_version: u32,
    /// Stable namespace, independent of the domain's own ledger kind.
    pub namespace: String,
    /// Exact evidence identity and Linux artifact profile contract.
    pub evidence_identity: EvidenceIdentityPolicyV1,
    /// Declarative generic closure rules.
    pub closure_policy: ClosurePolicyV1,
}

/// Evidence identity required by one domain pack.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceIdentityPolicyV1 {
    /// Evidence must bind every record to one exact manifest source revision.
    pub exact_revision: bool,
    /// Required build/genesis/machine artifact profile.
    pub artifact_profile: String,
    /// Required Linux execution lane.
    pub linux_lane: String,
    /// Whether tracked or staged source drift is forbidden.
    pub require_clean_tracked_tree: bool,
    /// Whether non-ignored untracked source drift is forbidden.
    pub require_clean_untracked_tree: bool,
}

/// Rules that the generic verifier can enforce without knowing the domain.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosurePolicyV1 {
    /// Every test marked required must be discovered and pass.
    pub require_all_required_tests: bool,
    /// Ignored or skipped required tests fail closure.
    pub reject_ignored_or_skipped: bool,
    /// Evidence for undeclared tests fails closure.
    pub reject_unknown_tests: bool,
}

/// Generic test metadata. Domain-specific fields remain in the raw document.
#[derive(Clone, Debug, Deserialize)]
pub struct TestDefinitionV1 {
    /// Exact cargo target, fixture, or repository path.
    #[serde(alias = "planned_path")]
    pub target: String,
    /// Exact test name. Legacy packs may use the stable test ID.
    #[serde(default)]
    pub name: Option<String>,
    /// Production seam exercised by the test.
    #[serde(alias = "production_entrypoint")]
    pub production_interface: String,
    /// Required tests close the domain; optional tests are still validated.
    #[serde(default = "default_required")]
    pub required: bool,
    /// Required execution lane.
    pub lane: String,
    /// Explicit non-production substitutions.
    #[serde(default)]
    pub substitutions: Vec<String>,
    /// Whether the test is statically ignored.
    #[serde(default)]
    pub ignored: bool,
    /// Whether the test is statically skipped.
    #[serde(default)]
    pub skipped: bool,
}

const fn default_required() -> bool {
    true
}

/// Generic requirement record extracted from any domain-owned taxonomy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequirementDefinitionV1 {
    /// Test references. `NAMESPACE::ID` is accepted only for the local namespace.
    pub tests: Vec<String>,
    /// Whether generic closure requires at least one proving test.
    pub required: bool,
}

/// Evidence for one stable test.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceTestV1 {
    /// Stable test ID.
    pub test_id: String,
    /// Exact package, fixture, or source target that executed.
    pub target: String,
    /// Exact test name observed by the lane runner.
    pub name: String,
    /// Exact domain-owned lane that executed this test.
    pub lane: String,
    /// Exact interface class observed by the evidence collector.
    pub production_interface: String,
    /// Explicit substitutions actually used by this test.
    pub substitutions: Vec<String>,
    /// Closed outcome.
    pub status: AssertionStatus,
    /// Content-addressed members proving this test's observed result.
    pub evidence_member_refs: Vec<String>,
}

/// Generic evidence manifest supplied to one domain closure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DomainEvidenceV1 {
    /// Runtime evidence schema.
    pub schema_version: u32,
    /// Domain namespace.
    pub namespace: String,
    /// Exact source/toolchain identity.
    pub source: SourceIdentityV1,
    /// Exact artifact profile used by this run.
    pub artifact_profile: String,
    /// Exact Linux lane actually executed.
    pub lane: String,
    /// Immutable Linux runner/container image content identity.
    pub linux_image_digest: String,
    /// Independent discovery result.
    pub discovery: TestDiscoveryV1,
    /// One result per executed stable test.
    pub tests: Vec<EvidenceTestV1>,
    /// Content-addressed evidence members.
    pub members: Vec<MemberDigestV1>,
}
