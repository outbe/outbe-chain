//! Supervisor-side adoption of one verified Lysis shuffle object closure.

use outbe_ocomp_protocol::{
    shuffle::{verified_shuffle_run_records, ShuffleRunArtifactV1},
    ObjectKind, ProtocolError, SchemaLimits,
};
use thiserror::Error;

use crate::{cas::FilesystemCas, inbox::WorkerInbox};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdoptedLysisShuffleClosureV1 {
    pub descendant_object_count: u32,
    pub verified_record_count: u32,
}

#[derive(Debug, Error)]
pub enum LysisShuffleAdoptionError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

/// Verifies the complete bounded page tree from the worker inbox while
/// publishing each digest-valid descendant into the authoritative CAS.
///
/// The root remains embedded in its `UnitArtifactV1`; only referenced
/// descendants are required in CAS. Content-addressed objects published before
/// a later validation failure are unreachable orphans, never an admitted unit.
pub fn adopt_lysis_shuffle_descendants(
    root: ShuffleRunArtifactV1,
    inbox: &WorkerInbox,
    cas: &FilesystemCas,
    limits: &SchemaLimits,
) -> Result<AdoptedLysisShuffleClosureV1, LysisShuffleAdoptionError> {
    let mut descendant_object_count = 0_u32;
    let records = verified_shuffle_run_records(root, limits, |reference| {
        let object = inbox.read_shuffle_object(reference, limits).map_err(|_| {
            ProtocolError::InvalidInvariant("worker shuffle descendant unavailable")
        })?;
        let published = cas.publish_bytes(object.bytes()).map_err(|_| {
            ProtocolError::InvalidInvariant("authoritative shuffle CAS publication")
        })?;
        if published.transport_digest != reference.transport_digest
            || published.encoded_bytes != reference.encoded_bytes
            || reference.expected_ocb1_kind != Some(ObjectKind::ShuffleRunArtifactV1.tag())
        {
            return Err(ProtocolError::InvalidInvariant(
                "published shuffle descendant descriptor",
            ));
        }
        descendant_object_count =
            descendant_object_count
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "adopted shuffle descendant count",
                })?;
        Ok(object.bytes().to_vec())
    })?;
    let mut verified_record_count = 0_u32;
    for record in records {
        let _ = record?;
        verified_record_count =
            verified_record_count
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "adopted shuffle record count",
                })?;
    }
    Ok(AdoptedLysisShuffleClosureV1 {
        descendant_object_count,
        verified_record_count,
    })
}
