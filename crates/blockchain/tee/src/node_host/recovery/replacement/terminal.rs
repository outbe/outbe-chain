use super::super::super::filesystem::remove_file_and_sync_directory;
use super::super::super::journal_records::node_host_codec_error;
use super::super::super::{
    codec_error, read_owned_bounded_file, read_replacement_promotion, read_replacement_submission,
    remove_file_if_exists, validate_durable_replacement_submission,
    validate_replacement_candidate_state, FinalizedReplacementAuthorizationV1, NodeHostPaths,
    ReplacementCandidateRecordV1, MAX_INITIALIZATION_MANIFEST_BYTES,
};
use super::journal::ReplacementPresence;
use super::{ReplacementAuthorizationState, ReplacementTerminalContext};
use crate::TransportError;
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;

pub(super) fn reconcile_without_replacement_candidate(
    paths: &NodeHostPaths,
    active: &EnclaveInitializationManifestV1,
    active_hash: B256,
    presence: &mut ReplacementPresence,
) -> Result<(), TransportError> {
    if presence.next_exists || presence.relay_exists {
        return Err(node_host_codec_error(
            "replacement journal is missing its candidate record",
        ));
    }
    let durable_promotion = if presence.promotion_exists {
        Some(read_replacement_promotion(&paths.replacement_promotion)?)
    } else {
        None
    };
    if presence.submission_exists {
        reconcile_committed_submission_residue(
            paths,
            active,
            active_hash,
            presence,
            durable_promotion,
        )?;
    }
    if presence.submission_exists {
        return Err(node_host_codec_error(
            "replacement submission residue could not be reconciled",
        ));
    }
    if durable_promotion.is_some_and(|promotion| promotion.candidate_manifest_hash != active_hash) {
        return Err(node_host_codec_error(
            "replacement promotion receipt does not target the active manifest",
        ));
    }
    Ok(())
}

fn reconcile_committed_submission_residue(
    paths: &NodeHostPaths,
    active: &EnclaveInitializationManifestV1,
    active_hash: B256,
    presence: &mut ReplacementPresence,
    durable_promotion: Option<FinalizedReplacementAuthorizationV1>,
) -> Result<(), TransportError> {
    let promotion = durable_promotion.ok_or_else(|| {
        node_host_codec_error("replacement submission residue is missing its promotion receipt")
    })?;
    let submission = read_replacement_submission(&paths.replacement_submission)?;
    let intent = validate_durable_replacement_submission(active, &submission)?;
    if promotion.candidate_manifest_hash != active_hash
        || intent.intent_hash().map_err(codec_error)? != promotion.intent_hash
    {
        return Err(node_host_codec_error(
            "replacement submission residue conflicts with committed promotion",
        ));
    }
    remove_file_and_sync_directory(&paths.replacement_submission, &paths.root)?;
    presence.submission_exists = false;
    Ok(())
}

pub(super) fn reconcile_staged_replacement(
    context: &ReplacementTerminalContext<'_>,
) -> Result<(), TransportError> {
    let paths = context.paths;
    let active = context.active;
    let candidate = &context.durable.candidate;
    let node_host = context.node_host;
    let authorization = &context.durable.authorization;
    let presence = context.presence;
    validate_replacement_candidate_state(candidate, active, node_host)?;
    validate_staged_promotion_receipt(authorization, presence)?;
    if presence.next_exists {
        validate_staged_next_manifest(paths, candidate, authorization, presence)?;
    }
    Ok(())
}

fn validate_staged_promotion_receipt(
    authorization: &ReplacementAuthorizationState,
    presence: &ReplacementPresence,
) -> Result<(), TransportError> {
    if let Some(promotion) = authorization.promotion {
        let prior_active_receipt = promotion.candidate_manifest_hash == authorization.active_hash;
        if (!prior_active_receipt || presence.next_exists)
            && authorization.submission != Some(promotion)
        {
            return Err(node_host_codec_error(
                "replacement promotion receipt conflicts with staged authorization",
            ));
        }
    }
    Ok(())
}

fn validate_staged_next_manifest(
    paths: &NodeHostPaths,
    candidate: &ReplacementCandidateRecordV1,
    authorization: &ReplacementAuthorizationState,
    presence: &ReplacementPresence,
) -> Result<(), TransportError> {
    if !presence.submission_exists || authorization.promotion != authorization.submission {
        return Err(node_host_codec_error(
            "next replacement manifest exists without exact durable promotion state",
        ));
    }
    let expected = candidate.manifest.encode_canonical().map_err(codec_error)?;
    if read_owned_bounded_file(
        &paths.next_manifest,
        MAX_INITIALIZATION_MANIFEST_BYTES,
        "next replacement manifest",
    )? != expected
    {
        return Err(node_host_codec_error(
            "next replacement manifest conflicts with the staged candidate",
        ));
    }
    Ok(())
}

pub(super) fn reconcile_promoted_replacement(
    context: &ReplacementTerminalContext<'_>,
) -> Result<(), TransportError> {
    let paths = context.paths;
    let active = context.active;
    let node_host = context.node_host;
    let authorization = &context.durable.authorization;
    let presence = context.presence;
    if active.node_host_noise_x25519 != node_host.public() {
        return Err(node_host_codec_error(
            "promoted manifest does not match the persistent NodeHost key",
        ));
    }
    let promotion = authorization
        .promotion
        .ok_or(crate::errors::NodeHostRecordError::PromotedManifestMissingAuthorizationReceipt)?;
    if promotion.candidate_manifest_hash != authorization.active_hash
        || authorization
            .submission
            .is_some_and(|expected| expected != promotion)
    {
        return Err(node_host_codec_error(
            "promoted manifest authorization receipt is inconsistent",
        ));
    }
    if presence.next_exists {
        let expected = active.encode_canonical().map_err(codec_error)?;
        if read_owned_bounded_file(
            &paths.next_manifest,
            MAX_INITIALIZATION_MANIFEST_BYTES,
            "next replacement manifest",
        )? != expected
        {
            return Err(node_host_codec_error(
                "post-promotion next manifest conflicts with committed state",
            ));
        }
        remove_file_if_exists(&paths.next_manifest)?;
    }
    remove_file_and_sync_directory(&paths.replacement_relay, &paths.root)?;
    remove_file_and_sync_directory(&paths.replacement_submission, &paths.root)?;
    remove_file_and_sync_directory(&paths.replacement_candidate, &paths.root)?;
    Ok(())
}
