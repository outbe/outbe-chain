//! CE-owned finalized sparse-tree persistence.
//!
//! This module owns deterministic local codecs, the separate MDBX environment,
//! atomic contiguous finalized application, and restart/ACK classification. It
//! is authenticated materialization only: the exact EVM root remains the sole
//! consensus authority.

mod audit;
mod checkpoint;
mod codec;
mod collections;
mod environment;
mod error;
mod identity;
mod readonly;
mod snapshot;
mod tables;
pub use audit::{CeAuditError, CeAuditLimits, CeAuditReport, CeAuditVisitor, CeAuditWork};
#[cfg(feature = "test-utils")]
mod test_support;

use checkpoint::read_marker;
pub use checkpoint::{
    classify_restart, ApplyOutcome, CeRetentionCursor, DurableFinalizedCheckpoint,
    ExactParentIdentity, FinalizationStage, FinalizedMarker, RestartClassification,
};
pub(crate) use codec::validate_root;
use codec::{decode_b256, Decoder};
pub use codec::{BranchKey, BranchNode, FieldValue, LeafValue, MergeValue, TreeKey, TreeNamespace};
use collections::{
    collection_has_records, count_collection_leaf_records, count_collection_root_records,
    delete_collection_records, prefixed_key, read_collection_roots, read_required_tree_root,
    read_tree_leaf, read_tree_root,
};
pub use environment::CeMdbx;
pub use error::PersistenceError;
use identity::validate_expected_environment_identity;
pub use identity::EnvironmentIdentity;
pub use readonly::CeMdbxReadOnly;
use snapshot::MdbxSnapshot;

pub const LOCAL_STORAGE_SCHEMA_VERSION: u32 = 3;
pub const FINALIZED_MARKER_ENCODED_LEN: usize = 4 + 8 + 32 * 4;
pub const CE_SMT_RELATIVE_PATH: &str = "compressed_entities/smt";
const IDENTITY_KEY: &[u8] = b"environment_identity";
const LAST_APPLIED_KEY: &[u8] = b"last_applied";

#[cfg(test)]
mod adr010_tests;

// ADR-009's flat-namespace fixtures are replaced by ADR-010 catalog fixtures below.
#[cfg(test)]
mod tests;
