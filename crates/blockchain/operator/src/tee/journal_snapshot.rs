//! Shared V1 journal envelope with a lifecycle owned by each operation.

use serde::{Deserialize, Serialize};

/// Durable journal metadata and the operation's distinct lifecycle state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JournalSnapshotV1<S> {
    pub version: u8,
    pub generation: u64,
    pub lifecycle: S,
}

impl<S> JournalSnapshotV1<S> {
    pub fn new(lifecycle: S) -> Self {
        Self {
            version: 1,
            generation: 1,
            lifecycle,
        }
    }
}
