//! OCOMP PoC evidence schema, discovery, publication and independent verifier.
//!
//! The ordinary Cucumber [`crate::evidence`] record stays backward compatible.
//! This module adds the stricter run-level contract required for OCOMP without
//! introducing a second test harness.

mod discovery;
mod io;
mod ledger;
mod schema;
mod verify;

pub use discovery::{discover, validate_discovery, TEST_ID_MARKER};
pub use io::{
    capture_source_identity, hash_file, publish_assertions, publish_manifest, publish_member,
    publish_report, sha256_hex,
};
pub use ledger::{PlanningLedger, LEDGER_KIND};
pub use schema::{
    AssertionRecordV1, AssertionStatus, ClosureReportV1, EvidenceMode, MemberDigestV1,
    RunManifestV1, SourceIdentityV1, TestDiscoveryV1, RUNTIME_SCHEMA_VERSION,
};
pub use verify::{
    manifest_in, missing_bundle_report, require_pass, task_progress_report, verify_manifest,
};
