//! OCOMP-specific manifests layered on the generic verification-ledger schema.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::verification_ledger::{
    AssertionRecordV1, AssertionStatus, MemberDigestV1, SourceIdentityV1, TestDiscoveryV1,
};

/// Runtime evidence schema implemented by the PoC.
pub const RUNTIME_SCHEMA_VERSION: u32 = 1;

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
