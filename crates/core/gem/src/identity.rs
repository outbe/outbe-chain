//! Pure gem identities shared by issuance and off-chain calculation.
use alloy_primitives::B256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::wwd_entity_id::WwdEntityId;

/// Canonical group identity `WWD_BE4 || group_key`, dated by the group's
/// `full_day_after_issue`.
#[must_use]
pub fn group_id(full_day_after_issue: WorldwideDay, group_key: B256) -> WwdEntityId {
    WwdEntityId::from_day_and_digest(full_day_after_issue, group_key)
}
