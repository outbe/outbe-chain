//! Re-export of the canonical V2 committee snapshot types.
//!
//! The definitions live in `outbe-primitives` so crates that do not depend on
//! the consensus stack can share the same wire types.

pub use outbe_primitives::committee_snapshot::{
    build_committee_snapshot, committee_set_hash_v2, committee_snapshot_key, CommitteeEntry,
    CommitteeSnapshot, SnapshotBuildError, OUTBE_COMMITTEE_SET_HASH_V2_DOMAIN,
    OUTBE_COMMITTEE_SNAPSHOT_KEY_V2_DOMAIN, VRF_MATERIAL_VERSION_GENESIS,
};
