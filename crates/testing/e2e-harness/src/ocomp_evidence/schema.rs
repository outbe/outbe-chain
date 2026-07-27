//! Versioned data carried by an OCOMP PoC evidence bundle.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Runtime evidence schema implemented by the PoC.
pub const RUNTIME_SCHEMA_VERSION: u32 = 1;

/// The only statuses accepted in assertion records.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AssertionStatus {
    /// The named assertion proved its claim.
    Pass,
    /// The named assertion observed a product failure.
    Fail,
    /// Infrastructure prevented the assertion from running to completion.
    InfraError,
    /// The assertion exceeded its declared deadline.
    Timeout,
    /// Discovery found the assertion but did not execute it.
    Skipped,
    /// The required assertion was not discovered.
    Missing,
    /// A normative requirement is intentionally outside the current scope.
    Deferred,
}

/// The claim a manifest is allowed to make.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidenceMode {
    /// Incremental evidence for exactly one task card.
    TaskProgress {
        /// `OCM-NN` task whose local merge gate is being checked.
        task_id: String,
    },
    /// Evidence for exactly one registered execution lane.
    Lane {
        /// Ledger lane such as `OCM-PUBLIC`.
        lane: String,
    },
    /// Complete PoC closure over every mandatory lane and requirement.
    PocClosure,
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

/// Content-addressed member published before `run-manifest.json`.
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

/// Exact OCOMP test inventory observed by a run.
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
    /// Stable ledger test identity.
    pub test_id: String,
    /// Closed assertion status.
    pub status: AssertionStatus,
    /// Oracle selected from the ledger's allowlist.
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
    /// Execution attempt; PoC evidence accepts only the first attempt.
    pub attempt: u32,
}

/// Atomically published, hash-indexed run manifest.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunManifestV1 {
    /// Must equal [`RUNTIME_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Unique, filesystem-safe run identity.
    pub run_id: String,
    /// Scope of the claim.
    pub mode: EvidenceMode,
    /// Inclusive run start in Unix milliseconds.
    pub started_at: u64,
    /// Inclusive run finish in Unix milliseconds.
    pub finished_at: u64,
    /// Exact source/toolchain identity.
    pub source: SourceIdentityV1,
    /// Independently recomputable source discovery.
    pub discovery: TestDiscoveryV1,
    /// Relative path of the JSONL assertion member.
    pub assertions_path: String,
    /// Every bundle member except this manifest and verifier reports.
    pub members: Vec<MemberDigestV1>,
    /// Named evidence sections required by the planning ledger.
    pub sections: BTreeMap<String, Value>,
}

/// Deterministic verifier output.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClosureReportV1 {
    /// Runtime evidence schema that was checked.
    pub schema_version: u32,
    /// `task_progress` or `poc_closure`.
    pub mode: String,
    /// Task identity for task-progress mode.
    pub task_id: Option<String>,
    /// Source SHA when a manifest was available.
    pub source_sha: Option<String>,
    /// Overall fail-closed result.
    pub status: String,
    /// Stable tests whose records all passed.
    pub passed_test_ids: Vec<String>,
    /// Ledger tests absent from discovery or assertions.
    pub missing_test_ids: Vec<String>,
    /// Tests with a non-PASS record.
    pub non_pass_test_ids: Vec<String>,
    /// Normative requirements not discharged by passing tests.
    pub requirement_gaps: Vec<String>,
    /// The two explicitly deferred PFS rows.
    pub deferred_requirement_ids: Vec<String>,
    /// Structural or policy errors.
    pub errors: Vec<String>,
}

impl ClosureReportV1 {
    /// Returns whether this report proves its declared mode.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.status == "PASS"
    }
}
