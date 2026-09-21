use super::codec_error;
use super::path_exists;
use super::read_committed_join_relay;
use super::read_committed_join_submission;
use super::read_finalized_join_admission_anchor;
use super::read_manifest;
use super::read_owned_bounded_file;
use super::read_replacement_candidate;
use super::read_replacement_promotion;
use super::read_replacement_relay;
use super::read_replacement_submission;
use super::remove_file_if_exists;
use super::validate_anchor_replacement;
use super::validate_durable_committed_join_submission;
use super::validate_durable_replacement_submission;
use super::validate_replacement_candidate_state;
use super::FinalizedReplacementAuthorizationV1;
use super::NodeHostPaths;
use super::ReplacementCandidateRecordV1;
use super::ReplacementCandidateSubmissionV1;
use super::MAX_INITIALIZATION_MANIFEST_BYTES;

use crate::NodeHostNoiseKey;
use crate::TransportError;

use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;

use std::fs;

use std::fs::File;

pub(super) fn replacement_authorization(
    candidate: &ReplacementCandidateRecordV1,
    submission: &ReplacementCandidateSubmissionV1,
) -> Result<FinalizedReplacementAuthorizationV1, TransportError> {
    let intent = validate_durable_replacement_submission(&candidate.manifest, submission)?;
    Ok(FinalizedReplacementAuthorizationV1 {
        intent_hash: intent.intent_hash().map_err(codec_error)?,
        candidate_manifest_hash: candidate
            .manifest
            .authorization_hash()
            .map_err(codec_error)?,
    })
}

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

