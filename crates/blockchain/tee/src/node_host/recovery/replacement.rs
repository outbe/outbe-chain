mod journal;
mod terminal;

use self::journal::{
    reconcile_candidate_journal, reconcile_promotion_journal, reconcile_relay_journal,
    reconcile_submission_journal, ReplacementPresence,
};
use self::terminal::{
    reconcile_promoted_replacement, reconcile_staged_replacement,
    reconcile_without_replacement_candidate,
};
use super::super::replacement::read_bound_replacement_relay;
use super::super::{
    codec_error, read_manifest, read_replacement_candidate, read_replacement_promotion,
    read_replacement_submission, validate_durable_replacement_submission,
    FinalizedReplacementAuthorizationV1, NodeHostPaths, ReplacementCandidateRecordV1,
    ReplacementCandidateSubmissionV1,
};
use super::remove_torn_scratch;
use crate::{NodeHostNoiseKey, TransportError};
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;

pub(in super::super) fn replacement_authorization(
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

struct ReplacementAuthorizationState {
    active_hash: B256,
    submission: Option<FinalizedReplacementAuthorizationV1>,
    promotion: Option<FinalizedReplacementAuthorizationV1>,
}

struct DurableReplacementState {
    candidate: ReplacementCandidateRecordV1,
    candidate_hash: B256,
    authorization: ReplacementAuthorizationState,
}

struct ReplacementTerminalContext<'a> {
    paths: &'a NodeHostPaths,
    active: &'a EnclaveInitializationManifestV1,
    node_host: &'a NodeHostNoiseKey,
    presence: &'a ReplacementPresence,
    durable: &'a DurableReplacementState,
}

fn read_durable_replacement_state(
    paths: &NodeHostPaths,
    presence: &ReplacementPresence,
    active_hash: B256,
) -> Result<DurableReplacementState, TransportError> {
    let candidate = read_replacement_candidate(&paths.replacement_candidate)?;
    let candidate_hash = candidate
        .manifest
        .authorization_hash()
        .map_err(codec_error)?;
    let submission_authorization = if presence.submission_exists {
        Some(replacement_authorization(
            &candidate,
            &read_replacement_submission(&paths.replacement_submission)?,
        )?)
    } else {
        None
    };
    if presence.relay_exists {
        if !presence.submission_exists {
            return Err(TransportError::Codec(
                "replacement relay is missing its durable submission".into(),
            ));
        }
        read_bound_replacement_relay(paths)?;
    }
    let durable_promotion = if presence.promotion_exists {
        Some(read_replacement_promotion(&paths.replacement_promotion)?)
    } else {
        None
    };
    Ok(DurableReplacementState {
        candidate,
        candidate_hash,
        authorization: ReplacementAuthorizationState {
            active_hash,
            submission: submission_authorization,
            promotion: durable_promotion,
        },
    })
}

pub(in super::super) fn reconcile_replacement_state(
    paths: &NodeHostPaths,
    node_host: &NodeHostNoiseKey,
) -> Result<(), TransportError> {
    remove_torn_scratch(paths, &paths.replacement_write_scratch)?;
    let active = read_manifest(&paths.manifest)?;
    let mut presence = ReplacementPresence::read(paths)?;
    reconcile_candidate_journal(paths, &active, node_host, &mut presence)?;
    reconcile_submission_journal(paths, &mut presence)?;
    reconcile_relay_journal(paths, &mut presence)?;
    reconcile_promotion_journal(paths, &mut presence)?;

    let active_hash = active.authorization_hash().map_err(codec_error)?;
    if !presence.candidate_exists {
        return reconcile_without_replacement_candidate(paths, &active, active_hash, &mut presence);
    }

    let durable = read_durable_replacement_state(paths, &presence, active_hash)?;
    let terminal = ReplacementTerminalContext {
        paths,
        active: &active,
        node_host,
        presence: &presence,
        durable: &durable,
    };
    if active_hash == durable.candidate.predecessor_manifest_hash {
        return reconcile_staged_replacement(&terminal);
    }
    if active_hash == durable.candidate_hash && active == durable.candidate.manifest {
        return reconcile_promoted_replacement(&terminal);
    }
    Err(TransportError::Codec(
        "replacement journal does not descend from or equal committed state".into(),
    ))
}
