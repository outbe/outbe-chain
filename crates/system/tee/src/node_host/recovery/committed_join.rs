use super::super::{
    path_exists, read_committed_join_relay, read_committed_join_submission,
    read_finalized_join_admission_anchor, remove_file_if_exists, validate_anchor_replacement,
    validate_durable_committed_join_submission, NodeHostPaths,
};
use super::remove_torn_scratch;
use crate::TransportError;
use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;
use std::fs::{self, File};

pub(in super::super) fn reconcile_finalized_join_admission_anchor(
    paths: &NodeHostPaths,
) -> Result<(), TransportError> {
    remove_torn_scratch(paths, &paths.finalized_join_admission_anchor_scratch)?;
    if !path_exists(&paths.finalized_join_admission_anchor_next)? {
        return Ok(());
    }
    let next = read_finalized_join_admission_anchor(&paths.finalized_join_admission_anchor_next)?;
    if path_exists(&paths.finalized_join_admission_anchor)? {
        let durable = read_finalized_join_admission_anchor(&paths.finalized_join_admission_anchor)?;
        validate_anchor_replacement(durable, next)?;
        if durable == next {
            remove_file_if_exists(&paths.finalized_join_admission_anchor_next)?;
        } else {
            fs::rename(
                &paths.finalized_join_admission_anchor_next,
                &paths.finalized_join_admission_anchor,
            )?;
        }
    } else {
        fs::rename(
            &paths.finalized_join_admission_anchor_next,
            &paths.finalized_join_admission_anchor,
        )?;
    }
    File::open(&paths.root)?.sync_all()?;
    Ok(())
}

pub(in super::super) fn reconcile_committed_join_state(
    paths: &NodeHostPaths,
    manifest: &EnclaveInitializationManifestV1,
) -> Result<(), TransportError> {
    remove_torn_scratch(paths, &paths.committed_join_write_scratch)?;
    let submission_exists = path_exists(&paths.committed_join_submission)?;
    let submission_exists =
        reconcile_committed_join_submission(paths, manifest, submission_exists)?;

    let relay_exists = path_exists(&paths.committed_join_relay)?;
    reconcile_committed_join_relay(paths, manifest, submission_exists, relay_exists)?;

    if path_exists(&paths.committed_join_relay)? && !submission_exists {
        return Err(TransportError::Codec(
            "committed join relay is missing its submission".into(),
        ));
    }
    Ok(())
}

fn reconcile_committed_join_submission(
    paths: &NodeHostPaths,
    manifest: &EnclaveInitializationManifestV1,
    mut submission_exists: bool,
) -> Result<bool, TransportError> {
    if path_exists(&paths.committed_join_submission_next)? {
        let next = read_committed_join_submission(&paths.committed_join_submission_next)?;
        validate_durable_committed_join_submission(manifest, &next)?;
        if submission_exists {
            if read_committed_join_submission(&paths.committed_join_submission)? != next {
                return Err(TransportError::Codec(
                    "committed join submission journal conflicts with durable state".into(),
                ));
            }
            remove_file_if_exists(&paths.committed_join_submission_next)?;
        } else {
            fs::rename(
                &paths.committed_join_submission_next,
                &paths.committed_join_submission,
            )?;
            submission_exists = true;
        }
        File::open(&paths.root)?.sync_all()?;
    }

    Ok(submission_exists)
}

fn reconcile_committed_join_relay(
    paths: &NodeHostPaths,
    manifest: &EnclaveInitializationManifestV1,
    submission_exists: bool,
    relay_exists: bool,
) -> Result<(), TransportError> {
    if path_exists(&paths.committed_join_relay_next)? {
        if !submission_exists {
            return Err(TransportError::Codec(
                "committed join relay journal is missing submission state".into(),
            ));
        }
        let submission = read_committed_join_submission(&paths.committed_join_submission)?;
        validate_durable_committed_join_submission(manifest, &submission)?;
        let next = read_committed_join_relay(&paths.committed_join_relay_next)?;
        if next.submission_hash() != submission.submission_hash()? {
            return Err(TransportError::Codec(
                "committed join relay journal targets another submission".into(),
            ));
        }
        if relay_exists {
            if read_committed_join_relay(&paths.committed_join_relay)? != next {
                return Err(TransportError::Codec(
                    "committed join relay journal conflicts with durable state".into(),
                ));
            }
            remove_file_if_exists(&paths.committed_join_relay_next)?;
        } else {
            fs::rename(
                &paths.committed_join_relay_next,
                &paths.committed_join_relay,
            )?;
        }
        File::open(&paths.root)?.sync_all()?;
    }

    Ok(())
}