pub(super) fn reconcile_finalized_join_admission_anchor(
    paths: &NodeHostPaths,
) -> Result<(), TransportError> {
    if path_exists(&paths.finalized_join_admission_anchor_scratch)? {
        remove_file_if_exists(&paths.finalized_join_admission_anchor_scratch)?;
        File::open(&paths.root)?.sync_all()?;
    }
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

pub(super) fn reconcile_committed_join_state(
    paths: &NodeHostPaths,
    manifest: &EnclaveInitializationManifestV1,
) -> Result<(), TransportError> {
    if path_exists(&paths.committed_join_write_scratch)? {
        remove_file_if_exists(&paths.committed_join_write_scratch)?;
        File::open(&paths.root)?.sync_all()?;
    }
    let mut submission_exists = path_exists(&paths.committed_join_submission)?;
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

    let relay_exists = path_exists(&paths.committed_join_relay)?;
    if path_exists(&paths.committed_join_relay_next)? {
        if !submission_exists {
            return Err(TransportError::Codec(
                "committed join relay journal is missing submission state".into(),
            ));
        }
        let submission = read_committed_join_submission(&paths.committed_join_submission)?;
        validate_durable_committed_join_submission(manifest, &submission)?;
        let next = read_committed_join_relay(&paths.committed_join_relay_next)?;
        if next.submission_hash != submission.submission_hash()? {
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

    if path_exists(&paths.committed_join_relay)? && !submission_exists {
        return Err(TransportError::Codec(
            "committed join relay is missing its submission".into(),
        ));
    }
    Ok(())
}

pub(super) fn reconcile_replacement_state(
    paths: &NodeHostPaths,
    node_host: &NodeHostNoiseKey,
) -> Result<(), TransportError> {
    if path_exists(&paths.replacement_write_scratch)? {
        remove_file_if_exists(&paths.replacement_write_scratch)?;
        File::open(&paths.root)?.sync_all()?;
    }
    let active = read_manifest(&paths.manifest)?;
    let mut candidate_exists = path_exists(&paths.replacement_candidate)?;
    let candidate_next_exists = path_exists(&paths.replacement_candidate_next)?;
    let mut submission_exists = path_exists(&paths.replacement_submission)?;
    let submission_next_exists = path_exists(&paths.replacement_submission_next)?;
    let mut relay_exists = path_exists(&paths.replacement_relay)?;
    let relay_next_exists = path_exists(&paths.replacement_relay_next)?;
    let mut promotion_exists = path_exists(&paths.replacement_promotion)?;
    let promotion_next_exists = path_exists(&paths.replacement_promotion_next)?;
    let next_exists = path_exists(&paths.next_manifest)?;

    if candidate_next_exists {
        let candidate_next = read_replacement_candidate(&paths.replacement_candidate_next)?;
        if candidate_exists {
            if submission_exists || submission_next_exists || relay_exists || relay_next_exists {
                return Err(TransportError::Codec(
                    "candidate refresh journal exists after replacement submission".into(),
                ));
            }
            let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
            validate_candidate_refresh_pair(&candidate, &candidate_next)?;
            remove_file_if_exists(&paths.replacement_candidate_next)?;
        } else {
            if submission_exists
                || submission_next_exists
                || relay_exists
                || relay_next_exists
                || next_exists
            {
                return Err(TransportError::Codec(
                    "initial candidate journal conflicts with later replacement state".into(),
                ));
            }
            validate_replacement_candidate_state(&candidate_next, &active, node_host)?;
            fs::rename(
                &paths.replacement_candidate_next,
                &paths.replacement_candidate,
            )?;
            candidate_exists = true;
        }
        File::open(&paths.root)?.sync_all()?;
    }

    if submission_next_exists {
        if !candidate_exists {
            return Err(TransportError::Codec(
                "replacement submission journal is missing its candidate".into(),
            ));
        }
        let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
        let submission_next = read_replacement_submission(&paths.replacement_submission_next)?;
        validate_durable_replacement_submission(&candidate.manifest, &submission_next)?;
        if submission_exists {
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
            submission_exists = true;
        }
        File::open(&paths.root)?.sync_all()?;
    }

    if relay_next_exists {
        if !candidate_exists || !submission_exists {
            return Err(TransportError::Codec(
                "replacement relay journal is missing candidate submission state".into(),
            ));
        }
        let submission = read_replacement_submission(&paths.replacement_submission)?;
        let relay_next = read_replacement_relay(&paths.replacement_relay_next)?;
        if relay_next.submission_hash != submission.submission_hash()? {
            return Err(TransportError::Codec(
                "replacement relay journal targets another submission".into(),
            ));
        }
        if relay_exists {
            if read_replacement_relay(&paths.replacement_relay)? != relay_next {
                return Err(TransportError::Codec(
                    "replacement relay journal conflicts with durable state".into(),
                ));
            }
            remove_file_if_exists(&paths.replacement_relay_next)?;
        } else {
            fs::rename(&paths.replacement_relay_next, &paths.replacement_relay)?;
            relay_exists = true;
        }
        File::open(&paths.root)?.sync_all()?;
    }

    if promotion_next_exists {
        if !candidate_exists || !submission_exists {
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
        promotion_exists = true;
        File::open(&paths.root)?.sync_all()?;
    }

    let active_hash = active.authorization_hash().map_err(codec_error)?;
    if !candidate_exists {
        if next_exists || relay_exists {
            return Err(TransportError::Codec(
                "replacement journal is missing its candidate record".into(),
            ));
        }
        let durable_promotion = if promotion_exists {
            Some(read_replacement_promotion(&paths.replacement_promotion)?)
        } else {
            None
        };
        if submission_exists {
            let promotion = durable_promotion.ok_or_else(|| {
                TransportError::Codec(
                    "replacement submission residue is missing its promotion receipt".into(),
                )
            })?;
            let submission = read_replacement_submission(&paths.replacement_submission)?;
            let intent = validate_durable_replacement_submission(&active, &submission)?;
            if promotion.candidate_manifest_hash != active_hash
                || intent.intent_hash().map_err(codec_error)? != promotion.intent_hash
            {
                return Err(TransportError::Codec(
                    "replacement submission residue conflicts with committed promotion".into(),
                ));
            }
            remove_file_if_exists(&paths.replacement_submission)?;
            File::open(&paths.root)?.sync_all()?;
            submission_exists = false;
        }
        if submission_exists {
            return Err(TransportError::Codec(
                "replacement submission residue could not be reconciled".into(),
            ));
        }
        if durable_promotion
            .is_some_and(|promotion| promotion.candidate_manifest_hash != active_hash)
        {
            return Err(TransportError::Codec(
                "replacement promotion receipt does not target the active manifest".into(),
            ));
        }
        return Ok(());
    }

    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    let candidate_hash = candidate
        .manifest
        .authorization_hash()
        .map_err(codec_error)?;
    let submission_authorization = if submission_exists {
        Some(replacement_authorization(
            &candidate,
            &read_replacement_submission(&paths.replacement_submission)?,
        )?)
    } else {
        None
    };
    if relay_exists {
        if !submission_exists {
            return Err(TransportError::Codec(
                "replacement relay is missing its durable submission".into(),
            ));
        }
        let submission = read_replacement_submission(&paths.replacement_submission)?;
        let relay = read_replacement_relay(&paths.replacement_relay)?;
        if relay.submission_hash != submission.submission_hash()? {
            return Err(TransportError::Codec(
                "replacement relay targets another durable submission".into(),
            ));
        }
    }
    let durable_promotion = if promotion_exists {
        Some(read_replacement_promotion(&paths.replacement_promotion)?)
    } else {
        None
    };
    if active_hash == candidate.predecessor_manifest_hash {
        validate_replacement_candidate_state(&candidate, &active, node_host)?;
        if let Some(promotion) = durable_promotion {
            let prior_active_receipt = promotion.candidate_manifest_hash == active_hash;
            if (!prior_active_receipt || next_exists) && submission_authorization != Some(promotion)
            {
                return Err(TransportError::Codec(
                    "replacement promotion receipt conflicts with staged authorization".into(),
                ));
            }
        }
        if next_exists {
            if !submission_exists || durable_promotion != submission_authorization {
                return Err(TransportError::Codec(
                    "next replacement manifest exists without exact durable promotion state".into(),
                ));
            }
            let expected = candidate.manifest.encode_canonical().map_err(codec_error)?;
            if read_owned_bounded_file(
                &paths.next_manifest,
                MAX_INITIALIZATION_MANIFEST_BYTES,
                "next replacement manifest",
            )? != expected
            {
                return Err(TransportError::Codec(
                    "next replacement manifest conflicts with the staged candidate".into(),
                ));
            }
        }
        return Ok(());
    }
    if active_hash == candidate_hash && active == candidate.manifest {
        if active.node_host_noise_x25519 != node_host.public() {
            return Err(TransportError::Codec(
                "promoted manifest does not match the persistent NodeHost key".into(),
            ));
        }
        let promotion = durable_promotion.ok_or_else(|| {
            TransportError::Codec("promoted manifest is missing its authorization receipt".into())
        })?;
        if promotion.candidate_manifest_hash != active_hash
            || submission_authorization.is_some_and(|expected| expected != promotion)
        {
            return Err(TransportError::Codec(
                "promoted manifest authorization receipt is inconsistent".into(),
            ));
        }
        if next_exists {
            let expected = active.encode_canonical().map_err(codec_error)?;
            if read_owned_bounded_file(
                &paths.next_manifest,
                MAX_INITIALIZATION_MANIFEST_BYTES,
                "next replacement manifest",
            )? != expected
            {
                return Err(TransportError::Codec(
                    "post-promotion next manifest conflicts with committed state".into(),
                ));
            }
            remove_file_if_exists(&paths.next_manifest)?;
        }
        remove_file_if_exists(&paths.replacement_relay)?;
        File::open(&paths.root)?.sync_all()?;
        remove_file_if_exists(&paths.replacement_submission)?;
        File::open(&paths.root)?.sync_all()?;
        remove_file_if_exists(&paths.replacement_candidate)?;
        File::open(&paths.root)?.sync_all()?;
        return Ok(());
    }
    Err(TransportError::Codec(
        "replacement journal does not descend from or equal committed state".into(),
    ))
}
