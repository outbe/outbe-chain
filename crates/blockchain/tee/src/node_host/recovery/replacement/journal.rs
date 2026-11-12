use super::super::super::{
    codec_error, path_exists, read_replacement_candidate, read_replacement_promotion,
    read_replacement_relay, read_replacement_submission, remove_file_if_exists,
    validate_durable_replacement_submission, validate_replacement_candidate_state, NodeHostPaths,
    ReplacementCandidateRecordV1,
};
use super::replacement_authorization;
use crate::{NodeHostNoiseKey, TransportError};
use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;
use std::fs::{self, File};

fn validate_candidate_refresh_pair(
    candidate: &ReplacementCandidateRecordV1,
    candidate_next: &ReplacementCandidateRecordV1,
) -> Result<(), TransportError> {
    if candidate_next.predecessor_manifest_hash != candidate.predecessor_manifest_hash
        || candidate_next
            .manifest
            .node_host_authorization_hash()
            .map_err(codec_error)?
            != candidate
                .manifest
                .node_host_authorization_hash()
                .map_err(codec_error)?
        || candidate_next.manifest.enclave_id().map_err(codec_error)?
            != candidate.manifest.enclave_id().map_err(codec_error)?
    {
        return Err(TransportError::Codec(
            "candidate refresh journal changes replacement identity".into(),
        ));
    }
    Ok(())
}

pub(super) struct ReplacementPresence {
    pub(super) candidate_exists: bool,
    pub(super) candidate_next_exists: bool,
    pub(super) submission_exists: bool,
    pub(super) submission_next_exists: bool,
    pub(super) relay_exists: bool,
    pub(super) relay_next_exists: bool,
    pub(super) promotion_exists: bool,
    pub(super) promotion_next_exists: bool,
    pub(super) next_exists: bool,
}

impl ReplacementPresence {
    pub(super) fn read(paths: &NodeHostPaths) -> Result<Self, TransportError> {
        let candidate_exists = path_exists(&paths.replacement_candidate)?;
        let candidate_next_exists = path_exists(&paths.replacement_candidate_next)?;
        let submission_exists = path_exists(&paths.replacement_submission)?;
        let submission_next_exists = path_exists(&paths.replacement_submission_next)?;
        let relay_exists = path_exists(&paths.replacement_relay)?;
        let relay_next_exists = path_exists(&paths.replacement_relay_next)?;
        let promotion_exists = path_exists(&paths.replacement_promotion)?;
        let promotion_next_exists = path_exists(&paths.replacement_promotion_next)?;
        let next_exists = path_exists(&paths.next_manifest)?;
        Ok(Self {
            candidate_exists,
            candidate_next_exists,
            submission_exists,
            submission_next_exists,
            relay_exists,
            relay_next_exists,
            promotion_exists,
            promotion_next_exists,
            next_exists,
        })
    }

    fn has_submission_or_relay(&self) -> bool {
        let submission = self.submission_exists || self.submission_next_exists;
        let relay = self.relay_exists || self.relay_next_exists;
        submission || relay
    }
}

pub(super) fn reconcile_candidate_journal(
    paths: &NodeHostPaths,
    active: &EnclaveInitializationManifestV1,
    node_host: &NodeHostNoiseKey,
    presence: &mut ReplacementPresence,
) -> Result<(), TransportError> {
    if presence.candidate_next_exists {
        let candidate_next = read_replacement_candidate(&paths.replacement_candidate_next)?;
        if presence.candidate_exists {
            if presence.has_submission_or_relay() {
                return Err(TransportError::Codec(
                    "candidate refresh journal exists after replacement submission".into(),
                ));
            }
            let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
            validate_candidate_refresh_pair(&candidate, &candidate_next)?;
            remove_file_if_exists(&paths.replacement_candidate_next)?;
        } else {
            if presence.has_submission_or_relay() || presence.next_exists {
                return Err(TransportError::Codec(
                    "initial candidate journal conflicts with later replacement state".into(),
                ));
            }
            validate_replacement_candidate_state(&candidate_next, active, node_host)?;
            fs::rename(
                &paths.replacement_candidate_next,
                &paths.replacement_candidate,
            )?;
            presence.candidate_exists = true;
        }
        File::open(&paths.root)?.sync_all()?;
    }
    Ok(())
}

pub(super) fn reconcile_submission_journal(
    paths: &NodeHostPaths,
    presence: &mut ReplacementPresence,
) -> Result<(), TransportError> {
    if presence.submission_next_exists {
        if !presence.candidate_exists {
            return Err(TransportError::Codec(
                "replacement submission journal is missing its candidate".into(),
            ));
        }
        let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
        let submission_next = read_replacement_submission(&paths.replacement_submission_next)?;
        validate_durable_replacement_submission(&candidate.manifest, &submission_next)?;
        if presence.submission_exists {
            if read_replacement_submission(&paths.replacement_submission)? != submission_next {
                return Err(TransportError::Codec(
                    "replacement submission journal conflicts with durable state".into(),
                ));
            }
            remove_file_if_exists(&paths.replacement_submission_next)?;
        } else {
            fs::rename(
                &paths.replacement_submission_next,
                &paths.replacement_submission,
            )?;
            presence.submission_exists = true;
        }
        File::open(&paths.root)?.sync_all()?;
    }
    Ok(())
}

pub(super) fn reconcile_relay_journal(
    paths: &NodeHostPaths,
    presence: &mut ReplacementPresence,
) -> Result<(), TransportError> {
    if presence.relay_next_exists {
        if !presence.candidate_exists || !presence.submission_exists {
            return Err(TransportError::Codec(
                "replacement relay journal is missing candidate submission state".into(),
            ));
        }
        let submission = read_replacement_submission(&paths.replacement_submission)?;
        let relay_next = read_replacement_relay(&paths.replacement_relay_next)?;
        if relay_next.submission_hash() != submission.submission_hash()? {
            return Err(TransportError::Codec(
                "replacement relay journal targets another submission".into(),
            ));
        }
        if presence.relay_exists {
            if read_replacement_relay(&paths.replacement_relay)? != relay_next {
                return Err(TransportError::Codec(
                    "replacement relay journal conflicts with durable state".into(),
                ));
            }
            remove_file_if_exists(&paths.replacement_relay_next)?;
        } else {
            fs::rename(&paths.replacement_relay_next, &paths.replacement_relay)?;
            presence.relay_exists = true;
        }
        File::open(&paths.root)?.sync_all()?;
    }
    Ok(())
}

pub(super) fn reconcile_promotion_journal(
    paths: &NodeHostPaths,
    presence: &mut ReplacementPresence,
) -> Result<(), TransportError> {
    if presence.promotion_next_exists {
        if !presence.candidate_exists || !presence.submission_exists {
            return Err(TransportError::Codec(
                "replacement promotion journal is missing candidate submission state".into(),
            ));
        }
        let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
        let submission = read_replacement_submission(&paths.replacement_submission)?;
        let expected = replacement_authorization(&candidate, &submission)?;
        if read_replacement_promotion(&paths.replacement_promotion_next)? != expected {
            return Err(TransportError::Codec(
                "replacement promotion journal targets another authorization".into(),
            ));
        }
        fs::rename(
            &paths.replacement_promotion_next,
            &paths.replacement_promotion,
        )?;
        presence.promotion_exists = true;
        File::open(&paths.root)?.sync_all()?;
    }
    Ok(())
}
